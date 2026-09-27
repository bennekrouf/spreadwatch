use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use super::{Feed, Tob};
use crate::model::Venue;

/// Spot order book, depth 1. The first message per market is a snapshot;
/// deltas may carry only one side, and a size of "0" removes the level, so
/// the last known bid and ask are kept per market. Bybit drops the
/// connection unless it gets an app-level `{"op":"ping"}` every 20s.
#[derive(Default)]
pub struct Bybit {
    books: HashMap<String, (Option<f64>, Option<f64>)>,
}

#[derive(Deserialize)]
struct Envelope {
    topic: Option<String>,
    /// When Bybit generated the push, Unix ms.
    ts: Option<i64>,
    #[serde(rename = "type")]
    kind: Option<String>,
    data: Option<Book>,
}

#[derive(Deserialize)]
struct Book {
    #[serde(default)]
    b: Vec<[String; 2]>,
    #[serde(default)]
    a: Vec<[String; 2]>,
}

/// Best level of one side, or `Some(None)` when the level was removed.
fn top(levels: &[[String; 2]]) -> Option<Option<f64>> {
    let [price, size] = levels.first()?;
    if size.parse::<f64>().ok()? == 0.0 {
        return Some(None);
    }
    Some(Some(price.parse().ok()?))
}

impl Feed for Bybit {
    fn venue(&self) -> Venue {
        Venue::Bybit
    }

    fn url(&self) -> &'static str {
        "wss://stream.bybit.com/v5/public/spot"
    }

    fn market(&self, base: &str) -> String {
        format!("{base}USDT")
    }

    fn subscribe_msg(&self, market: &str) -> String {
        format!(r#"{{"op":"subscribe","args":["orderbook.1.{market}"]}}"#)
    }

    fn ping_every(&self) -> Option<Duration> {
        Some(Duration::from_secs(20))
    }

    fn ping_msg(&self) -> String {
        r#"{"op":"ping"}"#.into()
    }

    fn parse(&mut self, txt: &str) -> Option<(String, Tob)> {
        // Subscribe acks and pongs have no topic and are skipped here.
        let env: Envelope = serde_json::from_str(txt).ok()?;
        let market = env.topic?.strip_prefix("orderbook.1.")?.to_owned();
        let data = env.data?;

        let book = self.books.entry(market.clone()).or_default();
        if env.kind.as_deref() == Some("snapshot") {
            *book = (top(&data.b).flatten(), top(&data.a).flatten());
        } else {
            if let Some(bid) = top(&data.b) {
                book.0 = bid;
            }
            if let Some(ask) = top(&data.a) {
                book.1 = ask;
            }
        }
        Some((market, Tob::new(book.0?, book.1?, env.ts)))
    }

    fn reset(&mut self) {
        self.books.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tob(r: Option<(String, Tob)>) -> Option<(String, f64, f64)> {
        r.map(|(m, t)| (m, t.bid, t.ask))
    }

    #[test]
    fn keeps_a_book_per_market_across_one_sided_deltas() {
        let mut f = Bybit::default();
        let sol = r#"{"topic":"orderbook.1.SOLUSDT","type":"snapshot","ts":1,"data":{"s":"SOLUSDT","b":[["142.30","10"]],"a":[["142.32","5"]],"u":1,"seq":1}}"#;
        let btc = r#"{"topic":"orderbook.1.BTCUSDT","type":"snapshot","ts":1,"data":{"s":"BTCUSDT","b":[["60000.1","1"]],"a":[["60000.2","1"]],"u":1,"seq":1}}"#;
        assert_eq!(tob(f.parse(sol)), Some(("SOLUSDT".into(), 142.30, 142.32)));
        assert_eq!(
            tob(f.parse(btc)),
            Some(("BTCUSDT".into(), 60000.1, 60000.2))
        );

        let delta = r#"{"topic":"orderbook.1.SOLUSDT","type":"delta","ts":2,"data":{"s":"SOLUSDT","b":[],"a":[["142.31","2"]],"u":2,"seq":2}}"#;
        let (market, t) = f.parse(delta).unwrap();
        assert_eq!(
            (market.as_str(), t),
            ("SOLUSDT", Tob::new(142.30, 142.31, Some(2)))
        );
    }

    #[test]
    fn ignores_acks_and_pongs() {
        let mut f = Bybit::default();
        assert_eq!(
            f.parse(r#"{"success":true,"ret_msg":"pong","op":"ping"}"#),
            None
        );
        assert_eq!(
            f.parse(r#"{"success":false,"ret_msg":"Invalid symbol","op":"subscribe"}"#),
            None
        );
    }
}
