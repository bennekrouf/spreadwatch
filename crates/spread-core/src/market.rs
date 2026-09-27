//! The watchlist and one engine per followed asset.
//!
//! Feeds send quotes tagged with their asset; this routes them, runs the UI's
//! add/remove/select commands, and publishes a summary line per asset plus
//! the full snapshot of the selected one.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{mpsc, watch};

use crate::engine::{Engine, EngineConfig};
use crate::feeds;
use crate::jupiter::{JupiterApi, SOL_MINT};
use crate::listings;
use crate::model::{
    Asset, AssetSummary, ConnState, FeedEvent, GateListing, JupToken, MarketEvent, MarketSnapshot,
    Venue,
};

#[derive(Debug, Clone)]
pub enum MarketCmd {
    Add(String),
    Remove(String),
    Select(String),
}

/// Jupiter tokens below this liquidity are too thin to price anything.
const MIN_JUPITER_LIQUIDITY_USD: f64 = 100_000.0;
const PUBLISH_EVERY: Duration = Duration::from_millis(50);

fn watchlist_path() -> PathBuf {
    crate::trade::config_dir().join("watchlist.json")
}

fn default_watchlist() -> Vec<Asset> {
    vec![Asset {
        symbol: "SOL".into(),
        jupiter: Some(JupToken {
            mint: SOL_MINT.into(),
            decimals: 9,
            name: "Wrapped SOL".into(),
        }),
    }]
}

fn load_watchlist() -> Vec<Asset> {
    std::fs::read_to_string(watchlist_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(default_watchlist)
}

fn save_watchlist(assets: &[Asset]) -> std::io::Result<()> {
    std::fs::create_dir_all(crate::trade::config_dir())?;
    std::fs::write(watchlist_path(), serde_json::to_string_pretty(assets)?)
}

/// "sol", " $WIF " → "SOL", "WIF". `Err` explains what is wrong.
pub fn normalize_symbol(input: &str) -> Result<String, String> {
    let s = input.trim().trim_start_matches('$').to_uppercase();
    if s.is_empty() {
        return Err("Enter a symbol, e.g. BTC".into());
    }
    if s.len() > 15 || !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("\"{}\" is not a ticker symbol", input.trim()));
    }
    if s == "USDT" {
        return Err("USDT is the quote currency; every asset is priced in it".into());
    }
    Ok(s)
}

/// Picks the Solana token Jupiter should price for `symbol` from a token
/// search: same symbol (ignoring case and a leading `$`), verified, liquid,
/// and the most liquid of those. Unverified lookalikes are common.
pub fn pick_token(symbol: &str, results: &[Value]) -> Option<JupToken> {
    results
        .iter()
        .filter(|t| t["isVerified"].as_bool() == Some(true))
        .filter(|t| {
            t["symbol"]
                .as_str()
                .is_some_and(|s| s.trim_start_matches('$').eq_ignore_ascii_case(symbol))
        })
        .filter(|t| t["liquidity"].as_f64().unwrap_or(0.0) >= MIN_JUPITER_LIQUIDITY_USD)
        .max_by(|a, b| {
            a["liquidity"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&b["liquidity"].as_f64().unwrap_or(0.0))
        })
        .and_then(|t| {
            Some(JupToken {
                mint: t["id"].as_str()?.to_owned(),
                decimals: u8::try_from(t["decimals"].as_u64()?).ok()?,
                name: t["name"].as_str().unwrap_or_default().to_owned(),
            })
        })
}

async fn lookup_token(
    api: &JupiterApi,
    client: &reqwest::Client,
    symbol: &str,
) -> anyhow::Result<Option<JupToken>> {
    api.take(1).await;
    let results: Vec<Value> = api
        .get(client, "/tokens/v2/search")
        .query(&[("query", symbol)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(pick_token(symbol, &results))
}

struct Tracked {
    asset: Asset,
    symbol: Arc<str>,
    engine: Engine,
    /// Last answer from Gate's listing endpoints.
    gate: Option<GateListing>,
}

pub async fn run(
    cfg: EngineConfig,
    jupiter: Arc<JupiterApi>,
    mut cmds: mpsc::Receiver<MarketCmd>,
    out: watch::Sender<MarketSnapshot>,
) {
    let (tx, mut events) = mpsc::channel::<MarketEvent>(4096);
    let (symbols_tx, symbols_rx) = watch::channel(Vec::<Arc<str>>::new());
    let (tokens_tx, tokens_rx) = watch::channel(Vec::<(Arc<str>, JupToken)>::new());
    tokio::spawn(feeds::run(feeds::Binance, symbols_rx.clone(), tx.clone()));
    tokio::spawn(feeds::run(
        feeds::Bybit::default(),
        symbols_rx.clone(),
        tx.clone(),
    ));
    tokio::spawn(feeds::run(feeds::Mexc, symbols_rx.clone(), tx.clone()));
    tokio::spawn(feeds::run(feeds::Gate, symbols_rx.clone(), tx.clone()));
    tokio::spawn(feeds::run(feeds::Okx, symbols_rx, tx.clone()));
    tokio::spawn(feeds::jupiter::run(jupiter.clone(), tokens_rx, tx));

    let mut market = Market {
        cfg,
        jupiter: jupiter.clone(),
        tracked: Vec::new(),
        conn: [ConnState::Connecting; Venue::COUNT],
        selected: None,
        adding: None,
        notice: None,
        gate_checking: HashSet::new(),
        symbols_tx,
        tokens_tx,
    };
    for asset in load_watchlist() {
        market.track(asset);
    }
    market.selected = market.tracked.first().map(|t| t.symbol.clone());
    market.republish_lists();

    let (resolved_tx, mut resolved) =
        mpsc::channel::<(String, anyhow::Result<Option<JupToken>>)>(8);
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client");
    let mut publish = tokio::time::interval(PUBLISH_EVERY);
    publish.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (gate_tx, mut gate_results) = mpsc::channel::<(Arc<str>, anyhow::Result<GateListing>)>(64);
    let mut gate_poll = tokio::time::interval(listings::POLL_EVERY);
    gate_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Some(ev) => market.route(ev),
                None => return,
            },
            cmd = cmds.recv() => match cmd {
                Some(MarketCmd::Add(input)) => {
                    if let Some(symbol) = market.begin_add(&input) {
                        // The lookup may wait on the Jupiter budget; keep quotes flowing meanwhile.
                        let (api, http, done) = (jupiter.clone(), http.clone(), resolved_tx.clone());
                        tokio::spawn(async move {
                            let token = lookup_token(&api, &http, &symbol).await;
                            let _ = done.send((symbol, token)).await;
                        });
                    }
                }
                Some(MarketCmd::Remove(symbol)) => market.remove(&symbol),
                Some(MarketCmd::Select(symbol)) => {
                    if market.tracked.iter().any(|t| *t.symbol == *symbol) {
                        market.selected = Some(symbol.into());
                        market.notice = None;
                    }
                }
                None => return,
            },
            Some((symbol, token)) = resolved.recv() => {
                market.finish_add(symbol, token);
                // Check the new asset now rather than at the next round.
                market.check_gate(&http, &gate_tx);
            }
            _ = gate_poll.tick() => market.check_gate(&http, &gate_tx),
            Some((symbol, result)) = gate_results.recv() => market.on_gate(symbol, result),
            _ = publish.tick() => {
                out.send_replace(market.snapshot(Instant::now()));
            }
        }
    }
}

struct Market {
    cfg: EngineConfig,
    jupiter: Arc<JupiterApi>,
    tracked: Vec<Tracked>,
    /// Current connection state per venue, to seed engines added later.
    conn: [ConnState; Venue::COUNT],
    selected: Option<Arc<str>>,
    adding: Option<String>,
    notice: Option<String>,
    /// Assets with a Gate listing check in flight.
    gate_checking: HashSet<Arc<str>>,
    symbols_tx: watch::Sender<Vec<Arc<str>>>,
    tokens_tx: watch::Sender<Vec<(Arc<str>, JupToken)>>,
}

impl Market {
    fn track(&mut self, asset: Asset) {
        let mut engine = Engine::new(self.cfg);
        for venue in Venue::ALL {
            engine.on_event(FeedEvent::Status(venue, self.conn[venue.idx()]));
        }
        engine.set_absent(Venue::Jupiter, asset.jupiter.is_none());
        self.tracked.push(Tracked {
            symbol: asset.symbol.as_str().into(),
            asset,
            engine,
            gate: None,
        });
    }

    /// Tells the feeds what to follow, and each engine how often Jupiter
    /// comes back to it now that the tokens share the polls.
    fn republish_lists(&mut self) {
        self.symbols_tx
            .send_replace(self.tracked.iter().map(|t| t.symbol.clone()).collect());
        let tokens: Vec<(Arc<str>, JupToken)> = self
            .tracked
            .iter()
            .filter_map(|t| Some((t.symbol.clone(), t.asset.jupiter.clone()?)))
            .collect();
        let per_token = self.jupiter.feed_poll_every() * tokens.len().max(1) as u32;
        for t in &mut self.tracked {
            t.engine.set_poll_every(per_token);
        }
        self.tokens_tx.send_replace(tokens);
    }

    fn save(&mut self) {
        let assets: Vec<Asset> = self.tracked.iter().map(|t| t.asset.clone()).collect();
        if let Err(e) = save_watchlist(&assets) {
            self.notice = Some(format!("could not save the watchlist: {e}"));
        }
    }

    fn route(&mut self, ev: MarketEvent) {
        match ev {
            MarketEvent::Quote(symbol, quote) => {
                if let Some(t) = self.tracked.iter_mut().find(|t| t.symbol == symbol) {
                    t.engine.on_event(FeedEvent::Quote(quote));
                }
            }
            MarketEvent::Status(venue, conn) => {
                self.conn[venue.idx()] = conn;
                for t in &mut self.tracked {
                    t.engine.on_event(FeedEvent::Status(venue, conn));
                }
            }
        }
    }

    /// Validates the input; returns the symbol to look up on Jupiter.
    fn begin_add(&mut self, input: &str) -> Option<String> {
        if self.adding.is_some() {
            self.notice = Some("Still adding the previous symbol".into());
            return None;
        }
        let symbol = match normalize_symbol(input) {
            Ok(s) => s,
            Err(e) => {
                self.notice = Some(e);
                return None;
            }
        };
        if self.tracked.iter().any(|t| *t.symbol == *symbol) {
            self.notice = Some(format!("{symbol} is already in the list"));
            self.selected = Some(symbol.into());
            return None;
        }
        self.notice = None;
        self.adding = Some(symbol.clone());
        Some(symbol)
    }

    fn finish_add(&mut self, symbol: String, token: anyhow::Result<Option<JupToken>>) {
        self.adding = None;
        self.notice = None;
        let jupiter = match token {
            Ok(t) => t,
            Err(e) => {
                self.notice = Some(format!(
                    "{symbol} added without Jupiter: token lookup failed ({e:#})"
                ));
                None
            }
        };
        let label = jupiter.as_ref().map_or("no Solana token".to_owned(), |t| {
            format!("Jupiter: {}", t.name)
        });
        tracing::info!("watching {symbol} ({label})");
        self.track(Asset {
            symbol: symbol.clone(),
            jupiter,
        });
        self.selected = Some(symbol.into());
        self.republish_lists();
        self.save();
    }

    /// Starts a Gate check for every asset Gate doesn't trade yet, or hasn't
    /// been asked about, unless one is already running for it.
    fn check_gate(
        &mut self,
        http: &reqwest::Client,
        done: &mpsc::Sender<(Arc<str>, anyhow::Result<GateListing>)>,
    ) {
        for t in &self.tracked {
            if t.gate == Some(GateListing::Trading) || !self.gate_checking.insert(t.symbol.clone())
            {
                continue;
            }
            let (http, done, symbol) = (http.clone(), done.clone(), t.symbol.clone());
            tokio::spawn(async move {
                let result = listings::check(&http, &symbol).await;
                let _ = done.send((symbol, result)).await;
            });
        }
    }

    /// Logs the first answer when Gate doesn't trade the asset yet, then every change.
    fn on_gate(&mut self, symbol: Arc<str>, result: anyhow::Result<GateListing>) {
        self.gate_checking.remove(&symbol);
        // Removed from the watchlist while the check ran.
        let Some(t) = self.tracked.iter_mut().find(|t| t.symbol == symbol) else {
            return;
        };
        let now = match result {
            Ok(now) => now,
            Err(e) => {
                // Keep the last known state; the next round retries.
                tracing::warn!("Gate listing check for {symbol} failed: {e:#}");
                return;
            }
        };
        match t.gate {
            Some(before) if before != now => {
                t.engine.note_listing(format!(
                    "{symbol}: {} (was: {})",
                    now.describe(),
                    before.short()
                ));
            }
            None if now != GateListing::Trading => {
                let every = listings::POLL_EVERY.as_secs();
                t.engine.note_listing(format!(
                    "{symbol}: {}; checking every {every} s",
                    now.describe()
                ));
            }
            _ => {}
        }
        t.gate = Some(now);
    }

    fn remove(&mut self, symbol: &str) {
        self.notice = None;
        self.tracked.retain(|t| &*t.symbol != symbol);
        if self.selected.as_deref() == Some(symbol) {
            self.selected = self.tracked.first().map(|t| t.symbol.clone());
        }
        self.republish_lists();
        self.save();
    }

    fn snapshot(&mut self, now: Instant) -> MarketSnapshot {
        let mut detail = None;
        let assets = self
            .tracked
            .iter_mut()
            .map(|t| {
                t.engine.tick(now);
                if self.selected.as_ref() == Some(&t.symbol) {
                    detail = Some(t.engine.snapshot(now));
                }
                let (mid, best_gross_bps, fresh, listed) = t.engine.summary(now);
                AssetSummary {
                    symbol: t.asset.symbol.clone(),
                    mid,
                    best_gross_bps,
                    fresh,
                    listed,
                    jupiter: t.asset.jupiter.clone(),
                    gate: t.gate,
                }
            })
            .collect();
        MarketSnapshot {
            assets,
            selected: self.selected.as_deref().map(str::to_owned),
            detail: detail.unwrap_or_default(),
            adding: self.adding.clone(),
            notice: self.notice.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn symbols_are_normalized_and_validated() {
        assert_eq!(normalize_symbol(" sol "), Ok("SOL".into()));
        assert_eq!(normalize_symbol("$wif"), Ok("WIF".into()));
        assert!(normalize_symbol("").is_err());
        assert!(normalize_symbol("BTC/USDT").is_err());
        assert!(normalize_symbol("usdt").is_err());
    }

    #[test]
    fn picks_the_liquid_verified_exact_match() {
        // Shapes from a real search for "BONK" and "WIF".
        let results = vec![
            json!({"id": "bonkSOL", "symbol": "bonkSOL", "isVerified": true, "liquidity": 11_761_413.0, "decimals": 9, "name": "bonkSOL"}),
            json!({"id": "fake", "symbol": "BONK", "isVerified": null, "liquidity": 2_536_519.0, "decimals": 6, "name": "BONK"}),
            json!({"id": "DezX", "symbol": "Bonk", "isVerified": true, "liquidity": 5_535_757.0, "decimals": 5, "name": "Bonk"}),
            json!({"id": "dead", "symbol": "BONK", "isVerified": true, "liquidity": 10.0, "decimals": 5, "name": "Old Bonk"}),
        ];
        let t = pick_token("BONK", &results).unwrap();
        assert_eq!((t.mint.as_str(), t.decimals), ("DezX", 5));

        let wif = vec![
            json!({"id": "EKp", "symbol": "$WIF", "isVerified": true, "liquidity": 7_366_305.0, "decimals": 6, "name": "dogwifhat"}),
        ];
        assert_eq!(pick_token("WIF", &wif).unwrap().mint, "EKp");

        // Wrapped bitcoins are different tokens, not "BTC".
        let btc = vec![
            json!({"id": "3NZ", "symbol": "WBTC", "isVerified": true, "liquidity": 39_413_461.0, "decimals": 8, "name": "Wrapped BTC"}),
        ];
        assert!(pick_token("BTC", &btc).is_none());
    }
}
