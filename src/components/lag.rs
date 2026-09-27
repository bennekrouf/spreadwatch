use dioxus::prelude::*;
use spread_core::model::{PairLag, VenueView};

/// Bar width for the most extreme lag; the others scale to it.
const BAR_MAX_PCT: f64 = 50.0;

fn fmt_ms(v: Option<f64>) -> String {
    v.map_or("–".into(), |ms| format!("{ms:.0} ms"))
}

#[component]
pub fn LagPanel(venues: Vec<VenueView>, pairs: Vec<PairLag>, samples: usize) -> Element {
    let mut ranked: Vec<&VenueView> = venues.iter().filter(|v| v.lag_ms.is_some()).collect();
    ranked.sort_by(|a, b| b.lag_ms.unwrap_or(0.0).total_cmp(&a.lag_ms.unwrap_or(0.0)));
    let scale = ranked
        .iter()
        .filter_map(|v| v.lag_ms)
        .map(f64::abs)
        .fold(1.0, f64::max);
    let reliable = pairs.iter().filter(|p| p.reliable).count();

    rsx! {
        div { class: "card lag",
            h3 { "Who is late" }
            if ranked.is_empty() {
                p { class: "muted", "Collecting price moves… needs a few moves of 3 bps or more." }
            } else {
                p { class: "lag-headline",
                    strong { "{ranked[0].venue.name()}" }
                    " trails the others by ~"
                    strong { "{ranked[0].lag_ms.unwrap_or(0.0):.0} ms" }
                    " on average"
                }
                div { class: "lag-row lag-legend muted small",
                    span {}
                    span { "← leads · follows →" }
                    span { class: "num", "lag" }
                    span { class: "num", title: "Median time for a message to reach this machine", "network" }
                }
                for v in ranked.iter() {
                    LagRow { key: "{v.venue.name()}", view: (*v).clone(), scale }
                }
                p { class: "muted small",
                    "{samples} matched moves · network = local receive time − exchange timestamp, "
                    "including clock skew. A lag that mirrors the network gap is distance, not the exchange."
                }
            }
            if !pairs.is_empty() {
                details { class: "pairs",
                    summary { class: "muted small", "Per pair ({reliable} of {pairs.len()} with enough moves)" }
                    table {
                        for p in pairs.iter() {
                            tr { key: "{p.leader.name()}-{p.follower.name()}",
                                class: if p.reliable { "" } else { "unreliable" },
                                td { "{p.follower.name()}" }
                                td { class: "muted", "follows" }
                                td { "{p.leader.name()}" }
                                td { class: "num", "+{p.median_ms:.0} ms" }
                                td { class: "num muted", "n={p.samples}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn LagRow(view: VenueView, scale: f64) -> Element {
    let ms = view.lag_ms.unwrap_or(0.0);
    let (class, style) = if ms > 0.0 {
        (
            "lag-bar late",
            format!("left: 50%; width: {:.1}%", ms / scale * BAR_MAX_PCT),
        )
    } else {
        (
            "lag-bar early",
            format!("right: 50%; width: {:.1}%", -ms / scale * BAR_MAX_PCT),
        )
    };
    rsx! {
        div { class: "lag-row",
            span { class: "lag-name", "{view.venue.name()}" }
            div { class: "lag-track", div { class, style } }
            span { class: "num", "{ms:+.0} ms" }
            span { class: "num muted", "{fmt_ms(view.net_latency_ms)}" }
        }
    }
}
