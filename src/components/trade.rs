use dioxus::prelude::*;
use spread_core::trade::{Side, TradeCmd, TradeRecord, TradeState, TradeStatus, WalletStatus};
use tokio::sync::mpsc;

use super::fmt_px;

/// Small on purpose; the executor still enforces the configured maximum.
const DEFAULT_SIZE: &str = "0.1";

fn short(key: &str) -> String {
    if key.len() > 12 {
        format!("{}…{}", &key[..4], &key[key.len() - 4..])
    } else {
        key.to_string()
    }
}

#[component]
pub fn TradePanel(state: TradeState) -> Element {
    let tx = use_context::<mpsc::Sender<TradeCmd>>();
    // A `Callback` is `Copy`, so every handler below can capture it.
    let send = use_callback(move |cmd: TradeCmd| {
        if let Err(e) = tx.try_send(cmd) {
            tracing::warn!("trade command dropped: {e}");
        }
    });
    let mut size = use_signal(|| DEFAULT_SIZE.to_string());
    let mut confirming_live = use_signal(|| false);
    let mut input_error = use_signal(|| None::<String>);

    let loaded = matches!(state.wallet, WalletStatus::Loaded { .. });
    let can_trade = loaded && !state.busy;

    let mut submit = move |side: Side| match size.read().trim().parse::<f64>() {
        Ok(v) if v > 0.0 => {
            input_error.set(None);
            send(TradeCmd::Swap { side, size_sol: v });
        }
        _ => input_error.set(Some("Enter a size in SOL, e.g. 0.1".into())),
    };

    rsx! {
        div { class: if state.live { "card trade live" } else { "card trade" },
            div { class: "trade-head",
                h3 { "On-chain · Jupiter · hot wallet" }
                span { class: if state.live { "badge live" } else { "badge dry" },
                    if state.live { "LIVE" } else { "DRY RUN" }
                }
            }

            div { class: "wallet-row",
                match &state.wallet {
                    WalletStatus::Loaded { pubkey } => rsx! {
                        span { class: "muted", "Wallet" }
                        code { class: "pubkey", title: "{pubkey}", "{pubkey}" }
                        span { class: "num", "{fmt_bal(state.sol, 4)} SOL" }
                        span { class: "num", "{fmt_bal(state.usdt, 2)} USDT" }
                        button { class: "btn ghost", onclick: move |_| send(TradeCmd::RefreshBalances), "Refresh" }
                    },
                    WalletStatus::Missing { path } => rsx! {
                        span { class: "muted", "No hot wallet at {path}" }
                        button { class: "btn", onclick: move |_| send(TradeCmd::CreateWallet), "Create hot wallet" }
                        span { class: "muted small", "Fund it only with what you can afford to lose." }
                    },
                    WalletStatus::Error(e) => rsx! {
                        span { class: "error", "{e}" }
                    },
                }
            }

            p { class: "muted small",
                "Max {state.max_trade_sol} SOL per trade · live today {state.used_today_sol:.3} / {state.max_daily_sol} SOL · slippage ≤ {state.max_slippage_bps} bps"
            }
            p { class: "muted small",
                "Jupiter {state.jupiter} · RPC {rpc_host(&state.rpc_url)} · settings {state.settings_path}"
            }

            div { class: "trade-controls",
                label { class: "size",
                    "Size"
                    input {
                        r#type: "number",
                        step: "0.1",
                        min: "0",
                        value: "{size}",
                        oninput: move |e| size.set(e.value()),
                    }
                    "SOL"
                }
                button {
                    class: "btn sell",
                    disabled: !can_trade,
                    onclick: move |_| submit(Side::SellSol),
                    "Sell SOL → USDT"
                }
                button {
                    class: "btn buy",
                    disabled: !can_trade,
                    onclick: move |_| submit(Side::BuySol),
                    "Buy SOL with USDT"
                }
                if state.busy {
                    span { class: "muted", "working…" }
                }
                span { class: "spacer" }
                if state.live {
                    button {
                        class: "btn ghost",
                        onclick: move |_| send(TradeCmd::SetLive(false)),
                        "Back to dry run"
                    }
                } else if *confirming_live.read() {
                    span { class: "error", "Real funds will be swapped." }
                    button {
                        class: "btn danger",
                        onclick: move |_| {
                            confirming_live.set(false);
                            send(TradeCmd::SetLive(true));
                        },
                        "Confirm live"
                    }
                    button { class: "btn ghost", onclick: move |_| confirming_live.set(false), "Cancel" }
                } else {
                    button {
                        class: "btn ghost",
                        disabled: !loaded,
                        onclick: move |_| confirming_live.set(true),
                        "Enable live trading…"
                    }
                }
            }
            if let Some(e) = input_error.read().as_ref() {
                p { class: "error small", "{e}" }
            }
            for n in state.notices.iter() {
                p { class: "error small", "{n}" }
            }

            if !state.records.is_empty() {
                table { class: "trades",
                    thead {
                        tr {
                            th { "Time" }
                            th { "Side" }
                            th { class: "num", "Size" }
                            th { "Mode" }
                            th { class: "num", "Quoted" }
                            th { class: "num", "Fill" }
                            th { class: "num", "Slip bps" }
                            th { class: "num", "Fee SOL" }
                            th { "Status" }
                            th { "Tx" }
                        }
                    }
                    tbody {
                        for r in state.records.iter() {
                            TradeRow { key: "{r.id}", record: r.clone() }
                        }
                    }
                }
            }
        }
    }
}

/// Provider URLs often carry an API key in the query string; show the host only.
fn rpc_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?']).next().unwrap_or(rest)
}

fn fmt_bal(v: Option<f64>, decimals: usize) -> String {
    v.map_or("–".into(), |v| format!("{v:.decimals$}"))
}

#[component]
fn TradeRow(record: TradeRecord) -> Element {
    let r = record;
    let (class, text) = match &r.status {
        TradeStatus::Quoting => ("pending", "quoting…".to_string()),
        TradeStatus::Simulating => ("pending", "simulating…".to_string()),
        TradeStatus::Confirming => ("pending", "confirming…".to_string()),
        TradeStatus::Simulated => ("ok", "would succeed".to_string()),
        TradeStatus::Confirmed => ("ok", "confirmed".to_string()),
        TradeStatus::Rejected(why) => ("warn", format!("rejected: {why}")),
        TradeStatus::Failed(why) => ("error", format!("failed: {why}")),
    };
    let slip = r.slippage_bps().map_or("–".into(), |s| format!("{s:+.1}"));
    let fee = r.fee_sol.map_or("–".into(), |f| format!("{f:.6}"));

    rsx! {
        tr {
            td { class: "muted", "{r.at}" }
            td { "{r.side.label()}" }
            td { class: "num", "{r.size_sol}" }
            td { if r.live { "live" } else { "dry" } }
            td { class: "num", "{fmt_px(r.quoted_px)}" }
            td { class: "num", "{fmt_px(r.fill_px)}" }
            td { class: "num", "{slip}" }
            td { class: "num", "{fee}" }
            td { class: "status {class}", title: "{text}", "{text}" }
            td {
                if let Some(sig) = r.signature.clone() {
                    a {
                        href: "#",
                        title: "{sig}",
                        // Inside the webview a plain link would navigate the app away.
                        onclick: move |e| {
                            e.prevent_default();
                            let _ = open::that(format!("https://solscan.io/tx/{sig}"));
                        },
                        "{short(&sig)}"
                    }
                }
            }
        }
    }
}
