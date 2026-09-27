//! One tokio task per venue: connect, subscribe, keep alive, parse, reconnect.
//! Venue specifics live behind the `Feed` trait. Jupiter has no stream and
//! runs its own polling loop in `jupiter.rs`.
//!
//! One connection per venue carries every followed asset. When the watchlist
//! changes, new markets are subscribed on the live connection; removed ones
//! are simply no longer routed.

mod binance;
mod bybit;
mod gate;
pub mod jupiter;
mod mexc;
mod okx;

pub use binance::Binance;
pub use bybit::Bybit;
pub use gate::Gate;
pub use mexc::Mexc;
pub use okx::Okx;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::model::{ConnState, MarketEvent, Quote, Venue};

/// Drop the connection if a venue sends nothing for this long. Catches
/// half-open sockets that never error out.
const STALL_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// A session that lasted this long counts as healthy and resets the backoff.
const HEALTHY_SESSION: Duration = Duration::from_secs(30);

/// Top of book as parsed from one message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tob {
    pub bid: f64,
    pub ask: f64,
    /// Exchange timestamp in Unix ms, the one closest to when the message
    /// left the exchange. Used only for the network latency diagnostic.
    pub ts_ms: Option<i64>,
}

impl Tob {
    pub fn new(bid: f64, ask: f64, ts_ms: Option<i64>) -> Self {
        Self { bid, ask, ts_ms }
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

pub trait Feed: Send + 'static {
    fn venue(&self) -> Venue;
    fn url(&self) -> &'static str;
    /// The venue's name for BASE/USDT exactly as its messages spell it,
    /// e.g. "SOLUSDT", "SOL-USDT", "SOL_USDT".
    fn market(&self, base: &str) -> String;
    /// Subscribe messages for these markets. By default one per market, so
    /// a symbol the venue doesn't list can't get the others rejected.
    fn subscribe_msgs(&self, markets: &[String]) -> Vec<String> {
        markets.iter().map(|m| self.subscribe_msg(m)).collect()
    }
    fn subscribe_msg(&self, market: &str) -> String;
    /// Period of the app-level ping some venues require on top of protocol pings.
    fn ping_every(&self) -> Option<Duration> {
        None
    }
    /// Built fresh for every ping, since some venues want a current timestamp.
    fn ping_msg(&self) -> String {
        String::new()
    }
    /// The market and its top of book, when a text message carries one.
    fn parse(&mut self, txt: &str) -> Option<(String, Tob)>;
    /// Same for binary messages, for venues that push protobuf.
    fn parse_binary(&mut self, _bytes: &[u8]) -> Option<(String, Tob)> {
        None
    }
    /// Forget book state before reconnecting.
    fn reset(&mut self) {}
}

/// Runs until the market goes away. `symbols` is the watchlist, base symbols.
pub async fn run<F: Feed>(
    mut feed: F,
    mut symbols: watch::Receiver<Vec<Arc<str>>>,
    tx: mpsc::Sender<MarketEvent>,
) {
    let venue = feed.venue();
    let mut backoff = Duration::from_secs(1);
    loop {
        // Idle connections get dropped by several venues; wait for something to follow.
        while symbols.borrow_and_update().is_empty() {
            if symbols.changed().await.is_err() {
                return;
            }
        }
        let _ = tx
            .send(MarketEvent::Status(venue, ConnState::Connecting))
            .await;
        let started = Instant::now();
        match session(&mut feed, &mut symbols, &tx).await {
            Ok(()) => tracing::warn!("{} closed the connection", venue.name()),
            Err(e) => tracing::warn!("{} feed error: {e:#}", venue.name()),
        }
        if tx.is_closed() {
            return;
        }
        let _ = tx
            .send(MarketEvent::Status(venue, ConnState::Disconnected))
            .await;
        feed.reset();
        if started.elapsed() > HEALTHY_SESSION {
            backoff = Duration::from_secs(1);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn session<F: Feed>(
    feed: &mut F,
    symbols: &mut watch::Receiver<Vec<Arc<str>>>,
    tx: &mpsc::Sender<MarketEvent>,
) -> Result<()> {
    let venue = feed.venue();
    let (ws, _) = connect_async(feed.url()).await?;
    let (mut write, mut read) = ws.split();

    // Venue market name → our symbol, for everything currently followed.
    let mut routes: HashMap<String, Arc<str>> = HashMap::new();
    let current = symbols.borrow_and_update().clone();
    for msg in subscribe_new(feed, &mut routes, &current) {
        write.send(Message::text(msg)).await?;
    }
    tx.send(MarketEvent::Status(venue, ConnState::Connected))
        .await?;

    let mut keepalive = feed
        .ping_every()
        .map(|period| tokio::time::interval_at(tokio::time::Instant::now() + period, period));

    loop {
        let ping_due = async {
            match keepalive.as_mut() {
                Some(interval) => {
                    interval.tick().await;
                }
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            msg = tokio::time::timeout(STALL_TIMEOUT, read.next()) => {
                // Protocol pings are answered by tungstenite itself.
                match msg {
                    Err(_) => return Err(anyhow!("no data for {STALL_TIMEOUT:?}")),
                    Ok(None) | Ok(Some(Ok(Message::Close(_)))) => return Ok(()),
                    Ok(Some(Err(e))) => return Err(e.into()),
                    Ok(Some(Ok(msg))) => {
                        let parsed = match &msg {
                            Message::Text(txt) => feed.parse(txt.as_str()),
                            Message::Binary(bytes) => feed.parse_binary(bytes),
                            _ => None,
                        };
                        let Some((market, t)) = parsed else { continue };
                        // Removed from the watchlist but still subscribed: drop it.
                        let Some(asset) = routes.get(&market) else { continue };
                        let quote = Quote {
                            venue,
                            bid: t.bid,
                            ask: t.ask,
                            recv_at: Instant::now(),
                            latency_ms: t.ts_ms.map(|ts| (unix_ms() - ts) as f64),
                        };
                        tx.send(MarketEvent::Quote(asset.clone(), quote)).await?;
                    }
                }
            }
            _ = ping_due => {
                write.send(Message::text(feed.ping_msg())).await?;
            }
            changed = symbols.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                let current = symbols.borrow_and_update().clone();
                let wanted: HashMap<String, Arc<str>> =
                    current.iter().map(|s| (feed.market(s), s.clone())).collect();
                routes.retain(|m, _| wanted.contains_key(m));
                for msg in subscribe_new(feed, &mut routes, &current) {
                    write.send(Message::text(msg)).await?;
                }
            }
        }
    }
}

/// Adds the symbols not yet routed and returns the messages that subscribe them.
fn subscribe_new<F: Feed>(
    feed: &F,
    routes: &mut HashMap<String, Arc<str>>,
    symbols: &[Arc<str>],
) -> Vec<String> {
    let mut new = Vec::new();
    for s in symbols {
        let market = feed.market(s);
        if !routes.contains_key(&market) {
            routes.insert(market.clone(), s.clone());
            new.push(market);
        }
    }
    if new.is_empty() {
        Vec::new()
    } else {
        feed.subscribe_msgs(&new)
    }
}
