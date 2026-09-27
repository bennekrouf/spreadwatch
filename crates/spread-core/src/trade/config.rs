//! Trading settings and the persisted daily volume.
//!
//! Everything lives in one folder (see `config_dir`). The settings file is
//! optional; without it the defaults below apply.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Ceilings the settings file cannot raise. A typo like `max_trade_sol = 100`
/// gets clamped here instead of trading 100 SOL.
const HARD_MAX_TRADE_SOL: f64 = 10.0;
const HARD_MAX_DAILY_SOL: f64 = 50.0;
const HARD_MAX_SLIPPAGE_BPS: u16 = 300;
/// 0.001 SOL.
const HARD_MAX_PRIORITY_FEE_LAMPORTS: u64 = 1_000_000;

/// Where the app kept its files when it was called SOL Spread.
fn legacy_config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config")
        .join("sol-spread")
}

/// `~/.config/spreadwatch` on macOS and Linux, next to where the Solana CLI
/// keeps its own files. `%LOCALAPPDATA%\Spreadwatch` on Windows: the profile
/// folder only its user can read, and unlike Roaming it is never copied to a
/// domain's profile server, which a hot wallet must not be.
fn default_config_dir() -> PathBuf {
    if cfg!(windows) {
        dirs::data_local_dir()
            .unwrap_or_default()
            .join("Spreadwatch")
    } else {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".config")
            .join("spreadwatch")
    }
}

/// The first call moves a folder left by SOL Spread into place, so the hot
/// wallet, settings and watchlist carry over. If the move fails the old
/// folder stays in use: a wallet the app cannot find would look like none.
pub fn config_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = default_config_dir();
        let legacy = legacy_config_dir();
        if dir.exists() || !legacy.exists() {
            return dir;
        }
        match std::fs::rename(&legacy, &dir) {
            Ok(()) => {
                tracing::info!("moved {} to {}", legacy.display(), dir.display());
                dir
            }
            Err(e) => {
                tracing::warn!(
                    "could not move {} to {}: {e}; still using it",
                    legacy.display(),
                    dir.display()
                );
                legacy
            }
        }
    })
    .clone()
}

pub fn settings_path() -> PathBuf {
    config_dir().join("trade.toml")
}

fn daily_path() -> PathBuf {
    config_dir().join("daily.json")
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TradeConfig {
    /// Solana CLI keyfile format: a JSON array of 64 bytes.
    pub keypair_path: PathBuf,
    pub rpc_url: String,
    pub max_trade_sol: f64,
    /// Live volume per local calendar day, summed over both directions.
    pub max_daily_sol: f64,
    pub max_slippage_bps: u16,
    /// Cap on the priority fee Jupiter may add to a swap.
    pub max_priority_fee_lamports: u64,
    /// From portal.jup.ag. Raises the limit from 30 to 60 requests a minute
    /// and switches to api.jup.ag. `JUPITER_API_KEY` in the environment wins.
    pub jupiter_api_key: Option<String>,
    /// Requests per minute of your Jupiter plan, only needed for paid plans.
    pub jupiter_rpm: Option<u32>,
}

impl Default for TradeConfig {
    fn default() -> Self {
        Self {
            keypair_path: config_dir().join("hot-wallet.json"),
            // Rate-limited and unreliable for sending; set a provider URL
            // (Helius, Triton, QuickNode…) in trade.toml before going live.
            rpc_url: "https://api.mainnet-beta.solana.com".into(),
            max_trade_sol: 1.0,
            max_daily_sol: 5.0,
            max_slippage_bps: 50,
            max_priority_fee_lamports: 100_000,
            jupiter_api_key: None,
            jupiter_rpm: None,
        }
    }
}

impl TradeConfig {
    /// Settings from `trade.toml` when present, clamped to the hard ceilings.
    /// The second value lists anything the user should know about.
    pub fn load() -> (Self, Vec<String>) {
        let path = settings_path();
        let mut notes = Vec::new();
        let mut cfg = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<TradeConfig>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    notes.push(format!(
                        "{} is invalid, using defaults: {e}",
                        path.display()
                    ));
                    TradeConfig::default()
                }
            },
            Err(_) => TradeConfig::default(),
        };
        cfg.keypair_path = relocate_legacy(expand_home(&cfg.keypair_path), &mut notes);
        if let Ok(key) = std::env::var("JUPITER_API_KEY") {
            cfg.jupiter_api_key = Some(key);
        }
        if cfg.jupiter_api_key.is_some() {
            if let Ok(Some(why)) = super::private::exposure(&path) {
                notes.push(format!(
                    "{} holds your Jupiter API key and other users can read it ({why}); run: {}",
                    path.display(),
                    super::private::fix_hint(&path)
                ));
            }
        }
        cfg.clamp(&mut notes);
        (cfg, notes)
    }

    fn clamp(&mut self, notes: &mut Vec<String>) {
        fn cap<T: PartialOrd + Copy + std::fmt::Display>(
            name: &str,
            v: &mut T,
            max: T,
            notes: &mut Vec<String>,
        ) {
            if *v > max {
                notes.push(format!("{name} = {v} is above the hard limit; using {max}"));
                *v = max;
            }
        }
        cap(
            "max_trade_sol",
            &mut self.max_trade_sol,
            HARD_MAX_TRADE_SOL,
            notes,
        );
        cap(
            "max_daily_sol",
            &mut self.max_daily_sol,
            HARD_MAX_DAILY_SOL,
            notes,
        );
        cap(
            "max_slippage_bps",
            &mut self.max_slippage_bps,
            HARD_MAX_SLIPPAGE_BPS,
            notes,
        );
        cap(
            "max_priority_fee_lamports",
            &mut self.max_priority_fee_lamports,
            HARD_MAX_PRIORITY_FEE_LAMPORTS,
            notes,
        );
        // NaN or negative would disable the checks that compare against these.
        let invalid = |v: f64| v.is_nan() || v < 0.0;
        if invalid(self.max_trade_sol) || invalid(self.max_daily_sol) {
            notes.push("negative or invalid SOL limits; trading disabled".into());
            self.max_trade_sol = 0.0;
            self.max_daily_sol = 0.0;
        }
    }
}

fn expand_home(p: &Path) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        Err(_) => p.to_path_buf(),
    }
}

/// A `keypair_path` written as `~/.config/sol-spread/...` points into the
/// folder `config_dir` has since moved; follow the file to its new place.
fn relocate_legacy(p: PathBuf, notes: &mut Vec<String>) -> PathBuf {
    let Ok(rest) = p.strip_prefix(legacy_config_dir()) else {
        return p;
    };
    if p.exists() {
        return p;
    }
    let moved = config_dir().join(rest);
    notes.push(format!(
        "keypair_path points into the old ~/.config/sol-spread folder; using {} (update trade.toml)",
        moved.display()
    ));
    moved
}

/// Live volume already traded today. Stored on disk so restarting the app
/// does not reset the daily limit.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DailyUsage {
    date: String,
    sol: f64,
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

impl DailyUsage {
    pub fn load() -> Self {
        std::fs::read_to_string(daily_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn used_today(&self) -> f64 {
        if self.date == today() {
            self.sol
        } else {
            0.0
        }
    }

    pub fn add(&mut self, sol: f64) -> std::io::Result<()> {
        let used = self.used_today();
        self.date = today();
        self.sol = used + sol;
        std::fs::create_dir_all(config_dir())?;
        std::fs::write(daily_path(), serde_json::to_string(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_clamped_to_hard_limits() {
        let mut cfg: TradeConfig =
            toml::from_str("max_trade_sol = 100.0\nmax_slippage_bps = 5000").unwrap();
        let mut notes = Vec::new();
        cfg.clamp(&mut notes);
        assert_eq!(cfg.max_trade_sol, HARD_MAX_TRADE_SOL);
        assert_eq!(cfg.max_slippage_bps, HARD_MAX_SLIPPAGE_BPS);
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn jupiter_settings_are_optional() {
        let cfg: TradeConfig =
            toml::from_str("jupiter_api_key = \"abc\"\njupiter_rpm = 600").unwrap();
        assert_eq!(cfg.jupiter_api_key.as_deref(), Some("abc"));
        assert_eq!(cfg.jupiter_rpm, Some(600));
        assert_eq!(
            toml::from_str::<TradeConfig>("").unwrap().jupiter_api_key,
            None
        );
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<TradeConfig>("max_trade = 1.0").is_err());
    }

    #[test]
    fn nan_limit_disables_trading() {
        let mut cfg = TradeConfig {
            max_trade_sol: f64::NAN,
            ..Default::default()
        };
        cfg.clamp(&mut Vec::new());
        assert_eq!(cfg.max_trade_sol, 0.0);
    }
}
