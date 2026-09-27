//! Quote and build a Jupiter swap for the hot wallet. Jupiter returns an
//! unsigned transaction with route, compute budget and priority fee set.

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use serde_json::{json, Value};

use super::Side;
use crate::jupiter::{JupiterApi, LAMPORTS_PER_SOL, SOL_MINT, USDT_MINT, USDT_UNITS};

pub struct SwapQuote {
    /// Passed back verbatim to the swap endpoint.
    raw: Value,
    /// USDT per SOL.
    pub price: f64,
    /// USDT the wallet pays (buy) or receives (sell) at the quoted price.
    pub usdt: f64,
}

pub struct UnsignedSwap {
    pub tx: Vec<u8>,
    /// The transaction can't land once the chain passes this height.
    pub last_valid_block_height: u64,
}

fn lamports(size_sol: f64) -> u64 {
    (size_sol * LAMPORTS_PER_SOL).round() as u64
}

/// Both sides are sized in SOL: selling spends exactly `size_sol`, buying
/// receives exactly `size_sol`.
///
/// Waits for room in the shared request budget rather than risk a 429.
pub async fn quote(
    api: &JupiterApi,
    client: &reqwest::Client,
    side: Side,
    size_sol: f64,
    slippage_bps: u16,
) -> Result<SwapQuote> {
    let (input, output, mode) = match side {
        Side::SellSol => (SOL_MINT, USDT_MINT, "ExactIn"),
        Side::BuySol => (USDT_MINT, SOL_MINT, "ExactOut"),
    };
    api.take(1).await;
    let raw: Value = api
        .get(client, "/swap/v1/quote")
        .query(&[
            ("inputMint", input),
            ("outputMint", output),
            ("amount", &lamports(size_sol).to_string()),
            ("swapMode", mode),
            ("slippageBps", &slippage_bps.to_string()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let amount = |key: &str| -> Result<f64> {
        raw[key]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or_else(|| anyhow!("quote has no {key}"))
    };
    let usdt = match side {
        Side::SellSol => amount("outAmount")?,
        Side::BuySol => amount("inAmount")?,
    } / USDT_UNITS;
    Ok(SwapQuote {
        price: usdt / size_sol,
        usdt,
        raw,
    })
}

pub async fn build(
    api: &JupiterApi,
    client: &reqwest::Client,
    quote: &SwapQuote,
    user: &str,
    max_priority_fee_lamports: u64,
) -> Result<UnsignedSwap> {
    let body = json!({
        "quoteResponse": quote.raw,
        "userPublicKey": user,
        "dynamicComputeUnitLimit": true,
        "prioritizationFeeLamports": {
            "priorityLevelWithMaxLamports": {
                "maxLamports": max_priority_fee_lamports,
                "priorityLevel": "high",
            }
        },
    });
    api.take(1).await;
    let resp: Value = api
        .post(client, "/swap/v1/swap")
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let b64 = resp["swapTransaction"]
        .as_str()
        .ok_or_else(|| anyhow!("swap response has no transaction"))?;
    let tx = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .context("swap transaction is not base64")?;
    let last_valid_block_height = resp["lastValidBlockHeight"]
        .as_u64()
        .ok_or_else(|| anyhow!("swap response has no lastValidBlockHeight"))?;
    Ok(UnsignedSwap {
        tx,
        last_valid_block_height,
    })
}
