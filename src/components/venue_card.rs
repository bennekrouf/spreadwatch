use dioxus::prelude::*;
use spread_core::model::{ConnState, VenueView};

use super::{fmt_bps_pct, fmt_px};

#[component]
pub fn VenueCard(view: VenueView) -> Element {
    let (dot, status) = match (view.conn, view.stale) {
        _ if view.unlisted && view.venue.is_polled() => ("bad", "no Solana token"),
        _ if view.unlisted => ("bad", "not listed"),
        (ConnState::Connected, false) => ("ok", "live"),
        (ConnState::Connected, true) => ("warn", "late"),
        (ConnState::Connecting, _) => ("warn", "connecting"),
        (ConnState::Disconnected, _) => ("bad", "disconnected"),
    };
    let age = view.age_ms.map_or("–".into(), |ms| format!("{ms} ms"));
    let spread = view.spread_bps.map(f64::abs);
    let polled = view.venue.is_polled();
    let lag = match view.lag_ms {
        _ if polled => "n/a (polled)".into(),
        Some(ms) => format!("{ms:+.0} ms"),
        None => "–".into(),
    };
    let network = match view.net_latency_ms {
        Some(ms) => format!("{ms:.0} ms"),
        None if polled => "n/a (polled)".into(),
        None => "no timestamp".into(),
    };
    // One tick is under 1 bps at current prices; flag only clear outliers.
    let dev_class = match view.dev_bps {
        Some(d) if d.abs() >= 2.0 => "dev-far",
        _ => "",
    };

    let card_class = if view.unlisted {
        "card venue unlisted"
    } else if view.stale {
        "card venue stale"
    } else {
        "card venue"
    };

    let fee = format!("{:.0} bps ({:.2} %)", view.fee_bps, view.fee_bps / 100.0);

    rsx! {
        div { class: card_class,
            div { class: "venue-head",
                h2 { "{view.venue.name()}" }
                span { class: "status",
                    span { class: "dot {dot}" }
                    "{status}"
                }
            }
            div { class: "book",
                div { class: "side",
                    span { class: "label", "Bid" }
                    span { class: "px bid", "{fmt_px(view.bid)}" }
                }
                div { class: "side",
                    span { class: "label", "Ask" }
                    span { class: "px ask", "{fmt_px(view.ask)}" }
                }
            }
            dl { class: "stats",
                dt { "Spread" }
                dd { "{fmt_bps_pct(spread)}" }
                dt { "Last quote" }
                dd { "{age}" }
                dt { "Rate" }
                dd { "{view.rate}/s" }
                dt { "vs median" }
                dd { class: dev_class, "{fmt_bps_pct(view.dev_bps)}" }
                dt { "Lag" }
                dd { "{lag}" }
                dt { title: "Local receive time minus the exchange's timestamp", "Network" }
                dd { "{network}" }
                dt { "Taker fee" }
                dd { "{fee}" }
            }
            if let Some(ms) = view.poll_every_ms {
                p { class: "muted small note",
                    "Executable quote for $100 of USDT, refreshed every {ms as f64 / 1000.0:.1} s"
                }
            }
        }
    }
}
