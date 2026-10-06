mod components;
mod notice;
mod telemetry;
mod update_check;

use std::time::Duration;

use dioxus::desktop::LogicalSize;
use dioxus::prelude::*;
use spread_core::jupiter::JupiterApi;
use spread_core::scanner::ScanSnapshot;
use spread_core::trade::{TradeCmd, TradeConfig, TradeState};
use spread_core::{EngineConfig, MarketCmd, MarketSnapshot};
use tokio::sync::{mpsc, watch};

use components::{
    EventLog, LagPanel, NewListingsPanel, RoutesPanel, Tab, TradePanel, VenueCard, Watchlist,
};

/// The trade module swaps SOL/USDT only.
const TRADABLE: &str = "SOL";

const MAIN_CSS: &str = include_str!("../assets/main.css");
/// The engine publishes every 50 ms; the UI only needs to repaint this often.
const UI_REFRESH: Duration = Duration::from_millis(100);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Feeds and engine get their own runtime on their own thread, so a busy
    // UI never delays quote timestamps (which the lag measurement relies on).
    // The trade task shares that runtime: it is mostly waiting on HTTP.
    // Settings are read once: the Jupiter key serves the feed and trading.
    let (trade_cfg, notices) = TradeConfig::load();
    let jupiter = JupiterApi::new(trade_cfg.jupiter_api_key.clone(), trade_cfg.jupiter_rpm);
    let engine_cfg = EngineConfig {
        poll_every: jupiter.feed_poll_every(),
        ..EngineConfig::default()
    };
    tracing::info!(
        "Jupiter: {}, price poll every {:?}",
        jupiter.describe(),
        engine_cfg.poll_every
    );

    let (tx, rx) = watch::channel(MarketSnapshot::default());
    let (market_cmd_tx, market_cmd_rx) = mpsc::channel::<MarketCmd>(16);
    let (trade_tx, trade_rx) = watch::channel(TradeState::default());
    let (cmd_tx, cmd_rx) = mpsc::channel::<TradeCmd>(16);
    let (scan_tx, scan_rx) = watch::channel(ScanSnapshot::default());
    std::thread::Builder::new()
        .name("market-data".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("market-data runtime");
            rt.spawn(spread_core::scanner::run(scan_tx));
            rt.spawn(spread_core::trade::run(
                trade_cfg,
                notices,
                jupiter.clone(),
                cmd_rx,
                trade_tx,
            ));
            rt.block_on(spread_core::run(engine_cfg, jupiter, market_cmd_rx, tx));
        })
        .expect("spawn market-data thread");

    // Injected once into the page head: a `document::Style` inside `App`
    // would be re-diffed on every refresh.
    let cfg = dioxus::desktop::Config::new()
        .with_custom_head(format!("<style>{MAIN_CSS}</style>"))
        .with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title(concat!("Spreadwatch ", env!("CARGO_PKG_VERSION")))
                .with_inner_size(LogicalSize::new(1420.0, 940.0)),
        );
    LaunchBuilder::desktop()
        .with_cfg(cfg)
        .with_context(rx)
        .with_context(trade_rx)
        .with_context(scan_rx)
        .with_context(cmd_tx)
        .with_context(market_cmd_tx)
        .launch(App);
}

#[component]
fn App() -> Element {
    let rx = use_context::<watch::Receiver<MarketSnapshot>>();
    let trade_rx = use_context::<watch::Receiver<TradeState>>();
    let scan_rx = use_context::<watch::Receiver<ScanSnapshot>>();
    let mut snap = use_signal(MarketSnapshot::default);
    let mut trade = use_signal(TradeState::default);
    let mut scan = use_signal(ScanSnapshot::default);
    let mut tab = use_context_provider(|| Signal::new(Tab::Market));

    use_future(move || {
        let rx = rx.clone();
        let trade_rx = trade_rx.clone();
        let mut scan_rx = scan_rx.clone();
        async move {
            let mut tick = tokio::time::interval(UI_REFRESH);
            loop {
                tick.tick().await;
                let latest = rx.borrow().clone();
                if *snap.peek() != latest {
                    snap.set(latest);
                }
                let latest = trade_rx.borrow().clone();
                if *trade.peek() != latest {
                    trade.set(latest);
                }
                // Changes every few seconds at most: check before cloning.
                if scan_rx.has_changed().unwrap_or(false) {
                    scan.set(scan_rx.borrow_and_update().clone());
                }
            }
        }
    });

    // Update check and the notice from mayorana.ch: delayed so they never
    // compete with the first quotes, best-effort, silent on failure.
    let mut update_info = use_signal(|| Option::<update_check::UpdateInfo>::None);
    let mut update_dismissed = use_signal(|| false);
    use_future(move || async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        if let Some(info) = update_check::check().await {
            telemetry::record(telemetry::Event::UpdateOffered {
                to: info.latest_version.clone(),
            });
            update_info.set(Some(info));
        }
    });
    let mut mayorana_notice = use_signal(|| Option::<notice::Notice>::None);
    use_future(move || async move {
        tokio::time::sleep(Duration::from_secs(4)).await;
        if let Some(n) = notice::fetch().await {
            mayorana_notice.set(Some(n));
        }
    });

    // Anonymous usage statistics. On by default, but only once the person has
    // been told: `start` reads the opt-outs (including a shell profile's),
    // records the launch, and says whether the notice is still owed.
    let mut ask_consent = use_signal(|| false);
    use_future(move || async move {
        if telemetry::start().await {
            ask_consent.set(true);
        }
        telemetry::flush_forever().await;
    });
    // Recorded while the notice is on screen, so collection starts from the
    // next event — never from one the person had no chance to read about.
    use_effect(move || {
        if ask_consent() {
            telemetry::mark_informed();
        }
    });

    let m = snap.read();
    let s = &m.detail;
    let selected = m.selected.clone();
    let token = m
        .assets
        .iter()
        .find(|a| Some(&a.symbol) == selected.as_ref())
        .map(|a| {
            a.jupiter
                .as_ref()
                .map_or("Jupiter: no Solana token".to_owned(), |t| {
                    format!("Jupiter: {} ({}…)", t.name, &t.mint[..6])
                })
        });
    let fresh = scan.read().rows.iter().filter(|r| r.fresh).count();
    let current = tab();
    rsx! {
        div { class: "layout",
            Watchlist {
                assets: m.assets.clone(),
                selected: selected.clone(),
                adding: m.adding.clone(),
                notice: m.notice.clone(),
            }
            main { class: "app",
                if let (Some(info), false) = (update_info.read().clone(), update_dismissed()) {
                    div { class: "banner",
                        span { class: "banner-text",
                            "Spreadwatch "
                            strong { "{info.latest_version}" }
                            " is available (you have {env!(\"CARGO_PKG_VERSION\")})."
                        }
                        button {
                            class: "banner-link",
                            onclick: move |_| {
                                telemetry::record(telemetry::Event::UpdateClicked {
                                    to: info.latest_version.clone(),
                                });
                                let _ = open::that(&info.release_url);
                            },
                            "Download"
                        }
                        button { class: "banner-dismiss", onclick: move |_| update_dismissed.set(true), "×" }
                    }
                }
                if let Some(n) = mayorana_notice.read().clone() {
                    div { class: "banner notice",
                        span { class: "banner-text", "{n.text}" }
                        if let Some(url) = n.url.clone() {
                            button {
                                class: "banner-link",
                                onclick: move |_| {
                                    let _ = open::that(&url);
                                },
                                {n.link_text.clone().unwrap_or_else(|| "Open".into())}
                            }
                        }
                        button {
                            class: "banner-dismiss",
                            onclick: move |_| {
                                notice::dismiss(&n.id);
                                mayorana_notice.set(None);
                            },
                            "×"
                        }
                    }
                }
                // Usage-statistics notice, once, in the same place and style as
                // the other banners. Either button is remembered.
                if ask_consent() {
                    div { class: "banner notice",
                        span { class: "banner-text",
                            strong { "Spreadwatch shares anonymous usage statistics. " }
                            "Whether it is installed and opened, and its version and operating \
                             system — never your watchlist, trades, keys or anything you type."
                        }
                        button {
                            class: "banner-link",
                            onclick: move |_| {
                                telemetry::set_consent(true);
                                ask_consent.set(false);
                            },
                            "OK"
                        }
                        button {
                            class: "banner-link",
                            onclick: move |_| {
                                telemetry::set_consent(false);
                                ask_consent.set(false);
                            },
                            "Turn off"
                        }
                    }
                }
                nav { class: "tabs",
                    button {
                        class: if current == Tab::Market { "tab active" } else { "tab" },
                        onclick: move |_| tab.set(Tab::Market),
                        {selected.clone().unwrap_or_else(|| "Market".into())}
                    }
                    button {
                        class: if current == Tab::NewListings { "tab active" } else { "tab" },
                        onclick: move |_| tab.set(Tab::NewListings),
                        "New listings"
                        if fresh > 0 {
                            span { class: "tab-count", title: "Tokens that appeared since the app started", "{fresh}" }
                        }
                    }
                    // Which build is running, for bug reports and support.
                    span { class: "app-version", { concat!("v", env!("CARGO_PKG_VERSION")) } }
                }
                if current == Tab::NewListings {
                    NewListingsPanel {
                        scan: scan.read().clone(),
                        watched: m.assets.iter().map(|a| a.symbol.clone()).collect::<Vec<_>>(),
                    }
                } else {
                match selected.as_deref() {
                    None => rsx! {
                        div { class: "card empty",
                            h2 { "Nothing followed" }
                            p { class: "muted", "Add a crypto on the left, e.g. BTC, ETH or JUP." }
                        }
                    },
                    Some(symbol) => rsx! {
                        header { class: "top",
                            h1 { "{symbol} / USDT" }
                            span { class: "muted", "5 exchanges + Jupiter on-chain · net edges pay the taker fee on both legs" }
                            if let Some(token) = token.as_ref() {
                                span { class: "muted small", "{token}" }
                            }
                        }
                        section { class: "venues",
                            for v in s.venues.iter() {
                                VenueCard { key: "{v.venue.name()}", view: v.clone() }
                            }
                        }
                        section { class: "middle",
                            RoutesPanel { routes: s.routes.clone() }
                            LagPanel { venues: s.venues.clone(), pairs: s.lag_pairs.clone(), samples: s.lag_samples }
                        }
                        if symbol == TRADABLE {
                            TradePanel { state: trade.read().clone() }
                        } else {
                            div { class: "card muted small", "On-chain trading covers {TRADABLE}/USDT only for now." }
                        }
                        EventLog { events: s.events.clone() }
                    },
                }
                }
            }
        }
    }
}
