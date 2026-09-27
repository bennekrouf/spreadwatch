use dioxus::prelude::*;
use spread_core::model::Route;

use super::fmt_bps_pct;

/// With four venues there are twelve routes; the rest are rarely interesting.
const SHOWN: usize = 5;

#[component]
pub fn RoutesPanel(routes: Vec<Route>) -> Element {
    rsx! {
        div { class: "card routes",
            h3 { "Best cross-venue edges" }
            p { class: "muted small", "In bps of the mid price: 1 bp = 0.01 %, about $0.012 per SOL at $120." }
            if routes.len() < 2 {
                p { class: "muted", "Waiting for fresh quotes from at least two venues…" }
            }
            for r in routes.iter().take(SHOWN) {
                div { class: "route",
                    span { class: "route-name", "Buy {r.buy.name()} → sell {r.sell.name()}" }
                    span { class: if r.gross_bps > 0.0 { "num pos" } else { "num" },
                        "{fmt_bps_pct(Some(r.gross_bps))} gross"
                    }
                    span { class: if r.net_bps > 0.0 { "num pos strong" } else { "num neg" },
                        "{fmt_bps_pct(Some(r.net_bps))} net"
                    }
                    span { class: "num muted", "best {fmt_bps_pct(Some(r.best_gross_bps))}" }
                }
            }
            if routes.len() > SHOWN {
                p { class: "muted small", "{routes.len() - SHOWN} more routes below zero edge or smaller" }
            }
        }
    }
}
