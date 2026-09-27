//! The handful of Solana JSON-RPC calls the trade flow needs, over reqwest.

use anyhow::{anyhow, bail, Result};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::jupiter::LAMPORTS_PER_SOL;

pub struct Rpc {
    client: reqwest::Client,
    url: String,
}

pub struct Simulation {
    /// `None` when the transaction would succeed.
    pub err: Option<Value>,
    pub logs: Vec<String>,
}

pub struct SignatureStatus {
    pub err: Option<Value>,
    /// "processed", "confirmed" or "finalized".
    pub confirmation: Option<String>,
}

/// What a confirmed swap did to the wallet, from the transaction's metadata.
pub struct Settlement {
    /// Change in the wallet's balance of the token, in whole units.
    pub token_delta: f64,
    pub fee_sol: f64,
}

impl Rpc {
    pub fn new(client: reqwest::Client, url: String) -> Self {
        Self { client, url }
    }

    async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let resp: Value = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(err) = resp.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| err.to_string(), str::to_owned);
            bail!("{method}: {msg}");
        }
        Ok(serde_json::from_value(resp["result"].clone())?)
    }

    pub async fn sol_balance(&self, owner: &str) -> Result<f64> {
        let r: Value = self
            .call("getBalance", json!([owner, { "commitment": "confirmed" }]))
            .await?;
        let lamports = r["value"]
            .as_u64()
            .ok_or_else(|| anyhow!("getBalance: no value"))?;
        Ok(lamports as f64 / LAMPORTS_PER_SOL)
    }

    /// Sum over all of the owner's accounts for this mint; 0 when there are none.
    pub async fn token_balance(&self, owner: &str, mint: &str) -> Result<f64> {
        let r: Value = self
            .call(
                "getTokenAccountsByOwner",
                json!([owner, { "mint": mint }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
            )
            .await?;
        let accounts = r["value"]
            .as_array()
            .ok_or_else(|| anyhow!("getTokenAccountsByOwner: no value"))?;
        Ok(accounts
            .iter()
            .filter_map(|a| {
                a["account"]["data"]["parsed"]["info"]["tokenAmount"]["uiAmount"].as_f64()
            })
            // Not `.sum()`: an empty f64 sum is -0.0, which displays as "-0.00".
            .fold(0.0, |acc, v| acc + v))
    }

    pub async fn simulate(&self, tx_b64: &str) -> Result<Simulation> {
        let r: Value = self
            .call(
                "simulateTransaction",
                json!([tx_b64, { "encoding": "base64", "sigVerify": true, "commitment": "processed" }]),
            )
            .await?;
        let v = &r["value"];
        Ok(Simulation {
            err: v.get("err").filter(|e| !e.is_null()).cloned(),
            logs: v["logs"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .filter_map(|s| s.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// Preflight is skipped because the same bytes were just simulated. The
    /// caller re-sends until confirmed, so the node does not retry either.
    pub async fn send(&self, tx_b64: &str) -> Result<String> {
        self.call(
            "sendTransaction",
            json!([tx_b64, { "encoding": "base64", "skipPreflight": true, "maxRetries": 0 }]),
        )
        .await
    }

    pub async fn signature_status(&self, sig: &str) -> Result<Option<SignatureStatus>> {
        let r: Value = self.call("getSignatureStatuses", json!([[sig]])).await?;
        let s = &r["value"][0];
        if s.is_null() {
            return Ok(None);
        }
        Ok(Some(SignatureStatus {
            err: s.get("err").filter(|e| !e.is_null()).cloned(),
            confirmation: s["confirmationStatus"].as_str().map(str::to_owned),
        }))
    }

    pub async fn block_height(&self) -> Result<u64> {
        self.call("getBlockHeight", json!([{ "commitment": "confirmed" }]))
            .await
    }

    /// `None` while the node has not indexed the transaction yet.
    pub async fn settlement(
        &self,
        sig: &str,
        owner: &str,
        mint: &str,
    ) -> Result<Option<Settlement>> {
        let r: Value = self
            .call(
                "getTransaction",
                json!([sig, { "encoding": "jsonParsed", "maxSupportedTransactionVersion": 0, "commitment": "confirmed" }]),
            )
            .await?;
        if r.is_null() {
            return Ok(None);
        }
        let meta = &r["meta"];
        let sum = |key: &str| -> f64 {
            meta[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| b["owner"] == owner && b["mint"] == mint)
                .filter_map(|b| b["uiTokenAmount"]["uiAmount"].as_f64())
                .fold(0.0, |acc, v| acc + v)
        };
        Ok(Some(Settlement {
            token_delta: sum("postTokenBalances") - sum("preTokenBalances"),
            fee_sol: meta["fee"].as_u64().unwrap_or(0) as f64 / LAMPORTS_PER_SOL,
        }))
    }
}
