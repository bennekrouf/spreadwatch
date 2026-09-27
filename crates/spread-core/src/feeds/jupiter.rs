//! Jupiter, Solana's DEX aggregator. There is no public price stream, so the
//! quote API is polled for executable prices routed across all on-chain
//! pools: "buy with $100 of USDT" gives the ask and "sell those tokens back"
//! the bid. A fixed dollar size keeps assets comparable (1 BTC and 1 BONK
//! are not), and $100 is in the range of a manual trade.
//!
//! Quoted against USDT on Solana, so it lines up with the …/USDT books
//! without a USDC/USDT basis. Followed tokens are polled in turn.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::{mpsc, watch};

use crate::jupiter::{JupiterApi, REQUESTS_PER_POLL, TRADE_RESERVE, USDT_MINT, USDT_UNITS};
use crate::model::{ConnState, JupToken, MarketEvent, Quote, Venue};

/// USDT spent on the buy quote, in whole dollars.
pub const NOTIONAL_USDT: f64 = 100.0;

/// After a 429 the IP stays blocked for minutes, and retrying early seems to
/// extend it, so the pause doubles on each consecutive 429.
const RATE_LIMITED_PAUSE: Duration = Duration::from_secs(15);
const MAX_RATE_LIMITED_PAUSE: Duration = Duration::from_secs(120);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuoteResponse {
    out_amount: String,
}

async fn quote_out(
    api: &JupiterApi,
    client: &reqwest::Client,
    input: &str,
    output: &str,
    amount: u64,
) -> reqwest::Result<u64> {
    let q: QuoteResponse = api
        .get(client, "/swap/v1/quote")
        .query(&[
            ("inputMint", input),
            ("outputMint", output),
            ("amount", &amount.to_string()),
            ("slippageBps", "50"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    // A malformed amount reads as 0 and is rejected by `bid_ask`.
    Ok(q.out_amount.parse().unwrap_or(0))
}

/// (bid, ask) in USDT per token: `tokens` raw units bought for the notional,
/// `usdt_back` raw USDT units for selling them again.
fn bid_ask(tokens: u64, usdt_back: u64, decimals: u8) -> Option<(f64, f64)> {
    if tokens == 0 || usdt_back == 0 {
        return None;
    }
    let qty = tokens as f64 / 10f64.powi(i32::from(decimals));
    Some((usdt_back as f64 / USDT_UNITS / qty, NOTIONAL_USDT / qty))
}

/// Buy then sell back the same amount. Sequential: the sell size comes from
/// the buy, so no earlier price is needed.
async fn quote_pair(
    api: &JupiterApi,
    client: &reqwest::Client,
    token: &JupToken,
) -> reqwest::Result<Option<(f64, f64)>> {
    let notional = (NOTIONAL_USDT * USDT_UNITS) as u64;
    let tokens = quote_out(api, client, USDT_MINT, &token.mint, notional).await?;
    if tokens == 0 {
        return Ok(None);
    }
    let usdt_back = quote_out(api, client, &token.mint, USDT_MINT, tokens).await?;
    Ok(bid_ask(tokens, usdt_back, token.decimals))
}

/// Polls one followed token per tick, in turn, at the rate `api` allows the
/// feed. A poll that would eat into the trade reserve is skipped.
pub async fn run(
    api: Arc<JupiterApi>,
    tokens: watch::Receiver<Vec<(Arc<str>, JupToken)>>,
    tx: mpsc::Sender<MarketEvent>,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("http client");
    let mut state = ConnState::Connecting;
    let mut pause = RATE_LIMITED_PAUSE;
    let mut next = 0usize;
    let mut tick = tokio::time::interval(api.feed_poll_every());
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tick.tick().await;
        let Some((asset, token)) = ({
            let list = tokens.borrow();
            (!list.is_empty()).then(|| list[next % list.len()].clone())
        }) else {
            continue;
        };
        next = next.wrapping_add(1);
        if !api.try_take(REQUESTS_PER_POLL, TRADE_RESERVE) {
            continue;
        }

        let new_state = match quote_pair(&api, &client, &token).await {
            Ok(Some((bid, ask))) => {
                pause = RATE_LIMITED_PAUSE;
                // Polled: latency here would be HTTP round trips, not transit.
                let q = Quote {
                    venue: Venue::Jupiter,
                    bid,
                    ask,
                    recv_at: Instant::now(),
                    latency_ms: None,
                };
                if tx.send(MarketEvent::Quote(asset, q)).await.is_err() {
                    return;
                }
                ConnState::Connected
            }
            Ok(None) => {
                tracing::warn!("Jupiter: no usable quote for {asset}");
                state
            }
            // No route for this one token: its problem, not Jupiter's.
            Err(e)
                if e.status().is_some_and(|s| {
                    s.is_client_error() && s != reqwest::StatusCode::TOO_MANY_REQUESTS
                }) =>
            {
                tracing::warn!("Jupiter: {asset}: {e}");
                state
            }
            Err(e) => {
                tracing::warn!("Jupiter quote failed: {e}");
                if e.status() == Some(reqwest::StatusCode::TOO_MANY_REQUESTS) {
                    tracing::warn!("Jupiter rate limit: pausing {pause:?}");
                    tokio::time::sleep(pause).await;
                    pause = (pause * 2).min(MAX_RATE_LIMITED_PAUSE);
                }
                ConnState::Disconnected
            }
        };

        if new_state != state {
            state = new_state;
            if tx
                .send(MarketEvent::Status(Venue::Jupiter, state))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_from_a_round_trip_of_the_same_amount() {
        // $100 buys 0.8264 SOL (9 decimals); selling it back returns $99.98.
        let (bid, ask) = bid_ask(826_400_000, 99_980_000, 9).unwrap();
        assert!((ask - 121.007).abs() < 0.001, "ask {ask}");
        assert!((bid - 120.983).abs() < 0.001, "bid {bid}");
        assert!(bid_ask(0, 1, 9).is_none());
    }
}
