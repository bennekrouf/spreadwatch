use serde::Deserialize;

use super::{Feed, Tob};
use crate::model::Venue;

/// Spot book ticker: pushes best bid/ask on every change. The server pings
/// every 20s and closes connections after 24h; the reconnect loop handles
/// both. This stream carries no timestamp, so Binance has no latency figure.
#[derive(Default)]
pub struct Binance;

#[derive(Deserialize)]
struct BookTicker {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "b")]
    bid: String,
    #[serde(rename = "a")]
    ask: String,
}

impl Feed for Binance {
    fn venue(&self) -> Venue {
        Venue::Binance
    }

    fn url(&self) -> &'static str {
        "wss://stream.binance.com:9443/ws"
    }

    fn market(&self, base: &str) -> String {
        format!("{base}USDT")
    }

    /// Binance drops clients sending more than 5 messages a second, so
    /// everything goes in one request. Unknown streams are simply silent.
    fn subscribe_msgs(&self, markets: &[String]) -> Vec<String> {
        let params: Vec<String> = markets
            .iter()
            .map(|m| format!("{}@bookTicker", m.to_lowercase()))
            .collect();
        vec![serde_json::json!({ "method": "SUBSCRIBE", "params": params, "id": 1 }).to_string()]
    }

    fn subscribe_msg(&self, market: &str) -> String {
        self.subscribe_msgs(&[market.to_owned()]).remove(0)
    }

    fn parse(&mut self, txt: &str) -> Option<(String, Tob)> {
        // The subscribe reply `{"result":null,"id":1}` has no symbol and falls out here.
        let t: BookTicker = serde_json::from_str(txt).ok()?;
        Some((
            t.symbol,
            Tob::new(t.bid.parse().ok()?, t.ask.parse().ok()?, None),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_book_ticker() {
        let msg = r#"{"u":400900217,"s":"SOLUSDT","b":"142.31000000","B":"31.2","a":"142.32000000","A":"40.6"}"#;
        assert_eq!(
            Binance.parse(msg),
            Some(("SOLUSDT".into(), Tob::new(142.31, 142.32, None)))
        );
        assert_eq!(Binance.parse(r#"{"result":null,"id":1}"#), None);
    }

    #[test]
    fn subscribes_every_market_in_one_message() {
        let msgs = Binance.subscribe_msgs(&["SOLUSDT".into(), "BTCUSDT".into()]);
        assert_eq!(
            msgs,
            vec![
                r#"{"id":1,"method":"SUBSCRIBE","params":["solusdt@bookTicker","btcusdt@bookTicker"]}"#
            ]
        );
    }
}
