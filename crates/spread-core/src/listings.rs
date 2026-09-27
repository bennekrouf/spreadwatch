//! Watches Gate for watchlist assets it doesn't trade yet.
//!
//! Gate's public REST API shows a new coin, and then its USDT pair, before
//! trading opens, often before the announcement. Polling the two per-asset
//! endpoints catches those steps. Assets Gate already trades are not polled.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::model::GateListing;

const GATE_API: &str = "https://api.gateio.ws/api/v4";
/// Two requests per unlisted asset per round, far under Gate's public limit.
pub const POLL_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
pub struct Currency {
    pub delisted: bool,
    pub deposit_disabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct Pair {
    /// "tradable", "buyable", "sellable" or "untradable".
    pub trade_status: String,
    /// Unix seconds; in the future when a listing is scheduled.
    pub buy_start: i64,
}

#[derive(Deserialize)]
struct ApiError {
    label: String,
}

pub fn classify(currency: Option<&Currency>, pair: Option<&Pair>, now_secs: i64) -> GateListing {
    match (currency, pair) {
        (None, _) => GateListing::NotListed,
        (Some(c), _) if c.delisted => GateListing::Delisted,
        (Some(c), None) => GateListing::CoinAdded {
            deposits: !c.deposit_disabled,
        },
        (Some(_), Some(p)) if p.buy_start > now_secs => GateListing::PairPending {
            opens_at: Some(p.buy_start),
        },
        (Some(_), Some(p)) if matches!(p.trade_status.as_str(), "tradable" | "buyable") => {
            GateListing::Trading
        }
        (Some(_), Some(_)) => GateListing::PairPending { opens_at: None },
    }
}

/// Where Gate stands on `symbol` against USDT.
pub async fn check(client: &reqwest::Client, symbol: &str) -> Result<GateListing> {
    let Some(currency) =
        get_opt::<Currency>(client, &format!("{GATE_API}/spot/currencies/{symbol}")).await?
    else {
        return Ok(GateListing::NotListed);
    };
    let pair = get_opt::<Pair>(
        client,
        &format!("{GATE_API}/spot/currency_pairs/{symbol}_USDT"),
    )
    .await?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    Ok(classify(Some(&currency), pair.as_ref(), now))
}

/// `None` when Gate answers that the currency or pair doesn't exist, e.g.
/// 400 `{"label":"INVALID_CURRENCY"}`; any other failure is an error.
async fn get_opt<T: DeserializeOwned>(client: &reqwest::Client, url: &str) -> Result<Option<T>> {
    let resp = client.get(url).send().await?;
    let status = resp.status();
    if status.is_client_error() {
        let body = resp.text().await.unwrap_or_default();
        if serde_json::from_str::<ApiError>(&body).is_ok_and(|e| e.label.starts_with("INVALID_")) {
            return Ok(None);
        }
        anyhow::bail!("{url}: {status} {body}");
    }
    Ok(Some(resp.error_for_status()?.json().await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn currency(delisted: bool, deposit_disabled: bool) -> Currency {
        Currency {
            delisted,
            deposit_disabled,
        }
    }

    fn pair(status: &str, buy_start: i64) -> Pair {
        Pair {
            trade_status: status.into(),
            buy_start,
        }
    }

    #[test]
    fn classifies_each_listing_step() {
        let now = 1_000;
        assert_eq!(classify(None, None, now), GateListing::NotListed);
        assert_eq!(
            classify(Some(&currency(true, true)), None, now),
            GateListing::Delisted
        );
        assert_eq!(
            classify(Some(&currency(false, false)), None, now),
            GateListing::CoinAdded { deposits: true }
        );
        assert_eq!(
            classify(Some(&currency(false, true)), None, now),
            GateListing::CoinAdded { deposits: false }
        );
        // Scheduled: Gate shows the pair before buying opens, whatever its status says.
        assert_eq!(
            classify(
                Some(&currency(false, false)),
                Some(&pair("sellable", 2_000)),
                now
            ),
            GateListing::PairPending {
                opens_at: Some(2_000)
            }
        );
        assert_eq!(
            classify(
                Some(&currency(false, false)),
                Some(&pair("untradable", 500)),
                now
            ),
            GateListing::PairPending { opens_at: None }
        );
        assert_eq!(
            classify(
                Some(&currency(false, false)),
                Some(&pair("tradable", 500)),
                now
            ),
            GateListing::Trading
        );
    }

    #[test]
    fn parses_gate_responses() {
        // Trimmed from real responses.
        let c: Currency = serde_json::from_str(
            r#"{"currency":"SOL","name":"Solana","delisted":false,"withdraw_disabled":false,"deposit_disabled":false,"trade_disabled":false,"chains":[]}"#,
        )
        .unwrap();
        assert!(!c.delisted && !c.deposit_disabled);
        let p: Pair = serde_json::from_str(
            r#"{"id":"BLKHION_USDT","base":"BLKHION","quote":"USDT","trade_status":"sellable","sell_start":1577808000,"buy_start":1790598600}"#,
        )
        .unwrap();
        assert_eq!(
            (p.trade_status.as_str(), p.buy_start),
            ("sellable", 1_790_598_600)
        );
        let e: ApiError = serde_json::from_str(
            r#"{"label":"INVALID_CURRENCY","message":"Invalid currency ZZZ"}"#,
        )
        .unwrap();
        assert_eq!(e.label, "INVALID_CURRENCY");
    }

    #[tokio::test]
    #[ignore = "hits the live Gate API"]
    async fn live_check() {
        let client = reqwest::Client::new();
        assert_eq!(check(&client, "SOL").await.unwrap(), GateListing::Trading);
        assert_eq!(
            check(&client, "ZZZNOPE123").await.unwrap(),
            GateListing::NotListed
        );
    }
}
