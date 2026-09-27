//! Headless view of the market: every two seconds, one line per followed
//! asset, then the venues, best routes and lead/lag of the first one.
//! Follows the same watchlist as the app.
//! `RUST_LOG=info cargo run -p spread-core --bin spread-cli`

use std::time::Duration;

use spread_core::jupiter::JupiterApi;
use spread_core::trade::TradeConfig;
use spread_core::{EngineConfig, MarketCmd, MarketSnapshot};
use tokio::sync::{mpsc, watch};

/// Enough decimals for BTC at 60 000 and BONK at 0.00002 alike.
fn fmt_px(v: Option<f64>) -> String {
    let Some(v) = v else { return "-".into() };
    let digits = if v > 0.0 {
        5 - v.log10().floor() as i32
    } else {
        2
    };
    format!("{v:.*}", digits.clamp(2, 10) as usize)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Same settings file as the app, for the Jupiter key.
    let (trade_cfg, notices) = TradeConfig::load();
    for n in notices {
        tracing::warn!("{n}");
    }
    let jupiter = JupiterApi::new(trade_cfg.jupiter_api_key, trade_cfg.jupiter_rpm);
    let cfg = EngineConfig {
        poll_every: jupiter.feed_poll_every(),
        ..EngineConfig::default()
    };
    println!("Jupiter: {}", jupiter.describe());

    let (tx, mut rx) = watch::channel(MarketSnapshot::default());
    // Kept alive so the market keeps running; the CLI sends no commands.
    let (_cmds, cmd_rx) = mpsc::channel::<MarketCmd>(1);
    tokio::spawn(spread_core::run(cfg, jupiter, cmd_rx, tx));

    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        let m = rx.borrow_and_update().clone();
        for a in &m.assets {
            println!(
                "{:<6} {:>14}  best {:>7}  {}/{} fresh{}",
                a.symbol,
                fmt_px(a.mid),
                a.best_gross_bps.map_or("-".into(), |b| format!("{b:+.2}")),
                a.fresh,
                a.listed,
                a.jupiter
                    .as_ref()
                    .map_or(String::new(), |t| format!("  jup: {}", t.name)),
            );
        }
        let s = &m.detail;
        println!("-- {}", m.selected.as_deref().unwrap_or("-"));
        for v in &s.venues {
            println!(
                "{:<8} {:>14}/{:<14} {:>6} bps  {:>10}  {:>11}  {:>4}/s{}{}",
                v.venue.name(),
                fmt_px(v.bid),
                fmt_px(v.ask),
                v.dev_bps.map_or("-".into(), |d| format!("{d:+.1}")),
                v.lag_ms
                    .map_or("lag -".into(), |ms| format!("lag {ms:+.0}ms")),
                v.net_latency_ms
                    .map_or("net -".into(), |ms| format!("net {ms:.0}ms")),
                v.rate,
                if v.stale { "  STALE" } else { "" },
                if v.unlisted { "  NOT LISTED" } else { "" },
            );
        }
        for r in s.routes.iter().take(3) {
            println!(
                "  buy {:<8} sell {:<8} {:+6.2} bps gross {:+7.2} bps net",
                r.buy.name(),
                r.sell.name(),
                r.gross_bps,
                r.net_bps
            );
        }
        for p in s.lag_pairs.iter().filter(|p| p.reliable) {
            println!(
                "  {:<8} follows {:<8} {:+5.0} ms  n={}",
                p.follower.name(),
                p.leader.name(),
                p.median_ms,
                p.samples
            );
        }
        println!("  {} lag samples\n", s.lag_samples);
    }
}
