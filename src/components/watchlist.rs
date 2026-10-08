use dioxus::prelude::*;
use spread_core::model::{AssetSummary, GateListing};
use spread_core::MarketCmd;
use tokio::sync::mpsc;

use super::{fmt_px, Pro, Tab};

#[component]
pub fn Watchlist(
    assets: Vec<AssetSummary>,
    selected: Option<String>,
    adding: Option<String>,
    notice: Option<String>,
    locked: Vec<String>,
    limit: Option<usize>,
    at_limit: bool,
) -> Element {
    let tx = use_context::<mpsc::Sender<MarketCmd>>();
    let mut tab = use_context::<Signal<Tab>>();
    let mut pro = use_context::<Pro>();
    // A `Callback` is `Copy`, so every handler below can capture it.
    let send = use_callback(move |cmd: MarketCmd| {
        if let Err(e) = tx.try_send(cmd) {
            tracing::warn!("watchlist command dropped: {e}");
        }
    });
    let mut input = use_signal(String::new);
    let mut submit = move || {
        let text = input.read().trim().to_owned();
        if !text.is_empty() {
            send(MarketCmd::Add(text));
            input.set(String::new());
        }
    };

    rsx! {
        aside { class: "watchlist",
            h3 { class: "watchlist-head",
                "Watchlist"
                if let Some(n) = limit {
                    span {
                        class: if at_limit { "muted small limit full" } else { "muted small limit" },
                        title: "The free version follows {n} assets at once",
                        "{assets.len()}/{n}"
                    }
                }
            }
            ul {
                for a in assets.iter() {
                    WatchItem {
                        key: "{a.symbol}",
                        asset: a.clone(),
                        active: Some(&a.symbol) == selected.as_ref(),
                        on_select: move |s: String| {
                            send(MarketCmd::Select(s));
                            tab.set(Tab::Market);
                        },
                        on_remove: move |s: String| send(MarketCmd::Remove(s)),
                    }
                }
                // Saved, but past the free version's limit: not followed.
                for symbol in locked.iter() {
                    LockedItem {
                        key: "locked-{symbol}",
                        symbol: symbol.clone(),
                        limit: limit.unwrap_or_default(),
                        on_follow: move |s: String| {
                            if at_limit {
                                pro.open.set(true);
                            } else {
                                send(MarketCmd::Follow(s));
                            }
                        },
                        on_remove: move |s: String| send(MarketCmd::Remove(s)),
                    }
                }
            }
            if let Some(symbol) = adding.as_ref() {
                p { class: "muted small", "Adding {symbol}…" }
            }
            form {
                class: "add",
                onsubmit: move |e| {
                    e.prevent_default();
                    submit();
                },
                input {
                    r#type: "text",
                    placeholder: "Add: BTC, ETH, JUP…",
                    value: "{input}",
                    oninput: move |e| input.set(e.value()),
                    disabled: adding.is_some(),
                }
                button { class: "btn", r#type: "submit", disabled: adding.is_some(), "Add" }
            }
            if let Some(n) = notice.as_ref() {
                p { class: "error small", "{n}" }
            }
            if at_limit && notice.as_deref().is_some_and(|n| n.starts_with("The free version")) {
                button { class: "btn buy small-btn", onclick: move |_| pro.open.set(true), "Get Pro…" }
            }
            p { class: "muted small",
                "Priced against USDT on each exchange. Jupiter joins when the symbol is a verified Solana token."
            }
        }
    }
}

#[component]
fn WatchItem(
    asset: AssetSummary,
    active: bool,
    on_select: EventHandler<String>,
    on_remove: EventHandler<String>,
) -> Element {
    let a = asset;
    let symbol = a.symbol.clone();
    let remove_symbol = a.symbol.clone();
    let edge_class = match a.best_gross_bps {
        Some(b) if b > 0.0 => "edge pos",
        Some(_) => "edge",
        None => "edge muted",
    };
    let edge = a.best_gross_bps.map_or("–".into(), |b| {
        format!("{b:+.1} bps ({:+.3} %)", b / 100.0)
    });
    // Only worth a line while Gate doesn't trade it yet.
    let gate = a.gate.filter(|g| *g != GateListing::Trading);
    let health = if a.listed == 0 {
        "none".to_owned()
    } else {
        format!("{}/{}", a.fresh, a.listed)
    };

    rsx! {
        li {
            class: if active { "item active" } else { "item" },
            onclick: move |_| on_select.call(symbol.clone()),
            div { class: "item-top",
                strong { "{a.symbol}" }
                span { class: "num", "{fmt_px(a.mid)}" }
            }
            div { class: "item-bottom small",
                span { class: edge_class, title: "Best gross edge between two venues", "{edge}" }
                span {
                    class: if a.listed == 0 { "error" } else { "muted" },
                    title: "Venues quoting now / venues listing it",
                    "{health}"
                }
                button {
                    class: "remove",
                    title: "Stop following {a.symbol}",
                    onclick: move |e| {
                        // Don't also select the row being removed.
                        e.stop_propagation();
                        on_remove.call(remove_symbol.clone());
                    },
                    "×"
                }
            }
            if let Some(g) = gate {
                div {
                    class: if matches!(g, GateListing::NotListed | GateListing::Delisted) { "item-note small muted" } else { "item-note small listing" },
                    title: "{g.describe()}",
                    "{g.short()}"
                }
            }
        }
    }
}

#[component]
fn LockedItem(
    symbol: String,
    limit: usize,
    on_follow: EventHandler<String>,
    on_remove: EventHandler<String>,
) -> Element {
    let follow_symbol = symbol.clone();
    let remove_symbol = symbol.clone();
    rsx! {
        li {
            class: "item locked",
            title: "Not followed: the free version follows {limit} assets. Click to follow it once there is room, or get Pro to follow every asset.",
            onclick: move |_| on_follow.call(follow_symbol.clone()),
            div { class: "item-top",
                strong { "{symbol}" }
                span { class: "lock-tag small", "🔒 Pro" }
            }
            div { class: "item-bottom small",
                span { class: "muted", "not followed" }
                button {
                    class: "remove",
                    title: "Remove {symbol} from the watchlist",
                    onclick: move |e| {
                        e.stop_propagation();
                        on_remove.call(remove_symbol.clone());
                    },
                    "×"
                }
            }
        }
    }
}
