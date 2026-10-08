use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Venue {
    Binance,
    Bybit,
    Mexc,
    Gate,
    Okx,
    Jupiter,
}

impl Venue {
    pub const COUNT: usize = 6;
    pub const ALL: [Venue; Venue::COUNT] = [
        Venue::Binance,
        Venue::Bybit,
        Venue::Mexc,
        Venue::Gate,
        Venue::Okx,
        Venue::Jupiter,
    ];

    /// Index into the engine's per-venue arrays.
    pub fn idx(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            Venue::Binance => "Binance",
            Venue::Bybit => "Bybit",
            Venue::Mexc => "MEXC",
            Venue::Gate => "Gate",
            Venue::Okx => "OKX",
            Venue::Jupiter => "Jupiter",
        }
    }

    /// Polled rather than streamed. Its quote times reflect the poll interval,
    /// not the market, so it is left out of lead/lag.
    pub fn is_polled(self) -> bool {
        self == Venue::Jupiter
    }
}

/// Top of book as received from one venue.
#[derive(Debug, Clone, Copy)]
pub struct Quote {
    pub venue: Venue,
    pub bid: f64,
    pub ask: f64,
    /// Our clock. All staleness and lag maths uses this, never exchange
    /// timestamps: the venues' clocks disagree by more than the lag.
    pub recv_at: Instant,
    /// Local receive time minus the exchange's send timestamp, when the venue
    /// provides one. Network transit plus clock skew between the two clocks.
    pub latency_ms: Option<f64>,
}

impl Quote {
    pub fn mid(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnState {
    #[default]
    Connecting,
    Connected,
    Disconnected,
}

/// Input of one asset's engine.
#[derive(Debug, Clone)]
pub enum FeedEvent {
    Quote(Quote),
    Status(Venue, ConnState),
}

/// What feed tasks send to the market, which routes quotes to the engine of
/// their asset. Connection status is per venue and goes to every engine.
#[derive(Debug, Clone)]
pub enum MarketEvent {
    Quote(Arc<str>, Quote),
    Status(Venue, ConnState),
}

// ---------------------------------------------------------------------------
// Watchlist
// ---------------------------------------------------------------------------

/// The Solana token Jupiter quotes for an asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JupToken {
    pub mint: String,
    pub decimals: u8,
    /// Jupiter's name for it, e.g. "Ether (Portal)": shows what is compared.
    pub name: String,
}

/// One followed crypto, quoted against USDT everywhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    /// Upper case, e.g. "SOL".
    pub symbol: String,
    /// `None` when there is no verified exact match on Solana.
    pub jupiter: Option<JupToken>,
}

/// Where Gate stands on listing an asset against USDT, from its public
/// currency and pair endpoints. Gate usually adds the coin, then the pair,
/// then opens trading, so each step is an earlier signal than the quotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateListing {
    /// Gate doesn't know the coin.
    NotListed,
    Delisted,
    /// The coin exists, but there is no USDT pair yet.
    CoinAdded {
        deposits: bool,
    },
    /// The USDT pair exists but buying is not open. `opens_at` is Unix
    /// seconds when Gate has scheduled it.
    PairPending {
        opens_at: Option<i64>,
    },
    Trading,
}

impl GateListing {
    /// Short form for the watchlist.
    pub fn short(self) -> String {
        match self {
            GateListing::NotListed => "not on Gate".into(),
            GateListing::Delisted => "Gate: delisted".into(),
            GateListing::CoinAdded { deposits: true } => "Gate: deposits open".into(),
            GateListing::CoinAdded { deposits: false } => "Gate: coin added".into(),
            GateListing::PairPending { opens_at: Some(t) } => {
                format!("Gate: opens {}", fmt_local(t, "%d %b %H:%M"))
            }
            GateListing::PairPending { opens_at: None } => "Gate: pair added".into(),
            GateListing::Trading => "Gate: trading".into(),
        }
    }

    /// Full sentence for the event log.
    pub fn describe(self) -> String {
        match self {
            GateListing::NotListed => "Gate does not list the coin".into(),
            GateListing::Delisted => "Gate has delisted the coin".into(),
            GateListing::CoinAdded { deposits: true } => {
                "Gate added the coin with deposits open, no USDT pair yet".into()
            }
            GateListing::CoinAdded { deposits: false } => {
                "Gate added the coin, deposits closed, no USDT pair yet".into()
            }
            GateListing::PairPending { opens_at: Some(t) } => {
                format!(
                    "Gate added the USDT pair, buying opens {}",
                    fmt_local(t, "%Y-%m-%d %H:%M")
                )
            }
            GateListing::PairPending { opens_at: None } => {
                "Gate added the USDT pair, trading not open yet".into()
            }
            GateListing::Trading => "Gate trades the USDT pair".into(),
        }
    }
}

fn fmt_local(unix_secs: i64, fmt: &str) -> String {
    chrono::DateTime::from_timestamp(unix_secs, 0).map_or_else(
        || unix_secs.to_string(),
        |t| t.with_timezone(&chrono::Local).format(fmt).to_string(),
    )
}

// ---------------------------------------------------------------------------
// Snapshot: everything the UI needs, precomputed and cheap to render.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct VenueView {
    pub venue: Venue,
    pub conn: ConnState,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    /// Own bid/ask spread in bps.
    pub spread_bps: Option<f64>,
    /// Milliseconds since the last quote.
    pub age_ms: Option<u64>,
    pub stale: bool,
    /// Quotes per second over the last full second.
    pub rate: u32,
    /// Mid vs the median mid of all fresh venues, in bps.
    pub dev_bps: Option<f64>,
    /// How far this venue trails the others when price moves: mean of its
    /// pairwise median follow delays. Positive is late, negative leads.
    pub lag_ms: Option<f64>,
    /// Taker fee for one leg on this venue.
    pub fee_bps: f64,
    /// Median of recent `Quote::latency_ms`: how long messages take to reach
    /// us, give or take clock skew. `None` for venues without a timestamp.
    pub net_latency_ms: Option<f64>,
    /// Poll interval, for polled venues only.
    pub poll_every_ms: Option<u64>,
    /// The venue doesn't quote this asset: connected for a while without a
    /// single quote, or no Solana token for Jupiter.
    pub unlisted: bool,
}

impl VenueView {
    pub fn new(venue: Venue) -> Self {
        Self {
            venue,
            conn: ConnState::Connecting,
            bid: None,
            ask: None,
            spread_bps: None,
            age_ms: None,
            stale: true,
            rate: 0,
            dev_bps: None,
            lag_ms: None,
            fee_bps: 0.0,
            net_latency_ms: None,
            poll_every_ms: None,
            unlisted: false,
        }
    }
}

/// Buy on one venue's ask, sell on the other's bid.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub buy: Venue,
    pub sell: Venue,
    pub gross_bps: f64,
    /// Gross minus the taker fee on both venues.
    pub net_bps: f64,
    /// Best gross edge seen on this route since start.
    pub best_gross_bps: f64,
}

/// Lead/lag between two venues, oriented so the delay is never negative.
#[derive(Debug, Clone, PartialEq)]
pub struct PairLag {
    pub leader: Venue,
    pub follower: Venue,
    /// Median delay for `follower` to repeat a move `leader` made first.
    pub median_ms: f64,
    /// Matched moves behind the median.
    pub samples: usize,
    /// Enough samples to count toward the per-venue lag figures.
    pub reliable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Opportunity,
    Stale,
    Connection,
    Listing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Local wall-clock time, preformatted.
    pub at: String,
    pub kind: EventKind,
    pub text: String,
}

impl Event {
    /// Stamped with the current local time.
    pub fn now(kind: EventKind, text: String) -> Self {
        Self {
            at: chrono::Local::now().format("%H:%M:%S%.3f").to_string(),
            kind,
            text,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub venues: Vec<VenueView>,
    /// Every ordered pair of fresh venues, best gross edge first.
    pub routes: Vec<Route>,
    /// Matched moves behind the per-venue lag figures.
    pub lag_samples: usize,
    /// Every venue pair with at least one matched move, largest delay first.
    pub lag_pairs: Vec<PairLag>,
    /// Newest first.
    pub events: Vec<Event>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            venues: Venue::ALL.map(VenueView::new).to_vec(),
            routes: Vec::new(),
            lag_samples: 0,
            lag_pairs: Vec::new(),
            events: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Market: every followed asset, plus the details of the selected one.
// ---------------------------------------------------------------------------

/// One line of the watchlist.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetSummary {
    pub symbol: String,
    /// Median mid across fresh venues.
    pub mid: Option<f64>,
    pub best_gross_bps: Option<f64>,
    /// Venues with a fresh quote.
    pub fresh: usize,
    /// Venues that quote this asset, or may still turn out to.
    pub listed: usize,
    pub jupiter: Option<JupToken>,
    /// `None` until Gate's API has answered once.
    pub gate: Option<GateListing>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MarketSnapshot {
    pub assets: Vec<AssetSummary>,
    pub selected: Option<String>,
    /// Everything for the selected asset.
    pub detail: Snapshot,
    /// Symbol being looked up before it joins the list.
    pub adding: Option<String>,
    /// Last problem with the watchlist, e.g. an invalid symbol.
    pub notice: Option<String>,
    /// Watchlist assets past the limit: listed, not followed.
    pub locked: Vec<String>,
    /// How many assets may be followed at once; `None` for no limit.
    pub limit: Option<usize>,
    /// The limit is reached: adding another asset is refused.
    pub at_limit: bool,
}
