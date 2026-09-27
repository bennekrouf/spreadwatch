//! On-chain trading through Jupiter with an app-held hot wallet.
//!
//! One task owns the wallet and runs commands from the UI one at a time:
//! quote → build → sign → simulate, and only in live mode send → confirm.
//! Live mode is off at every start. Limits are checked here, not in the UI.

mod config;
mod jupiter;
mod private;
mod rpc;
mod wallet;

pub use config::{config_dir, settings_path, TradeConfig};

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use base64::Engine as _;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use tokio::sync::{mpsc, watch};

use std::sync::Arc;

use config::DailyUsage;
use rpc::Rpc;

use crate::jupiter::{JupiterApi, USDT_MINT};

const MAX_RECORDS: usize = 50;
const BALANCE_REFRESH: Duration = Duration::from_secs(30);
/// Left in the wallet when selling SOL: network fee, priority fee and rent
/// for a USDT account on the first swap.
const SOL_RESERVE: f64 = 0.01;
const CONFIRM_POLL: Duration = Duration::from_millis(500);
const RESEND_EVERY: Duration = Duration::from_secs(2);
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// SOL → USDT, spending exactly the size.
    SellSol,
    /// USDT → SOL, receiving exactly the size.
    BuySol,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::SellSol => "Sell SOL",
            Side::BuySol => "Buy SOL",
        }
    }
}

#[derive(Debug, Clone)]
pub enum TradeCmd {
    RefreshBalances,
    CreateWallet,
    SetLive(bool),
    Swap { side: Side, size_sol: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum WalletStatus {
    Missing { path: String },
    Loaded { pubkey: String },
    Error(String),
}

impl Default for WalletStatus {
    fn default() -> Self {
        WalletStatus::Missing {
            path: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TradeStatus {
    Quoting,
    Simulating,
    Confirming,
    /// Dry run finished: the swap would have succeeded.
    Simulated,
    Confirmed,
    /// Stopped by a limit or a missing precondition; nothing was sent.
    Rejected(String),
    Failed(String),
}

impl TradeStatus {
    pub fn is_done(&self) -> bool {
        !matches!(
            self,
            TradeStatus::Quoting | TradeStatus::Simulating | TradeStatus::Confirming
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TradeRecord {
    pub id: u64,
    pub at: String,
    pub side: Side,
    pub size_sol: f64,
    pub live: bool,
    /// USDT per SOL.
    pub quoted_px: Option<f64>,
    pub fill_px: Option<f64>,
    pub fee_sol: Option<f64>,
    pub signature: Option<String>,
    pub status: TradeStatus,
}

impl TradeRecord {
    /// Positive when the fill was worse than the quote.
    pub fn slippage_bps(&self) -> Option<f64> {
        let (q, f) = (self.quoted_px?, self.fill_px?);
        Some(match self.side {
            Side::SellSol => (q - f) / q * 1e4,
            Side::BuySol => (f - q) / q * 1e4,
        })
    }
}

/// `Default` is only the placeholder until the trade task publishes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TradeState {
    pub wallet: WalletStatus,
    pub sol: Option<f64>,
    pub usdt: Option<f64>,
    pub live: bool,
    pub busy: bool,
    pub used_today_sol: f64,
    pub max_trade_sol: f64,
    pub max_daily_sol: f64,
    pub max_slippage_bps: u16,
    pub rpc_url: String,
    /// Jupiter access, e.g. "API key · 60 req/min".
    pub jupiter: String,
    pub settings_path: String,
    /// Settings problems and the last balance error.
    pub notices: Vec<String>,
    /// Newest first.
    pub records: Vec<TradeRecord>,
}

/// `notices` are the settings problems from `TradeConfig::load`, shown in the panel.
pub async fn run(
    cfg: TradeConfig,
    notices: Vec<String>,
    jupiter: Arc<JupiterApi>,
    mut cmds: mpsc::Receiver<TradeCmd>,
    out: watch::Sender<TradeState>,
) {
    let mut ex = Executor::new(cfg, notices, jupiter, out);
    ex.refresh_balances().await;
    let mut refresh = tokio::time::interval_at(
        tokio::time::Instant::now() + BALANCE_REFRESH,
        BALANCE_REFRESH,
    );
    loop {
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(cmd) => ex.handle(cmd).await,
                None => return,
            },
            _ = refresh.tick() => ex.refresh_balances().await,
        }
    }
}

struct Executor {
    cfg: TradeConfig,
    jupiter: Arc<JupiterApi>,
    http: reqwest::Client,
    rpc: Rpc,
    keypair: Option<Keypair>,
    daily: DailyUsage,
    state: TradeState,
    out: watch::Sender<TradeState>,
    next_id: u64,
}

impl Executor {
    fn new(
        cfg: TradeConfig,
        notices: Vec<String>,
        jupiter: Arc<JupiterApi>,
        out: watch::Sender<TradeState>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("http client");
        let daily = DailyUsage::load();
        let state = TradeState {
            wallet: WalletStatus::Missing {
                path: cfg.keypair_path.display().to_string(),
            },
            sol: None,
            usdt: None,
            live: false,
            busy: false,
            used_today_sol: daily.used_today(),
            max_trade_sol: cfg.max_trade_sol,
            max_daily_sol: cfg.max_daily_sol,
            max_slippage_bps: cfg.max_slippage_bps,
            rpc_url: cfg.rpc_url.clone(),
            jupiter: jupiter.describe(),
            settings_path: settings_path().display().to_string(),
            notices,
            records: Vec::new(),
        };
        let mut ex = Self {
            rpc: Rpc::new(http.clone(), cfg.rpc_url.clone()),
            cfg,
            jupiter,
            http,
            keypair: None,
            daily,
            state,
            out,
            next_id: 1,
        };
        ex.load_wallet();
        ex.publish();
        ex
    }

    fn publish(&self) {
        self.out.send_replace(self.state.clone());
    }

    fn load_wallet(&mut self) {
        let path = &self.cfg.keypair_path;
        match wallet::load(path) {
            Ok(kp) => {
                self.state.wallet = WalletStatus::Loaded {
                    pubkey: kp.pubkey().to_string(),
                };
                self.keypair = Some(kp);
            }
            Err(wallet::LoadError::Missing) => {
                self.state.wallet = WalletStatus::Missing {
                    path: path.display().to_string(),
                };
            }
            Err(wallet::LoadError::Invalid(e)) => self.state.wallet = WalletStatus::Error(e),
        }
    }

    async fn handle(&mut self, cmd: TradeCmd) {
        match cmd {
            TradeCmd::RefreshBalances => self.refresh_balances().await,
            // Only offered while no wallet is loaded; a stray repeat is a no-op.
            TradeCmd::CreateWallet if self.keypair.is_some() => {}
            TradeCmd::CreateWallet => {
                if let Err(e) = wallet::create(&self.cfg.keypair_path) {
                    self.state.wallet =
                        WalletStatus::Error(format!("could not create wallet: {e}"));
                } else {
                    self.load_wallet();
                    self.refresh_balances().await;
                }
                self.publish();
            }
            TradeCmd::SetLive(live) => {
                self.state.live = live && self.keypair.is_some();
                tracing::warn!(
                    "trading mode: {}",
                    if self.state.live { "LIVE" } else { "dry run" }
                );
                self.publish();
            }
            TradeCmd::Swap { side, size_sol } => {
                self.state.busy = true;
                self.swap(side, size_sol).await;
                self.state.busy = false;
                self.publish();
                self.refresh_balances().await;
            }
        }
    }

    async fn refresh_balances(&mut self) {
        let Some(kp) = &self.keypair else { return };
        let owner = kp.pubkey().to_string();
        let (sol, usdt) = tokio::join!(
            self.rpc.sol_balance(&owner),
            self.rpc.token_balance(&owner, USDT_MINT)
        );
        self.state.notices.retain(|n| !n.starts_with("balance:"));
        match (sol, usdt) {
            (Ok(sol), Ok(usdt)) => {
                self.state.sol = Some(sol);
                self.state.usdt = Some(usdt);
            }
            (Err(e), _) | (_, Err(e)) => self.state.notices.push(format!("balance: {e:#}")),
        }
        self.publish();
    }

    fn record_mut(&mut self, id: u64) -> &mut TradeRecord {
        self.state
            .records
            .iter_mut()
            .find(|r| r.id == id)
            .expect("record exists while its swap runs")
    }

    fn set_status(&mut self, id: u64, status: TradeStatus) {
        if status.is_done() {
            tracing::info!("trade #{id}: {status:?}");
        }
        self.record_mut(id).status = status;
        self.publish();
    }

    async fn swap(&mut self, side: Side, size_sol: f64) {
        let id = self.next_id;
        self.next_id += 1;
        let live = self.state.live;
        self.state.records.insert(
            0,
            TradeRecord {
                id,
                at: chrono::Local::now().format("%H:%M:%S").to_string(),
                side,
                size_sol,
                live,
                quoted_px: None,
                fill_px: None,
                fee_sol: None,
                signature: None,
                status: TradeStatus::Quoting,
            },
        );
        self.state.records.truncate(MAX_RECORDS);
        self.publish();

        let status = match self.run_swap(id, side, size_sol, live).await {
            Ok(status) => status,
            Err(e) => TradeStatus::Failed(format!("{e:#}")),
        };
        self.set_status(id, status);
    }

    /// Returns the final status; `Err` is an unexpected failure.
    async fn run_swap(
        &mut self,
        id: u64,
        side: Side,
        size_sol: f64,
        live: bool,
    ) -> Result<TradeStatus> {
        let Some(owner) = self.keypair.as_ref().map(|k| k.pubkey().to_string()) else {
            return Ok(TradeStatus::Rejected("no wallet loaded".into()));
        };
        if size_sol.is_nan() || size_sol <= 0.0 || size_sol > self.cfg.max_trade_sol {
            return Ok(TradeStatus::Rejected(format!(
                "size must be above 0 and at most {} SOL",
                self.cfg.max_trade_sol
            )));
        }
        if live && self.daily.used_today() + size_sol > self.cfg.max_daily_sol {
            return Ok(TradeStatus::Rejected(format!(
                "daily limit: {:.3} of {} SOL already traded today",
                self.daily.used_today(),
                self.cfg.max_daily_sol
            )));
        }
        if side == Side::SellSol {
            if let Some(sol) = self.state.sol.filter(|&sol| sol < size_sol + SOL_RESERVE) {
                return Ok(TradeStatus::Rejected(format!(
                    "not enough SOL: have {sol:.4}, need {size_sol} + {SOL_RESERVE} for fees"
                )));
            }
        }

        let quote = jupiter::quote(
            &self.jupiter,
            &self.http,
            side,
            size_sol,
            self.cfg.max_slippage_bps,
        )
        .await?;
        self.record_mut(id).quoted_px = Some(quote.price);
        if side == Side::BuySol {
            let worst = quote.usdt * (1.0 + f64::from(self.cfg.max_slippage_bps) / 1e4);
            if let Some(usdt) = self.state.usdt.filter(|&usdt| usdt < worst) {
                return Ok(TradeStatus::Rejected(format!(
                    "not enough USDT: have {usdt:.2}, need up to {worst:.2}"
                )));
            }
        }

        let unsigned = jupiter::build(
            &self.jupiter,
            &self.http,
            &quote,
            &owner,
            self.cfg.max_priority_fee_lamports,
        )
        .await?;
        let tx: VersionedTransaction = wincode::deserialize(&unsigned.tx)?;
        let keypair = self.keypair.as_ref().expect("checked above");
        // Fails unless the wallet is the one and only required signer.
        let signed = VersionedTransaction::try_new(tx.message, &[keypair])
            .map_err(|e| anyhow!("signing: {e}"))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(wincode::serialize(&signed)?);

        self.set_status(id, TradeStatus::Simulating);
        let sim = self.rpc.simulate(&b64).await?;
        if let Some(err) = sim.err {
            let tail = sim
                .logs
                .iter()
                .rev()
                .take(2)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ");
            return Ok(TradeStatus::Failed(format!("simulation: {err} {tail}")));
        }
        if !live {
            return Ok(TradeStatus::Simulated);
        }

        // Counted as soon as it is sent and never given back: if the outcome
        // turns out unknown, the limit errs on the safe side.
        self.daily.add(size_sol)?;
        self.state.used_today_sol = self.daily.used_today();
        let sig = self.rpc.send(&b64).await?;
        self.record_mut(id).signature = Some(sig.clone());
        self.set_status(id, TradeStatus::Confirming);

        let status = self
            .confirm(&sig, &b64, unsigned.last_valid_block_height)
            .await;
        if status == TradeStatus::Confirmed {
            self.settle(id, side, size_sol, &sig, &owner).await;
        }
        Ok(status)
    }

    /// Polls until confirmed, failed or expired, re-sending the same bytes
    /// meanwhile (a signature can only land once).
    async fn confirm(&self, sig: &str, b64: &str, last_valid: u64) -> TradeStatus {
        let started = Instant::now();
        let mut last_send = Instant::now();
        loop {
            tokio::time::sleep(CONFIRM_POLL).await;
            if let Ok(Some(st)) = self.rpc.signature_status(sig).await {
                if let Some(err) = st.err {
                    return TradeStatus::Failed(format!("landed but failed on-chain: {err}"));
                }
                if matches!(st.confirmation.as_deref(), Some("confirmed" | "finalized")) {
                    return TradeStatus::Confirmed;
                }
            }
            if last_send.elapsed() >= RESEND_EVERY {
                if self.rpc.block_height().await.is_ok_and(|h| h > last_valid) {
                    return TradeStatus::Failed(
                        "expired before landing; nothing was swapped".into(),
                    );
                }
                let _ = self.rpc.send(b64).await;
                last_send = Instant::now();
            }
            if started.elapsed() > CONFIRM_TIMEOUT {
                return TradeStatus::Failed(
                    "no confirmation after 90 s; check the signature on Solscan".into(),
                );
            }
        }
    }

    /// Fill price and fee from the confirmed transaction.
    async fn settle(&mut self, id: u64, side: Side, size_sol: f64, sig: &str, owner: &str) {
        for _ in 0..5 {
            match self.rpc.settlement(sig, owner, USDT_MINT).await {
                Ok(Some(s)) => {
                    let usdt = match side {
                        Side::SellSol => s.token_delta,
                        Side::BuySol => -s.token_delta,
                    };
                    let rec = self.record_mut(id);
                    rec.fill_px = Some(usdt / size_sol);
                    rec.fee_sol = Some(s.fee_sol);
                    return;
                }
                Ok(None) => tokio::time::sleep(Duration::from_secs(1)).await,
                Err(e) => {
                    tracing::warn!("trade #{id}: could not read settlement: {e:#}");
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(side: Side, quoted: f64, fill: f64) -> TradeRecord {
        TradeRecord {
            id: 1,
            at: String::new(),
            side,
            size_sol: 1.0,
            live: true,
            quoted_px: Some(quoted),
            fill_px: Some(fill),
            fee_sol: None,
            signature: None,
            status: TradeStatus::Confirmed,
        }
    }

    /// Quote → build → sign → simulate against mainnet with a throwaway key.
    /// The empty wallet makes the simulation fail, which is the point: it
    /// proves the signed bytes are accepted and evaluated by a real node.
    /// `cargo test -p spread-core -- --ignored dry_run`
    #[tokio::test]
    #[ignore = "network: Jupiter and Solana mainnet"]
    async fn dry_run_pipeline_against_mainnet() {
        let http = reqwest::Client::new();
        let rpc = Rpc::new(http.clone(), TradeConfig::default().rpc_url);
        let api = JupiterApi::new(std::env::var("JUPITER_API_KEY").ok(), None);
        let kp = Keypair::new();

        for side in [Side::SellSol, Side::BuySol] {
            let quote = jupiter::quote(&api, &http, side, 0.1, 50).await.unwrap();
            assert!(quote.price > 1.0, "{side:?} price {}", quote.price);

            let unsigned = jupiter::build(&api, &http, &quote, &kp.pubkey().to_string(), 100_000)
                .await
                .unwrap();
            let tx: VersionedTransaction = wincode::deserialize(&unsigned.tx).unwrap();
            let signed = VersionedTransaction::try_new(tx.message, &[&kp]).unwrap();
            let bytes = wincode::serialize(&signed).unwrap();
            assert_eq!(bytes.len(), unsigned.tx.len());

            let sim = rpc
                .simulate(&base64::engine::general_purpose::STANDARD.encode(&bytes))
                .await
                .unwrap();
            let err = sim.err.expect("an empty wallet cannot swap");
            println!("{side:?}: quoted {:.4}, simulation: {err}", quote.price);
        }
    }

    #[test]
    fn slippage_is_positive_when_the_fill_is_worse() {
        assert!((record(Side::SellSol, 100.0, 99.9).slippage_bps().unwrap() - 10.0).abs() < 1e-9);
        assert!((record(Side::BuySol, 100.0, 100.1).slippage_bps().unwrap() - 10.0).abs() < 1e-9);
        assert!(record(Side::SellSol, 100.0, 100.1).slippage_bps().unwrap() < 0.0);
    }
}
