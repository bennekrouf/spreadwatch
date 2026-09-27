use std::time::Duration;

use serde::Deserialize;

use super::{Feed, Tob};
use crate::model::Venue;

/// Tick-by-tick best bid/offer: pushed within 10 ms of a change, both sides
/// every time. OKX closes the connection after 30s without traffic and wants
/// a plain-text `ping` (answered with `pong`, not JSON). The only timestamp
/// is the book update time, so its latency includes OKX's own push delay.
#[derive(Default)]
pub struct Okx;

#[derive(Deserialize)]
struct Envelope {
    arg: Option<Arg>,
    data: Option<Vec<Bbo>>,
}

#[derive(Deserialize)]
struct Arg {
    /// Market, e.g. "SOL-USDT". The data itself doesn't repeat it.
    #[serde(rename = "instId")]
    inst_id: String,
}

#[derive(Deserialize)]
struct Bbo {
    /// Levels are [price, size, deprecated, order count].
    bids: Vec<Vec<String>>,
    asks: Vec<Vec<String>>,
    /// Order book update time, Unix ms as a string.
    ts: Option<String>,
}

impl Feed for Okx {
    fn venue(&self) -> Venue {
        Venue::Okx
    }

    fn url(&self) -> &'static str {
        "wss://ws.okx.com:8443/ws/v5/public"
    }

    fn market(&self, base: &str) -> String {
        format!("{base}-USDT")
    }

    fn subscribe_msg(&self, market: &str) -> String {
        format!(r#"{{"op":"subscribe","args":[{{"channel":"bbo-tbt","instId":"{market}"}}]}}"#)
    }

    fn ping_every(&self) -> Option<Duration> {
        Some(Duration::from_secs(20))
    }

    fn ping_msg(&self) -> String {
        "ping".into()
    }

    fn parse(&mut self, txt: &str) -> Option<(String, Tob)> {
        // `pong` is not JSON and the subscribe ack has no `data`; both fall out here.
        let env: Envelope = serde_json::from_str(txt).ok()?;
        let market = env.arg?.inst_id;
        let bbo = env.data?.into_iter().next()?;
        let bid = bbo.bids.first()?.first()?.parse().ok()?;
        let ask = bbo.asks.first()?.first()?.parse().ok()?;
        Some((
            market,
            Tob::new(bid, ask, bbo.ts.and_then(|t| t.parse().ok())),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bbo_and_skips_the_rest() {
        let msg = r#"{"arg":{"channel":"bbo-tbt","instId":"SOL-USDT"},"data":[{"asks":[["142.33","12.5","0","3"]],"bids":[["142.31","8","0","2"]],"ts":"1790000000000","seqId":1}]}"#;
        assert_eq!(
            Okx.parse(msg),
            Some((
                "SOL-USDT".into(),
                Tob::new(142.31, 142.33, Some(1790000000000))
            ))
        );

        assert_eq!(Okx.parse("pong"), None);
        assert_eq!(
            Okx.parse(r#"{"event":"subscribe","arg":{"channel":"bbo-tbt","instId":"SOL-USDT"},"connId":"a1"}"#),
            None
        );
    }
}
