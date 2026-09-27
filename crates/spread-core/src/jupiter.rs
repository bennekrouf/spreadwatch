//! Shared access to Jupiter's HTTP API: which host, the API key, and one
//! request budget for the price feed and the trade module together.
//!
//! Jupiter limits requests per account over a 60 s sliding window: 30 a
//! minute keyless, 60 with a free key, more on paid plans. Going over gets
//! the IP blocked for minutes, so both users draw from one budget here and
//! the feed always leaves room for trades.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

pub const SOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const USDT_MINT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
pub const LAMPORTS_PER_SOL: f64 = 1e9;
pub const USDT_UNITS: f64 = 1e6;

const KEYLESS_BASE: &str = "https://lite-api.jup.ag";
const KEYED_BASE: &str = "https://api.jup.ag";
/// Documented plan limits, requests per 60 s sliding window.
pub const KEYLESS_RPM: u32 = 30;
pub const FREE_KEY_RPM: u32 = 60;
const WINDOW: Duration = Duration::from_secs(60);
/// Requests per window the feed leaves unused: a trade is one quote and one
/// swap build, so this is five trades a minute.
pub const TRADE_RESERVE: u32 = 10;
/// Each feed poll is two quotes, buy and sell.
pub const REQUESTS_PER_POLL: u32 = 2;
const MIN_POLL: Duration = Duration::from_secs(1);

pub struct JupiterApi {
    base: &'static str,
    key: Option<String>,
    rpm: u32,
    /// Send times inside the current window, oldest first.
    sent: Mutex<VecDeque<Instant>>,
}

impl JupiterApi {
    /// `rpm` overrides the plan default: set it for a paid plan.
    pub fn new(key: Option<String>, rpm: Option<u32>) -> Arc<Self> {
        let key = key.map(|k| k.trim().to_owned()).filter(|k| !k.is_empty());
        let default_rpm = if key.is_some() {
            FREE_KEY_RPM
        } else {
            KEYLESS_RPM
        };
        Arc::new(Self {
            base: if key.is_some() {
                KEYED_BASE
            } else {
                KEYLESS_BASE
            },
            rpm: rpm.unwrap_or(default_rpm).max(1),
            key,
            sent: Mutex::new(VecDeque::new()),
        })
    }

    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    pub fn rpm(&self) -> u32 {
        self.rpm
    }

    /// For the UI; never includes the key.
    pub fn describe(&self) -> String {
        let access = if self.has_key() { "API key" } else { "keyless" };
        format!("{access} · {} req/min", self.rpm)
    }

    /// Poll interval that keeps the feed inside its share of the budget.
    pub fn feed_poll_every(&self) -> Duration {
        let feed_rpm = self
            .rpm
            .saturating_sub(TRADE_RESERVE)
            .max(REQUESTS_PER_POLL);
        let polls_per_min = f64::from(feed_rpm) / f64::from(REQUESTS_PER_POLL);
        Duration::from_secs_f64(60.0 / polls_per_min).max(MIN_POLL)
    }

    pub fn get(&self, client: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        self.auth(client.get(format!("{}{path}", self.base)))
    }

    pub fn post(&self, client: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        self.auth(client.post(format!("{}{path}", self.base)))
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.key {
            Some(key) => rb.header("x-api-key", key),
            None => rb,
        }
    }

    /// Feed side: takes `n` requests only if `keep_free` stay available
    /// afterwards. A poll that doesn't fit is skipped, not delayed.
    pub fn try_take(&self, n: u32, keep_free: u32) -> bool {
        let now = Instant::now();
        let mut sent = self.sent.lock().expect("budget lock");
        prune(&mut sent, now);
        if sent.len() as u32 + n + keep_free > self.rpm {
            return false;
        }
        sent.extend(std::iter::repeat_n(now, n as usize));
        true
    }

    /// Trade side: waits until `n` requests fit in the window.
    pub async fn take(&self, n: u32) {
        loop {
            let wait = {
                let now = Instant::now();
                let mut sent = self.sent.lock().expect("budget lock");
                prune(&mut sent, now);
                if sent.len() as u32 + n <= self.rpm {
                    sent.extend(std::iter::repeat_n(now, n as usize));
                    return;
                }
                // The oldest send leaves the window first.
                (sent[0] + WINDOW).saturating_duration_since(now)
            };
            tokio::time::sleep(wait.max(Duration::from_millis(10))).await;
        }
    }

    /// Requests counted in the last 60 s.
    pub fn used(&self) -> u32 {
        let mut sent = self.sent.lock().expect("budget lock");
        prune(&mut sent, Instant::now());
        sent.len() as u32
    }
}

fn prune(sent: &mut VecDeque<Instant>, now: Instant) {
    while sent
        .front()
        .is_some_and(|&t| now.duration_since(t) >= WINDOW)
    {
        sent.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_picks_host_and_limit() {
        let keyless = JupiterApi::new(None, None);
        assert_eq!((keyless.base, keyless.rpm()), (KEYLESS_BASE, 30));
        // Blank keys from an empty setting count as no key.
        assert!(!JupiterApi::new(Some("  ".into()), None).has_key());

        let keyed = JupiterApi::new(Some("k".into()), None);
        assert_eq!((keyed.base, keyed.rpm()), (KEYED_BASE, 60));
        assert_eq!(JupiterApi::new(Some("k".into()), Some(600)).rpm(), 600);

        let described = JupiterApi::new(Some("secret-123".into()), None).describe();
        assert_eq!(described, "API key · 60 req/min");
    }

    #[test]
    fn feed_polls_within_its_share() {
        // Keyless: 30 - 10 reserve = 20 req/min = 10 polls/min.
        assert_eq!(
            JupiterApi::new(None, None).feed_poll_every(),
            Duration::from_secs(6)
        );
        // Free key: 50 req/min = 25 polls/min.
        assert_eq!(
            JupiterApi::new(Some("k".into()), None).feed_poll_every(),
            Duration::from_millis(2400)
        );
        // Paid plans are capped at one poll a second.
        assert_eq!(
            JupiterApi::new(Some("k".into()), Some(600)).feed_poll_every(),
            MIN_POLL
        );
    }

    #[tokio::test(start_paused = true)]
    async fn feed_leaves_the_reserve_for_trades() {
        let api = JupiterApi::new(None, None);
        let mut polls = 0;
        while api.try_take(REQUESTS_PER_POLL, TRADE_RESERVE) {
            polls += 1;
        }
        assert_eq!(polls, 10);
        assert_eq!(api.used(), 20);

        // A trade still fits at once.
        let started = Instant::now();
        api.take(2).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn trade_waits_until_the_oldest_requests_leave_the_window() {
        let api = JupiterApi::new(None, None);
        api.take(20).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
        api.take(10).await;
        assert_eq!(api.used(), 30);

        // Full: the next request waits until the first 20 age out, 30 s later.
        let started = Instant::now();
        api.take(1).await;
        assert_eq!(started.elapsed(), Duration::from_secs(30));
        assert_eq!(api.used(), 11);
        // The feed still has to leave the reserve free: 11 + 2 + 10 fits in 30.
        assert!(api.try_take(REQUESTS_PER_POLL, TRADE_RESERVE));
    }
}
