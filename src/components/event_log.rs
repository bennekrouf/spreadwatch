use dioxus::prelude::*;
use spread_core::model::{Event, EventKind};

#[component]
pub fn EventLog(
    #[props(into, default = "Events".to_string())] title: String,
    events: Vec<Event>,
) -> Element {
    rsx! {
        div { class: "card events",
            h3 { "{title}" }
            if events.is_empty() {
                p { class: "muted", "Nothing yet." }
            }
            ul {
                for (i, ev) in events.iter().enumerate() {
                    li { key: "{events.len() - i}",
                        class: match ev.kind {
                            EventKind::Opportunity => "ev opp",
                            EventKind::Stale => "ev stale",
                            EventKind::Connection => "ev conn",
                            EventKind::Listing => "ev listing",
                        },
                        span { class: "ev-time", "{ev.at}" }
                        span { "{ev.text}" }
                    }
                }
            }
        }
    }
}
