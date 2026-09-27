use std::time::Duration;

use prost::Message as _;

use super::{Feed, Tob};
use crate::model::Venue;

/// Spot aggregated book ticker. MEXC's spot streams are protobuf only: acks
/// and pongs arrive as JSON text, market data as binary frames. The server
/// drops connections that go 60s without a client PING.
///
/// The `@10ms` interval matters here: at `@100ms` MEXC would batch updates
/// and look up to 100 ms late in the lag measurement for reasons of its own.
#[derive(Default)]
pub struct Mexc;

/// Subset of `PushDataV3ApiWrapper` from github.com/mexcdevelop/websocket-proto.
/// The `body` oneof is declared as a plain optional field, which is the same
/// on the wire; prost skips every field not listed.
#[derive(Clone, PartialEq, prost::Message)]
struct PushDataV3ApiWrapper {
    #[prost(string, tag = "1")]
    channel: String,
    /// Market, e.g. "SOLUSDT".
    #[prost(string, optional, tag = "3")]
    symbol: Option<String>,
    #[prost(message, optional, tag = "315")]
    public_aggre_book_ticker: Option<PublicAggreBookTickerV3Api>,
    /// Push time, Unix ms.
    #[prost(int64, optional, tag = "6")]
    send_time: Option<i64>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct PublicAggreBookTickerV3Api {
    #[prost(string, tag = "1")]
    bid_price: String,
    #[prost(string, tag = "3")]
    ask_price: String,
}

impl Feed for Mexc {
    fn venue(&self) -> Venue {
        Venue::Mexc
    }

    fn url(&self) -> &'static str {
        "wss://wbs-api.mexc.com/ws"
    }

    fn market(&self, base: &str) -> String {
        format!("{base}USDT")
    }

    fn subscribe_msg(&self, market: &str) -> String {
        format!(
            r#"{{"method":"SUBSCRIPTION","params":["spot@public.aggre.bookTicker.v3.api.pb@10ms@{market}"]}}"#
        )
    }

    fn ping_every(&self) -> Option<Duration> {
        Some(Duration::from_secs(20))
    }

    fn ping_msg(&self) -> String {
        r#"{"method":"PING"}"#.into()
    }

    fn parse(&mut self, _txt: &str) -> Option<(String, Tob)> {
        None
    }

    fn parse_binary(&mut self, bytes: &[u8]) -> Option<(String, Tob)> {
        let wrapper = PushDataV3ApiWrapper::decode(bytes).ok()?;
        let t = wrapper.public_aggre_book_ticker?;
        // The channel ends with the market too, should `symbol` be missing.
        let market = wrapper
            .symbol
            .or_else(|| wrapper.channel.rsplit('@').next().map(str::to_owned))?;
        let tob = Tob::new(
            t.bid_price.parse().ok()?,
            t.ask_price.parse().ok()?,
            wrapper.send_time,
        );
        Some((market, tob))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_aggre_book_ticker() {
        let msg = PushDataV3ApiWrapper {
            channel: "spot@public.aggre.bookTicker.v3.api.pb@10ms@SOLUSDT".into(),
            symbol: None,
            public_aggre_book_ticker: Some(PublicAggreBookTickerV3Api {
                bid_price: "142.31".into(),
                ask_price: "142.32".into(),
            }),
            send_time: Some(1_790_000_000_000),
        };
        assert_eq!(
            Mexc.parse_binary(&msg.encode_to_vec()),
            Some((
                "SOLUSDT".into(),
                Tob::new(142.31, 142.32, Some(1_790_000_000_000))
            ))
        );
    }

    #[test]
    fn ignores_other_channels() {
        let msg = PushDataV3ApiWrapper {
            channel: "spot@public.deals".into(),
            symbol: Some("SOLUSDT".into()),
            public_aggre_book_ticker: None,
            send_time: None,
        };
        assert_eq!(Mexc.parse_binary(&msg.encode_to_vec()), None);
    }
}
