//! Holds the latest quote per venue and derives everything the UI shows:
//! cross-venue edges, stale feeds, and which venues lag the others.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::model::{
    ConnState, Event, EventKind, FeedEvent, PairLag, Quote, Route, Snapshot, Venue, VenueView,
};

const N: usize = Venue::COUNT;
const MAX_EVENTS: usize = 200;
/// A venue pair with fewer matched moves than this is noise, not a lag estimate.
const MIN_LAG_SAMPLES: usize = 5;
/// Quotes behind each venue's network latency median.
const LATENCY_WINDOW: usize = 200;
/// Connected this long without a single quote: the venue doesn't list the asset.
const UNLISTED_AFTER: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    /// A venue with no quote for this long is stale and left out of routes.
    pub stale_after: Duration,
    /// How often polled venues (Jupiter) are fetched. They count as stale
    /// after missing a couple of polls rather than after `stale_after`.
    pub poll_every: Duration,
    /// A mid move of at least this many bps counts as a "move" for lead/lag.
    pub move_bps: f64,
    /// How long other venues have to follow a move before it is dropped.
    pub follow_window: Duration,
    /// Taker fee per leg, indexed by `Venue::idx`. A route pays both legs.
    pub taker_fee_bps: [f64; N],
    /// Rolling window of matched moves per venue pair.
    pub lag_samples: usize,
    /// How often a snapshot is published to the UI.
    pub publish_every: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            // Bybit depth-1 re-sends a snapshot every 3s when the book is quiet.
            stale_after: Duration::from_secs(5),
            poll_every: Duration::from_secs(6),
            move_bps: 3.0,
            follow_window: Duration::from_secs(2),
            // Regular-tier spot taker fees; check yours, VIP tiers pay less.
            // Jupiter quotes already include pool fees; the network fee on a
            // 1 SOL swap is a fraction of a bp.
            taker_fee_bps: [
                10.0, // Binance
                10.0, // Bybit
                5.0,  // MEXC
                10.0, // Gate
                10.0, // OKX
                0.0,  // Jupiter
            ],
            lag_samples: 50,
            publish_every: Duration::from_millis(50),
        }
    }
}

#[derive(Default)]
struct VenueState {
    conn: ConnState,
    connected_at: Option<Instant>,
    /// Known not to quote this asset (Jupiter without a Solana token).
    absent: bool,
    last: Option<Quote>,
    /// Mid at the last detected move; the next move is measured from here.
    anchor_mid: Option<f64>,
    was_stale: bool,
    rate_count: u32,
    rate: u32,
    latencies: VecDeque<f64>,
}

#[derive(Debug, Clone, Copy)]
struct Move {
    venue: Venue,
    up: bool,
    at: Instant,
    /// Venues that already followed this move; each is counted once.
    followed: [bool; N],
}

pub struct Engine {
    cfg: EngineConfig,
    venues: [VenueState; N],
    /// Recent moves that other venues may still follow.
    pending: VecDeque<Move>,
    /// Signed follow delays in ms per venue pair, stored at `[i][j]` with
    /// i < j. Positive: venue j followed venue i.
    pair_samples: [[VecDeque<f64>; N]; N],
    /// Per route `[buy][sell]`: best gross edge seen and whether it is open.
    best_gross: [[f64; N]; N],
    open: [[bool; N]; N],
    events: VecDeque<Event>,
    rate_window: Option<Instant>,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Self {
        Self {
            cfg,
            venues: Default::default(),
            pending: VecDeque::new(),
            pair_samples: Default::default(),
            best_gross: [[f64::NEG_INFINITY; N]; N],
            open: [[false; N]; N],
            events: VecDeque::new(),
            rate_window: None,
        }
    }

    pub fn on_event(&mut self, ev: FeedEvent) {
        match ev {
            FeedEvent::Quote(q) => self.on_quote(q),
            FeedEvent::Status(venue, conn) => self.on_status(venue, conn),
        }
    }

    fn on_status(&mut self, venue: Venue, conn: ConnState) {
        let st = &mut self.venues[venue.idx()];
        if st.conn == conn {
            return;
        }
        st.conn = conn;
        st.connected_at = (conn == ConnState::Connected).then(Instant::now);
        // A reconnect starts a fresh book; don't measure moves across the gap.
        st.anchor_mid = None;
        let text = match conn {
            ConnState::Connecting => return,
            ConnState::Connected => format!("{} connected", venue.name()),
            ConnState::Disconnected => format!("{} disconnected, reconnecting", venue.name()),
        };
        self.push_event(EventKind::Connection, text);
    }

    fn on_quote(&mut self, q: Quote) {
        let st = &mut self.venues[q.venue.idx()];
        st.last = Some(q);
        st.rate_count += 1;
        if let Some(ms) = q.latency_ms {
            st.latencies.push_back(ms);
            if st.latencies.len() > LATENCY_WINDOW {
                st.latencies.pop_front();
            }
        }

        if q.venue.is_polled() {
            self.update_routes(q.recv_at);
            return;
        }
        let mid = q.mid();
        let anchor = *st.anchor_mid.get_or_insert(mid);
        let moved_bps = (mid - anchor) / anchor * 1e4;
        if moved_bps.abs() >= self.cfg.move_bps {
            st.anchor_mid = Some(mid);
            self.on_move(Move {
                venue: q.venue,
                up: moved_bps > 0.0,
                at: q.recv_at,
                followed: [false; N],
            });
        }

        self.update_routes(q.recv_at);
    }

    fn on_move(&mut self, m: Move) {
        let window = self.cfg.follow_window;
        self.pending.retain(|p| m.at.duration_since(p.at) <= window);

        // A venue that already has an open move this way is continuing its own
        // move, not following anyone. Counting it would record a leader as
        // following the venues that followed it.
        if self
            .pending
            .iter()
            .any(|p| p.venue == m.venue && p.up == m.up)
        {
            return;
        }

        let x = m.venue.idx();
        for p in self.pending.iter_mut() {
            let y = p.venue.idx();
            if p.up != m.up || p.followed[x] {
                continue;
            }
            p.followed[x] = true;
            // y led, x followed.
            let ms = m.at.duration_since(p.at).as_secs_f64() * 1e3;
            let (lo, hi, signed) = if y < x { (y, x, ms) } else { (x, y, -ms) };
            let samples = &mut self.pair_samples[lo][hi];
            samples.push_back(signed);
            if samples.len() > self.cfg.lag_samples {
                samples.pop_front();
            }
        }
        self.pending.push_back(m);
    }

    /// Opens and closes opportunity events as net edge crosses zero.
    fn update_routes(&mut self, now: Instant) {
        for route in self.routes(now) {
            let (b, s) = (route.buy.idx(), route.sell.idx());
            self.best_gross[b][s] = self.best_gross[b][s].max(route.gross_bps);
            let open = route.net_bps > 0.0;
            if open && !self.open[b][s] {
                let text = format!(
                    "Buy {} / sell {}: {:+.2} bps ({:+.4} %) gross, {:+.2} bps ({:+.4} %) net",
                    route.buy.name(),
                    route.sell.name(),
                    route.gross_bps,
                    route.gross_bps / 100.0,
                    route.net_bps,
                    route.net_bps / 100.0
                );
                self.push_event(EventKind::Opportunity, text);
            }
            self.open[b][s] = open;
        }
    }

    /// Marks a venue that can't quote this asset, so it isn't waited for.
    pub fn set_absent(&mut self, venue: Venue, absent: bool) {
        self.venues[venue.idx()].absent = absent;
    }

    /// Logs a change in how another exchange lists this asset.
    pub fn note_listing(&mut self, text: String) {
        self.push_event(EventKind::Listing, text);
    }

    pub fn set_poll_every(&mut self, every: Duration) {
        self.cfg.poll_every = every;
    }

    fn is_unlisted(&self, venue: Venue, now: Instant) -> bool {
        let st = &self.venues[venue.idx()];
        st.absent
            || (st.conn == ConnState::Connected
                && st.last.is_none()
                && st
                    .connected_at
                    .is_some_and(|t| now.saturating_duration_since(t) >= UNLISTED_AFTER))
    }

    /// Watchlist line: consensus mid, best gross edge, fresh and listed venues.
    pub fn summary(&self, now: Instant) -> (Option<f64>, Option<f64>, usize, usize) {
        let fresh = Venue::ALL
            .iter()
            .filter(|&&v| self.is_fresh(v, now))
            .count();
        let listed = Venue::ALL
            .iter()
            .filter(|&&v| !self.is_unlisted(v, now))
            .count();
        let best = self.routes(now).first().map(|r| r.gross_bps);
        (self.consensus_mid(now), best, fresh, listed)
    }

    /// Periodic housekeeping: stale transitions and message rates.
    pub fn tick(&mut self, now: Instant) {
        let window = *self.rate_window.get_or_insert(now);
        if now.duration_since(window) >= Duration::from_secs(1) {
            for st in &mut self.venues {
                st.rate = st.rate_count;
                st.rate_count = 0;
            }
            self.rate_window = Some(now);
        }

        for venue in Venue::ALL {
            let stale = !self.is_fresh(venue, now);
            let st = &mut self.venues[venue.idx()];
            // Before the first quote a venue is just starting, not late.
            if st.last.is_none() || stale == st.was_stale {
                continue;
            }
            st.was_stale = stale;
            let text = if stale {
                format!(
                    "{} is late: no quote for {:.1}s",
                    venue.name(),
                    self.stale_after(venue).as_secs_f64()
                )
            } else {
                format!("{} quoting again", venue.name())
            };
            self.push_event(EventKind::Stale, text);
        }
    }

    pub fn snapshot(&self, now: Instant) -> Snapshot {
        let consensus = self.consensus_mid(now);
        let lags = self.lags();
        let venues = Venue::ALL
            .iter()
            .map(|&venue| {
                let st = &self.venues[venue.idx()];
                let mut view = VenueView::new(venue);
                view.conn = st.conn;
                view.rate = st.rate;
                view.stale = !self.is_fresh(venue, now);
                view.lag_ms = lags[venue.idx()];
                view.fee_bps = self.cfg.taker_fee_bps[venue.idx()];
                view.net_latency_ms = median(st.latencies.iter().copied());
                view.poll_every_ms = venue
                    .is_polled()
                    .then_some(self.cfg.poll_every.as_millis() as u64);
                view.unlisted = self.is_unlisted(venue, now);
                if let Some(q) = st.last {
                    view.bid = Some(q.bid);
                    view.ask = Some(q.ask);
                    view.spread_bps = Some((q.ask - q.bid) / q.mid() * 1e4);
                    view.age_ms = Some(now.saturating_duration_since(q.recv_at).as_millis() as u64);
                    if !view.stale {
                        view.dev_bps = consensus.map(|c| (q.mid() - c) / c * 1e4);
                    }
                }
                view
            })
            .collect();

        let mut routes = self.routes(now);
        for r in &mut routes {
            r.best_gross_bps = self.best_gross[r.buy.idx()][r.sell.idx()];
        }

        Snapshot {
            venues,
            routes,
            lag_samples: self.pair_samples.iter().flatten().map(VecDeque::len).sum(),
            lag_pairs: self.lag_pairs(),
            events: self.events.iter().cloned().collect(),
        }
    }

    fn stale_after(&self, venue: Venue) -> Duration {
        if venue.is_polled() {
            (self.cfg.poll_every * 5 / 2).max(self.cfg.stale_after)
        } else {
            self.cfg.stale_after
        }
    }

    fn is_fresh(&self, venue: Venue, now: Instant) -> bool {
        let st = &self.venues[venue.idx()];
        let limit = self.stale_after(venue);
        !st.absent
            && st.conn == ConnState::Connected
            && st
                .last
                .is_some_and(|q| now.saturating_duration_since(q.recv_at) < limit)
    }

    fn fresh_quotes(&self, now: Instant) -> Vec<Quote> {
        Venue::ALL
            .iter()
            .filter(|&&v| self.is_fresh(v, now))
            .filter_map(|&v| self.venues[v.idx()].last)
            .collect()
    }

    /// Median mid across fresh venues: one venue off on its own can't drag it.
    fn consensus_mid(&self, now: Instant) -> Option<f64> {
        let mut mids: Vec<f64> = self.fresh_quotes(now).iter().map(Quote::mid).collect();
        if mids.len() < 2 {
            return None;
        }
        mids.sort_by(f64::total_cmp);
        let n = mids.len();
        Some(if n % 2 == 1 {
            mids[n / 2]
        } else {
            (mids[n / 2 - 1] + mids[n / 2]) / 2.0
        })
    }

    /// Every ordered pair of fresh venues, best gross edge first.
    fn routes(&self, now: Instant) -> Vec<Route> {
        let fresh = self.fresh_quotes(now);
        let mut routes = Vec::new();
        for buy in &fresh {
            for sell in &fresh {
                if buy.venue == sell.venue {
                    continue;
                }
                let mid = (buy.mid() + sell.mid()) / 2.0;
                let gross_bps = (sell.bid - buy.ask) / mid * 1e4;
                routes.push(Route {
                    buy: buy.venue,
                    sell: sell.venue,
                    gross_bps,
                    net_bps: gross_bps
                        - self.cfg.taker_fee_bps[buy.venue.idx()]
                        - self.cfg.taker_fee_bps[sell.venue.idx()],
                    best_gross_bps: gross_bps,
                });
            }
        }
        routes.sort_by(|a, b| b.gross_bps.total_cmp(&a.gross_bps));
        routes
    }

    /// How far venue `i` trails venue `j` (negative: it leads), with the
    /// sample count. Stored at `[min][max]`, positive when the higher index
    /// followed, so it is flipped when `i` is the lower index.
    fn pair_median(&self, i: usize, j: usize) -> Option<(f64, usize)> {
        let s = &self.pair_samples[i.min(j)][i.max(j)];
        let m = median(s.iter().copied())?;
        Some((if i > j { m } else { -m }, s.len()))
    }

    /// Per venue: mean over the other venues of the pairwise median follow
    /// delay, from this venue's side. Positive means it tends to be late.
    fn lags(&self) -> [Option<f64>; N] {
        std::array::from_fn(|v| {
            let behind: Vec<f64> = (0..N)
                .filter(|&u| u != v)
                .filter_map(|u| self.pair_median(v, u))
                .filter(|&(_, n)| n >= MIN_LAG_SAMPLES)
                .map(|(m, _)| m)
                .collect();
            (!behind.is_empty()).then(|| behind.iter().sum::<f64>() / behind.len() as f64)
        })
    }

    fn lag_pairs(&self) -> Vec<PairLag> {
        let mut pairs = Vec::new();
        for i in 0..N {
            for j in i + 1..N {
                // Positive: i trails j.
                let Some((m, samples)) = self.pair_median(i, j) else {
                    continue;
                };
                let (leader, follower) = if m >= 0.0 { (j, i) } else { (i, j) };
                pairs.push(PairLag {
                    leader: Venue::ALL[leader],
                    follower: Venue::ALL[follower],
                    median_ms: m.abs(),
                    samples,
                    reliable: samples >= MIN_LAG_SAMPLES,
                });
            }
        }
        pairs.sort_by(|a, b| b.median_ms.total_cmp(&a.median_ms));
        pairs
    }

    fn push_event(&mut self, kind: EventKind, text: String) {
        tracing::info!("{text}");
        self.events.push_front(Event::now(kind, text));
        self.events.truncate(MAX_EVENTS);
    }
}

fn median(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = values.collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
        let mut e = Engine::new(EngineConfig::default());
        for v in Venue::ALL {
            e.on_event(FeedEvent::Status(v, ConnState::Connected));
        }
        e
    }

    fn quote(e: &mut Engine, venue: Venue, px: f64, at: Instant) {
        e.on_event(FeedEvent::Quote(Quote {
            venue,
            bid: px,
            ask: px + 0.01,
            recv_at: at,
            latency_ms: None,
        }));
    }

    #[test]
    fn crossed_books_give_positive_edge() {
        let mut e = engine();
        let t = Instant::now();
        quote(&mut e, Venue::Binance, 100.00, t);
        quote(&mut e, Venue::Bybit, 100.50, t);

        let snap = e.snapshot(t);
        let best = &snap.routes[0];
        assert_eq!((best.buy, best.sell), (Venue::Binance, Venue::Bybit));
        // (100.50 - 100.01) / 100.255 ≈ 48.9 bps gross, minus 20 bps fees.
        assert!((best.gross_bps - 48.88).abs() < 0.05);
        assert!(best.net_bps > 28.0);
        assert!(snap
            .events
            .iter()
            .any(|ev| ev.kind == EventKind::Opportunity));
    }

    #[test]
    fn routes_cover_every_ordered_pair_of_fresh_venues() {
        let mut e = engine();
        let t = Instant::now();
        for v in Venue::ALL {
            quote(&mut e, v, 100.0, t);
        }
        assert_eq!(e.snapshot(t).routes.len(), N * (N - 1));
    }

    #[test]
    fn deviation_is_measured_against_the_median() {
        let mut e = engine();
        let t = Instant::now();
        quote(&mut e, Venue::Binance, 100.0, t);
        quote(&mut e, Venue::Bybit, 100.0, t);
        quote(&mut e, Venue::Mexc, 100.0, t);
        quote(&mut e, Venue::Gate, 100.1, t);

        let snap = e.snapshot(t);
        assert!(snap.venues[Venue::Binance.idx()].dev_bps.unwrap().abs() < 1e-9);
        assert!((snap.venues[Venue::Gate.idx()].dev_bps.unwrap() - 10.0).abs() < 0.01);
    }

    #[test]
    fn stale_venue_drops_out_of_routes() {
        let mut e = engine();
        let t = Instant::now();
        quote(&mut e, Venue::Binance, 100.0, t);
        quote(&mut e, Venue::Bybit, 100.0, t);
        let later = t + Duration::from_secs(6);
        quote(&mut e, Venue::Binance, 100.0, later);
        e.tick(later);

        let snap = e.snapshot(later);
        assert!(snap.routes.is_empty());
        assert!(snap.venues[Venue::Bybit.idx()].stale);
        assert!(snap.events.iter().any(|ev| ev.kind == EventKind::Stale));
    }

    #[test]
    fn ranks_venues_by_how_late_they_follow() {
        let mut e = engine();
        let mut t = Instant::now();
        let mut px = 100.0;
        for v in Venue::ALL {
            quote(&mut e, v, px, t);
        }

        // Binance moves first; Bybit follows after 50 ms, Gate after 100 ms,
        // MEXC after 200 ms. Alternate direction so each round is a new move.
        for round in 0..6 {
            px *= if round % 2 == 0 { 1.0005 } else { 1.0 / 1.0005 };
            t += Duration::from_secs(3);
            quote(&mut e, Venue::Binance, px, t);
            quote(&mut e, Venue::Bybit, px, t + Duration::from_millis(50));
            quote(&mut e, Venue::Gate, px, t + Duration::from_millis(100));
            quote(&mut e, Venue::Mexc, px, t + Duration::from_millis(200));
        }

        let snap = e.snapshot(t);
        let lag = |v: Venue| snap.venues[v.idx()].lag_ms.expect("enough samples");
        assert!(lag(Venue::Binance) < lag(Venue::Bybit));
        assert!(lag(Venue::Bybit) < lag(Venue::Gate));
        assert!(lag(Venue::Gate) < lag(Venue::Mexc));
        // MEXC trails Binance by 200, Bybit by 150, Gate by 100.
        assert!((lag(Venue::Mexc) - 150.0).abs() < 1.0);

        let pair = |leader: Venue, follower: Venue| {
            snap.lag_pairs
                .iter()
                .find(|p| p.leader == leader && p.follower == follower)
                .unwrap_or_else(|| panic!("{leader:?} → {follower:?}"))
        };
        assert!((pair(Venue::Binance, Venue::Mexc).median_ms - 200.0).abs() < 1.0);
        assert!((pair(Venue::Gate, Venue::Mexc).median_ms - 100.0).abs() < 1.0);
        assert_eq!(pair(Venue::Binance, Venue::Bybit).samples, 6);
        assert_eq!(snap.lag_pairs.len(), 6);
        assert_eq!(snap.lag_pairs[0].median_ms, 200.0);
    }

    #[test]
    fn network_latency_is_the_median_of_recent_quotes() {
        let mut e = engine();
        let t = Instant::now();
        for ms in [100.0, 400.0, 120.0, 110.0, 130.0] {
            e.on_event(FeedEvent::Quote(Quote {
                venue: Venue::Okx,
                bid: 100.0,
                ask: 100.01,
                recv_at: t,
                latency_ms: Some(ms),
            }));
        }
        let snap = e.snapshot(t);
        assert_eq!(snap.venues[Venue::Okx.idx()].net_latency_ms, Some(120.0));
        assert_eq!(snap.venues[Venue::Binance.idx()].net_latency_ms, None);
    }

    #[test]
    fn polled_venue_trades_but_is_left_out_of_lag() {
        let mut e = engine();
        let mut t = Instant::now();
        let mut px = 100.0;
        quote(&mut e, Venue::Binance, px, t);
        quote(&mut e, Venue::Jupiter, px, t);
        for round in 0..6 {
            px *= if round % 2 == 0 { 1.0005 } else { 1.0 / 1.0005 };
            t += Duration::from_secs(3);
            quote(&mut e, Venue::Binance, px, t);
            quote(&mut e, Venue::Jupiter, px, t + Duration::from_millis(500));
        }

        let snap = e.snapshot(t + Duration::from_millis(500));
        assert_eq!(snap.lag_samples, 0);
        assert!(snap.venues[Venue::Jupiter.idx()].lag_ms.is_none());
        // Binance → Jupiter pays Binance's fee only.
        let r = snap
            .routes
            .iter()
            .find(|r| r.buy == Venue::Binance && r.sell == Venue::Jupiter)
            .unwrap();
        assert!((r.gross_bps - r.net_bps - 10.0).abs() < 1e-9);
    }

    #[test]
    fn polled_venue_goes_stale_after_missed_polls_not_the_stream_limit() {
        let mut e = engine();
        let t = Instant::now();
        quote(&mut e, Venue::Jupiter, 100.0, t);
        quote(&mut e, Venue::Binance, 100.0, t);

        // Default: 6 s polls, stale after 15 s; streams after 5 s.
        let at = |s| e.snapshot(t + Duration::from_secs(s));
        assert!(!at(10).venues[Venue::Jupiter.idx()].stale);
        assert!(at(10).venues[Venue::Binance.idx()].stale);
        assert!(at(16).venues[Venue::Jupiter.idx()].stale);
        assert_eq!(at(0).venues[Venue::Jupiter.idx()].poll_every_ms, Some(6000));
    }

    #[test]
    fn venue_without_quotes_or_marked_absent_is_unlisted() {
        let mut e = Engine::new(EngineConfig::default());
        e.on_event(FeedEvent::Status(Venue::Gate, ConnState::Connected));
        e.set_absent(Venue::Jupiter, true);
        let now = Instant::now();
        assert!(
            !e.snapshot(now).venues[Venue::Gate.idx()].unlisted,
            "just connected"
        );
        let later = now + UNLISTED_AFTER;
        assert!(e.snapshot(later).venues[Venue::Gate.idx()].unlisted);
        assert!(e.snapshot(now).venues[Venue::Jupiter.idx()].unlisted);
        // Disconnected venues are unknown, not unlisted.
        assert!(!e.snapshot(later).venues[Venue::Okx.idx()].unlisted);
        let (_, _, fresh, listed) = e.summary(later);
        assert_eq!((fresh, listed), (0, N - 2));
    }

    #[test]
    fn continuing_leader_is_not_counted_as_following() {
        let mut e = engine();
        let t = Instant::now();
        quote(&mut e, Venue::Binance, 100.0, t);
        quote(&mut e, Venue::Bybit, 100.0, t);

        // Binance leads, Bybit follows, then Binance keeps going up.
        quote(
            &mut e,
            Venue::Binance,
            100.05,
            t + Duration::from_millis(10),
        );
        quote(&mut e, Venue::Bybit, 100.05, t + Duration::from_millis(60));
        quote(
            &mut e,
            Venue::Binance,
            100.10,
            t + Duration::from_millis(80),
        );

        let samples = &e.pair_samples[Venue::Binance.idx()][Venue::Bybit.idx()];
        assert_eq!(samples.iter().copied().collect::<Vec<_>>(), vec![50.0]);
    }
}
