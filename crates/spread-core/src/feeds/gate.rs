use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use super::{Feed, Tob};
use crate::model::Venue;

/// Spot book ticker, v4 API. Every client message carries the current time in
/// seconds, so the subscribe and ping messages are built when sent.
#[derive(Default)]
pub struct Gate;

#[derive(Deserialize)]
struct Envelope {
    /// Server push time, Unix ms.
    time_ms: Option<i64>,
    channel: Option<String>,
    event: Option<String>,
    result: Option<BookTicker>,
}

#[derive(Deserialize)]
struct BookTicker {
    /// Market, e.g. "SOL_USDT".
    s: String,
    b: String,
    a: String,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Feed for Gate {
    fn venue(&self) -> Venue {
        Venue::Gate
    }

    fn url(&self) -> &'static str {
        "wss://api.gateio.ws/ws/v4/"
    }

    fn market(&self, base: &str) -> String {
        format!("{base}_USDT")
    }

    fn subscribe_msg(&self, market: &str) -> String {
        format!(
            r#"{{"time":{},"channel":"spot.book_ticker","event":"subscribe","payload":["{market}"]}}"#,
            now_secs()
        )
    }

    fn ping_every(&self) -> Option<Duration> {
        Some(Duration::from_secs(20))
    }

    fn ping_msg(&self) -> String {
        format!(r#"{{"time":{},"channel":"spot.ping"}}"#, now_secs())
    }

    fn parse(&mut self, txt: &str) -> Option<(String, Tob)> {
        // The subscribe ack also has a `result`, but with event "subscribe".
        let env: Envelope = serde_json::from_str(txt).ok()?;
        if env.channel.as_deref() != Some("spot.book_ticker")
            || env.event.as_deref() != Some("update")
        {
            return None;
        }
        let t = env.result?;
        Some((
            t.s,
            Tob::new(t.b.parse().ok()?, t.a.parse().ok()?, env.time_ms),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_update_and_skips_ack() {
        let upd = r#"{"time":1,"time_ms":1000,"channel":"spot.book_ticker","event":"update","result":{"t":1000,"u":1,"s":"SOL_USDT","b":"142.31","B":"5","a":"142.33","A":"2"}}"#;
        assert_eq!(
            Gate.parse(upd),
            Some(("SOL_USDT".into(), Tob::new(142.31, 142.33, Some(1000))))
        );

        let ack = r#"{"time":1,"channel":"spot.book_ticker","event":"subscribe","result":{"status":"success"}}"#;
        assert_eq!(Gate.parse(ack), None);
        assert_eq!(
            Gate.parse(r#"{"time":1,"channel":"spot.pong","event":"","result":null}"#),
            None
        );
    }
}
