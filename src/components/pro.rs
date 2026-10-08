//! The Spreadwatch Pro window: what the free version does, the licence on this
//! computer, and where to paste a key.

use dioxus::prelude::*;

use crate::licence::{self, Status, FREE_ASSETS};

/// Shared by every part of the UI that can open the window or needs to know
/// whether the app is licensed. Provided once, by `App`.
#[derive(Clone, Copy)]
pub struct Pro {
    pub open: Signal<bool>,
    pub status: Signal<Status>,
}

/// "Get Pro…" while free, "✦ PRO" once licensed; opens the window either way.
/// Nothing at all in a build that can't check licences.
#[component]
pub fn ProButton() -> Element {
    let mut pro = use_context::<Pro>();
    let status = pro.status.read().clone();
    let (label, class, title) = match &status {
        Status::Unavailable => return rsx! {},
        Status::Pro(l) => (
            "✦ PRO",
            "pro-btn active",
            format!("Spreadwatch Pro, licensed to {}", l.email),
        ),
        Status::Renew(_) => (
            "✦ RENEW PRO",
            "pro-btn renew",
            "Your Pro licence doesn't cover this version".to_owned(),
        ),
        Status::Free => (
            "Get Pro…",
            "pro-btn",
            format!("The free version follows {FREE_ASSETS} assets; Pro follows any number"),
        ),
    };
    rsx! {
        button { class, title, onclick: move |_| pro.open.set(true), "{label}" }
    }
}

#[component]
pub fn ProWindow() -> Element {
    let mut pro = use_context::<Pro>();
    let mut key = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    if !(pro.open)() {
        return rsx! {};
    }
    let status = pro.status.read().clone();
    let mut close = move || {
        pro.open.set(false);
        error.set(None);
    };
    let mut activate = move || {
        let pasted = key.read().clone();
        match licence::activate(&pasted) {
            Ok(s) => {
                pro.status.set(s);
                key.set(String::new());
                error.set(None);
            }
            Err(e) => error.set(Some(e)),
        }
    };

    rsx! {
        div { class: "modal-backdrop", onclick: move |_| close(),
            div {
                class: "modal card",
                // Clicks inside the window don't close it.
                onclick: move |e| e.stop_propagation(),
                h2 { "Spreadwatch Pro" }
                match &status {
                    Status::Free => rsx! {
                        p {
                            "The free version follows {FREE_ASSETS} assets at once, with every tab, every venue and trading. "
                            "Pro follows any number of assets, and is the licence for commercial use."
                        }
                    },
                    Status::Pro(l) => rsx! {
                        p { class: "pro-ok",
                            "Pro is active on this computer, licensed to "
                            strong { "{l.email}" }
                            ". Updates included until {l.updates_until}."
                        }
                    },
                    Status::Renew(l) => rsx! {
                        p {
                            "Your licence ("
                            strong { "{l.email}" }
                            ") covers versions released until {l.updates_until}. This version is newer, so it runs as the free version: renew Pro to use it here, or keep using a version from before that date."
                        }
                    },
                    Status::Unavailable => rsx! {
                        p { "This build of Spreadwatch can't check licences, so nothing is limited. Builds from mayorana.ch can." }
                    },
                }
                if matches!(status, Status::Free | Status::Renew(_)) {
                    button {
                        class: "btn buy",
                        onclick: move |_| {
                            let _ = open::that(licence::BUY_URL);
                        },
                        if matches!(status, Status::Renew(_)) { "Renew Spreadwatch Pro" } else { "Buy Spreadwatch Pro" }
                    }
                    p { class: "muted small", "Already bought it? Paste the licence key from your email:" }
                    textarea {
                        class: "licence-key",
                        rows: 4,
                        placeholder: "Licence key",
                        value: "{key}",
                        oninput: move |e| key.set(e.value()),
                    }
                    if let Some(e) = error.read().as_ref() {
                        p { class: "error small", "{e}" }
                    }
                    p { class: "muted small",
                        "The key is checked on this computer: no account, nothing sent anywhere."
                    }
                }
                div { class: "modal-actions",
                    if matches!(status, Status::Free | Status::Renew(_)) {
                        button {
                            class: "btn",
                            disabled: key.read().trim().is_empty(),
                            onclick: move |_| activate(),
                            "Activate"
                        }
                    }
                    if matches!(status, Status::Pro(_) | Status::Renew(_)) {
                        button {
                            class: "btn ghost",
                            title: "To move the licence to another computer",
                            onclick: move |_| {
                                licence::deactivate();
                                pro.status.set(licence::current());
                            },
                            "Remove the licence from this computer"
                        }
                    }
                    button { class: "btn ghost", onclick: move |_| close(), "Close" }
                }
            }
        }
    }
}
