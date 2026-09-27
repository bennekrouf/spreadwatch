//! New-listing scanner, independent of the watchlist.
//!
//! Finds tokens Gate or MEXC list against USDT in the last `NEW_WITHIN_DAYS`
//! (or has scheduled), that Binance spot doesn't list, and scores how serious
//! they look: volume, how many other exchanges list them, market cap, and
//! whether Binance already follows them through futures or Binance Alpha.
//!
//! Everything comes from public bulk endpoints, fetched every `SCAN_EVERY`.
//! Gate publishes each pair's opening time; MEXC doesn't, so its first daily
//! candle stands in, fetched once per token and cached on disk.
//! Tokens are matched across exchanges by ticker, so two different tokens
//! sharing a ticker look like one.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use tokio::sync::watch;

use crate::model::{Event, EventKind};

/// Listings trickle in a few a day and volumes move slowly: every five
/// minutes is timely and keeps the ~8 MB of bulk downloads per scan light.
pub const SCAN_EVERY: Duration = Duration::from_secs(300);
/// Ages move with the clock: rows are rebuilt from the last scan this often.
const REFRESH_EVERY: Duration = Duration::from_secs(60);
pub const NEW_WITHIN_DAYS: i64 = 30;
/// Daily candles asked from MEXC: fewer back means the token is that young.
const MEXC_DATE_DAYS: usize = 60;
/// Concurrent MEXC candle requests, well under its public limit.
const MEXC_DATE_CONCURRENCY: usize = 5;
const MAX_EVENTS: usize = 200;
/// Bid/ask spread on the main venue at or under this is a good sign…
pub const TIGHT_SPREAD_BPS: f64 = 20.0;
/// …and at or over this, a warning: 1 % lost on every round trip.
pub const WIDE_SPREAD_BPS: f64 = 100.0;
const DAY: i64 = 86_400;

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Exchange {
    Gate,
    Mexc,
    Bybit,
    Okx,
}

impl Exchange {
    pub fn name(self) -> &'static str {
        match self {
            Exchange::Gate => "Gate",
            Exchange::Mexc => "MEXC",
            Exchange::Bybit => "Bybit",
            Exchange::Okx => "OKX",
        }
    }
}

/// One exchange's USDT market for a token.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VenueQuote {
    pub exchange: Exchange,
    pub volume_usd: Option<f64>,
    pub spread_bps: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub text: String,
    /// A reason to take the token seriously, or a warning.
    pub good: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewListing {
    pub symbol: String,
    pub name: String,
    /// Unix seconds of the earliest known USDT listing on Gate or MEXC; in
    /// the future when Gate has scheduled it.
    pub listed_at: i64,
    /// "3 d ago", "in 5 h".
    pub age: String,
    /// Local date and time, e.g. "24 Sep 14:30". MEXC dates are the UTC day.
    pub listed_date: String,
    /// Where it trades against USDT.
    pub venues: Vec<VenueQuote>,
    pub volume_usd: f64,
    /// 24 h price change on its most traded venue, in %.
    pub change_pct: Option<f64>,
    /// Bid/ask spread on its most traded venue, in bps of the mid.
    pub spread_bps: Option<f64>,
    pub market_cap: Option<f64>,
    pub holders: Option<u64>,
    pub binance_futures: bool,
    pub binance_alpha: bool,
    pub signals: Vec<Signal>,
    /// Good signals minus warnings, for sorting.
    pub score: i32,
    /// Showed up after this session's first scan.
    pub fresh: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScanSnapshot {
    /// Best score first.
    pub rows: Vec<NewListing>,
    /// Local time of the last finished scan.
    pub last_scan: Option<String>,
    /// What the scanner is doing right now, if anything.
    pub progress: Option<String>,
    /// Sources that failed on the last scan; their previous data is kept.
    pub errors: Vec<String>,
    /// New tokens and Binance moves, newest first.
    pub events: Vec<Event>,
}

// ---------------------------------------------------------------------------
// Exchange responses, only the fields used
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BinancePrice {
    symbol: String,
}

#[derive(Deserialize)]
struct AlphaList {
    data: Vec<AlphaToken>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct AlphaToken {
    symbol: String,
    name: String,
    market_cap: Option<String>,
    holders: Option<String>,
}

#[derive(Deserialize, Clone)]
struct GatePair {
    base: String,
    base_name: String,
    quote: String,
    /// Unix seconds trading opens; in the future when scheduled.
    buy_start: i64,
}

#[derive(Deserialize)]
struct GateTicker {
    currency_pair: String,
    quote_volume: Option<String>,
    change_percentage: Option<String>,
    highest_bid: Option<String>,
    lowest_ask: Option<String>,
}

#[derive(Deserialize, Clone)]
struct GateCurrency {
    currency: String,
    market_cap: Option<String>,
    /// "stocks", "metals", "indices"…: not crypto.
    #[serde(default)]
    category: Vec<String>,
}

#[derive(Deserialize)]
struct MexcInfo {
    symbols: Vec<MexcSymbol>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MexcSymbol {
    symbol: String,
    /// "1" online.
    status: String,
    base_asset: String,
    quote_asset: String,
    full_name: Option<String>,
    #[serde(default)]
    is_spot_trading_allowed: bool,
    #[serde(default)]
    concept_plates: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MexcTicker {
    symbol: String,
    quote_volume: Option<String>,
    /// A fraction: 0.05 is +5 %.
    price_change_percent: Option<String>,
    bid_price: Option<String>,
    ask_price: Option<String>,
}

#[derive(Deserialize)]
struct BybitResp {
    result: BybitList,
}

#[derive(Deserialize)]
struct BybitList {
    list: Vec<BybitTicker>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BybitTicker {
    symbol: String,
    turnover24h: Option<String>,
    /// A fraction, like MEXC's.
    price24h_pcnt: Option<String>,
    bid1_price: Option<String>,
    ask1_price: Option<String>,
}

#[derive(Deserialize)]
struct OkxResp {
    data: Vec<OkxTicker>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OkxTicker {
    inst_id: String,
    vol_ccy24h: Option<String>,
    last: Option<String>,
    open24h: Option<String>,
    bid_px: Option<String>,
    ask_px: Option<String>,
}

fn num(s: &Option<String>) -> Option<f64> {
    s.as_deref()
        .and_then(|s| s.parse().ok())
        .filter(|v: &f64| v.is_finite())
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Ticker {
    volume_usd: Option<f64>,
    change_pct: Option<f64>,
    bid: Option<f64>,
    ask: Option<f64>,
}

impl Ticker {
    fn spread_bps(&self) -> Option<f64> {
        let (bid, ask) = (self.bid?, self.ask?);
        (bid > 0.0 && ask >= bid).then(|| (ask - bid) / ((ask + bid) / 2.0) * 1e4)
    }
}

/// Latest data from every source. A source that fails keeps its previous
/// data, so one slow exchange doesn't empty the table.
#[derive(Default)]
struct Sources {
    binance_spot: HashSet<String>,
    binance_futures: HashSet<String>,
    alpha: HashMap<String, AlphaToken>,
    gate_pairs: Vec<GatePair>,
    gate_tickers: HashMap<String, Ticker>,
    gate_currencies: HashMap<String, GateCurrency>,
    mexc_symbols: Vec<MexcSymbol>,
    mexc_tickers: HashMap<String, Ticker>,
    bybit: HashMap<String, Ticker>,
    okx: HashMap<String, Ticker>,
}

/// "PEPEUSDT" → "PEPE" for the stablecoin quotes every Binance listing has.
fn binance_base(symbol: &str) -> Option<&str> {
    ["FDUSD", "USDT", "USDC"]
        .iter()
        .find_map(|q| symbol.strip_suffix(q))
        .filter(|b| !b.is_empty())
}

/// Futures quote memecoins in thousands: "1000PEPE", "1MBABYDOGE".
fn futures_base(symbol: &str) -> Option<&str> {
    let base = binance_base(symbol)?;
    Some(
        ["1000000", "10000", "1000", "1M"]
            .iter()
            .find_map(|p| base.strip_prefix(p))
            .unwrap_or(base),
    )
}

async fn get<T: DeserializeOwned>(http: &reqwest::Client, url: &str) -> Result<T> {
    Ok(http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

impl Sources {
    /// Fetches everything at once; returns one line per failed source.
    async fn update(&mut self, http: &reqwest::Client) -> Vec<String> {
        let (bn, bnf, alpha, gp, gt, gc, mi, mt, by, okx) = tokio::join!(
            get::<Vec<BinancePrice>>(http, "https://api.binance.com/api/v3/ticker/price"),
            get::<Vec<BinancePrice>>(http, "https://fapi.binance.com/fapi/v1/ticker/price"),
            get::<AlphaList>(
                http,
                "https://www.binance.com/bapi/defi/v1/public/wallet-direct/buw/wallet/cex/alpha/all/token/list"
            ),
            get::<Vec<GatePair>>(http, "https://api.gateio.ws/api/v4/spot/currency_pairs"),
            get::<Vec<GateTicker>>(http, "https://api.gateio.ws/api/v4/spot/tickers"),
            get::<Vec<GateCurrency>>(http, "https://api.gateio.ws/api/v4/spot/currencies"),
            get::<MexcInfo>(http, "https://api.mexc.com/api/v3/exchangeInfo"),
            get::<Vec<MexcTicker>>(http, "https://api.mexc.com/api/v3/ticker/24hr"),
            get::<BybitResp>(http, "https://api.bybit.com/v5/market/tickers?category=spot"),
            get::<OkxResp>(http, "https://www.okx.com/api/v5/market/tickers?instType=SPOT"),
        );
        let mut errors = Vec::new();
        let mut keep = |name: &str, r: Result<()>| {
            if let Err(e) = r {
                tracing::warn!("scanner: {name} failed: {e:#}");
                errors.push(format!("{name}: {e:#}"));
            }
        };
        keep(
            "Binance spot",
            bn.map(|v| {
                self.binance_spot = v
                    .iter()
                    .filter_map(|p| binance_base(&p.symbol))
                    .map(str::to_owned)
                    .collect();
            }),
        );
        keep(
            "Binance futures",
            bnf.map(|v| {
                self.binance_futures = v
                    .iter()
                    .filter_map(|p| futures_base(&p.symbol))
                    .map(str::to_owned)
                    .collect();
            }),
        );
        keep(
            "Binance Alpha",
            alpha.map(|v| {
                self.alpha = v
                    .data
                    .into_iter()
                    .map(|t| (t.symbol.to_uppercase(), t))
                    .collect();
            }),
        );
        keep(
            "Gate pairs",
            gp.map(|v| self.gate_pairs = v.into_iter().filter(|p| p.quote == "USDT").collect()),
        );
        keep(
            "Gate tickers",
            gt.map(|v| {
                self.gate_tickers = v
                    .into_iter()
                    .filter_map(|t| {
                        let base = t.currency_pair.strip_suffix("_USDT")?.to_owned();
                        let ticker = Ticker {
                            volume_usd: num(&t.quote_volume),
                            change_pct: num(&t.change_percentage),
                            bid: num(&t.highest_bid),
                            ask: num(&t.lowest_ask),
                        };
                        Some((base, ticker))
                    })
                    .collect();
            }),
        );
        keep(
            "Gate currencies",
            gc.map(|v| {
                self.gate_currencies = v.into_iter().map(|c| (c.currency.clone(), c)).collect();
            }),
        );
        keep(
            "MEXC symbols",
            mi.map(|v| {
                self.mexc_symbols = v
                    .symbols
                    .into_iter()
                    .filter(|s| {
                        s.quote_asset == "USDT" && s.status == "1" && s.is_spot_trading_allowed
                    })
                    .collect();
            }),
        );
        keep(
            "MEXC tickers",
            mt.map(|v| {
                self.mexc_tickers = v
                    .into_iter()
                    .filter_map(|t| {
                        let base = t.symbol.strip_suffix("USDT")?.to_owned();
                        let ticker = Ticker {
                            volume_usd: num(&t.quote_volume),
                            change_pct: num(&t.price_change_percent).map(|f| f * 100.0),
                            bid: num(&t.bid_price),
                            ask: num(&t.ask_price),
                        };
                        Some((base, ticker))
                    })
                    .collect();
            }),
        );
        keep(
            "Bybit",
            by.map(|v| {
                self.bybit = v
                    .result
                    .list
                    .into_iter()
                    .filter_map(|t| {
                        let ticker = Ticker {
                            volume_usd: num(&t.turnover24h),
                            change_pct: num(&t.price24h_pcnt).map(|f| f * 100.0),
                            bid: num(&t.bid1_price),
                            ask: num(&t.ask1_price),
                        };
                        Some((t.symbol.strip_suffix("USDT")?.to_owned(), ticker))
                    })
                    .collect();
            }),
        );
        keep(
            "OKX",
            okx.map(|v| {
                self.okx = v
                    .data
                    .into_iter()
                    .filter_map(|t| {
                        let (last, open) = (num(&t.last), num(&t.open24h).filter(|o| *o > 0.0));
                        let ticker = Ticker {
                            volume_usd: num(&t.vol_ccy24h),
                            change_pct: last.zip(open).map(|(l, o)| (l / o - 1.0) * 100.0),
                            bid: num(&t.bid_px),
                            ask: num(&t.ask_px),
                        };
                        Some((t.inst_id.strip_suffix("-USDT")?.to_owned(), ticker))
                    })
                    .collect();
            }),
        );
        errors
    }

    /// MEXC tokens Binance doesn't list, as candidates for dating.
    fn mexc_candidates(&self) -> impl Iterator<Item = &MexcSymbol> {
        self.mexc_symbols
            .iter()
            .filter(|s| !self.binance_spot.contains(&s.base_asset) && !mexc_excluded(s))
    }
}

/// Leveraged tokens: "SUI5S" named "SUI5xShort", "BTC3L".
fn is_leveraged(base: &str, name: &str) -> bool {
    let mut rev = base.chars().rev();
    let shape =
        matches!(rev.next(), Some('L' | 'S')) && rev.next().is_some_and(|c| c.is_ascii_digit());
    let name = name.to_ascii_lowercase();
    shape && (name.contains("long") || name.contains("short") || name.is_empty())
}

fn gate_excluded(p: &GatePair, currencies: &HashMap<String, GateCurrency>) -> bool {
    is_leveraged(&p.base, &p.base_name)
        || currencies
            .get(&p.base)
            .is_some_and(|c| !c.category.is_empty())
}

fn mexc_excluded(s: &MexcSymbol) -> bool {
    let stock = s
        .concept_plates
        .as_ref()
        .is_some_and(|c| c.iter().any(|p| p == "Tokenized Stocks"));
    stock || is_leveraged(&s.base_asset, s.full_name.as_deref().unwrap_or(""))
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// MEXC listing day per symbol: `Some(unix secs)` for its first daily
/// candle, `None` when it is older than `MEXC_DATE_DAYS`.
type MexcDates = HashMap<String, Option<i64>>;

fn build(src: &Sources, dates: &MexcDates, now: i64) -> Vec<NewListing> {
    // Base → (earliest listing, known to be old, name).
    let mut found: HashMap<&str, (Option<i64>, bool, &str)> = HashMap::new();
    for p in &src.gate_pairs {
        if src.binance_spot.contains(&p.base) || gate_excluded(p, &src.gate_currencies) {
            continue;
        }
        let e = found.entry(&p.base).or_insert((None, false, &p.base_name));
        e.0 = Some(e.0.map_or(p.buy_start, |t| t.min(p.buy_start)));
    }
    for s in src.mexc_candidates() {
        // Not dated yet: shown once it is.
        let Some(date) = dates.get(&s.symbol) else {
            continue;
        };
        let e = found.entry(&s.base_asset).or_insert((
            None,
            false,
            s.full_name.as_deref().unwrap_or(""),
        ));
        match date {
            Some(t) => e.0 = Some(e.0.map_or(*t, |x| x.min(*t))),
            None => e.1 = true,
        }
    }

    let mut rows: Vec<NewListing> = found
        .into_iter()
        .filter_map(|(base, (listed_at, old, name))| {
            let listed_at = listed_at?;
            (!old && now - listed_at <= NEW_WITHIN_DAYS * DAY)
                .then(|| row(src, base, name, listed_at, now))
        })
        .collect();
    rows.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.volume_usd.total_cmp(&a.volume_usd))
    });
    rows
}

fn row(src: &Sources, base: &str, name: &str, listed_at: i64, now: i64) -> NewListing {
    let gate_listed = src.gate_pairs.iter().any(|p| p.base == base);
    let mexc_listed = src.mexc_symbols.iter().any(|s| s.base_asset == base);
    // A listed pair may have no ticker yet, e.g. before trading opens.
    let listed = |yes: bool, t: Option<&Ticker>| yes.then(|| t.copied().unwrap_or_default());
    let tickers: Vec<(Exchange, Ticker)> = [
        (
            Exchange::Gate,
            listed(gate_listed, src.gate_tickers.get(base)),
        ),
        (
            Exchange::Mexc,
            listed(mexc_listed, src.mexc_tickers.get(base)),
        ),
        (Exchange::Bybit, src.bybit.get(base).copied()),
        (Exchange::Okx, src.okx.get(base).copied()),
    ]
    .into_iter()
    .filter_map(|(e, t)| Some((e, t?)))
    .collect();
    // Price change and spread come from the most traded venue.
    let main = tickers
        .iter()
        .filter_map(|(_, t)| Some((t.volume_usd?, *t)))
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, t)| t);
    let venues: Vec<VenueQuote> = tickers
        .iter()
        .map(|(e, t)| VenueQuote {
            exchange: *e,
            volume_usd: t.volume_usd,
            spread_bps: t.spread_bps(),
        })
        .collect();
    let alpha = src.alpha.get(base);
    let market_cap = alpha
        .and_then(|a| num(&a.market_cap))
        .or_else(|| {
            src.gate_currencies
                .get(base)
                .and_then(|c| num(&c.market_cap))
        })
        .filter(|m| *m > 0.0);
    let name = if name.is_empty() {
        alpha.map_or("", |a| a.name.as_str())
    } else {
        name
    };

    let mut r = NewListing {
        symbol: base.to_owned(),
        name: name.to_owned(),
        listed_at,
        age: age(listed_at, now),
        listed_date: chrono::DateTime::from_timestamp(listed_at, 0)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%d %b %H:%M")
                    .to_string()
            })
            .unwrap_or_default(),
        volume_usd: venues.iter().filter_map(|v| v.volume_usd).sum(),
        venues,
        change_pct: main.and_then(|t| t.change_pct),
        spread_bps: main.and_then(|t| t.spread_bps()),
        market_cap,
        holders: alpha.and_then(|a| a.holders.as_deref()?.parse().ok()),
        binance_futures: src.binance_futures.contains(base),
        binance_alpha: alpha.is_some(),
        signals: Vec::new(),
        score: 0,
        fresh: false,
    };
    (r.signals, r.score) = assess(&r, now);
    r
}

/// The reasons to look closer, and the warnings, behind the score.
fn assess(r: &NewListing, now: i64) -> (Vec<Signal>, i32) {
    let mut out = Vec::new();
    let mut good = |text: String, points: i32| out.push((Signal { text, good: true }, points));
    if r.binance_futures {
        good("Binance futures".into(), 2);
    }
    if r.binance_alpha {
        good("Binance Alpha".into(), 1);
    }
    let others: Vec<&str> = r
        .venues
        .iter()
        .filter(|v| matches!(v.exchange, Exchange::Bybit | Exchange::Okx))
        .map(|v| v.exchange.name())
        .collect();
    if !others.is_empty() {
        good(format!("on {}", others.join(" + ")), others.len() as i32);
    }
    let on = |e: Exchange| r.venues.iter().any(|v| v.exchange == e);
    if on(Exchange::Gate) && on(Exchange::Mexc) {
        good("Gate + MEXC".into(), 1);
    }
    if r.volume_usd >= 1_000_000.0 {
        good(format!("vol {}", fmt_usd(r.volume_usd)), 2);
    } else if r.volume_usd >= 250_000.0 {
        good(format!("vol {}", fmt_usd(r.volume_usd)), 1);
    }
    if let Some(m) = r.market_cap.filter(|m| *m >= 10_000_000.0) {
        good(
            format!("mcap {}", fmt_usd(m)),
            if m >= 100_000_000.0 { 2 } else { 1 },
        );
    }
    if let Some(h) = r.holders.filter(|h| *h >= 10_000) {
        good(format!("{}k holders", h / 1000), 1);
    }
    if let Some(s) = r.spread_bps.filter(|s| *s <= TIGHT_SPREAD_BPS) {
        good(format!("tight spread {}", fmt_spread(s)), 1);
    }

    let mut bad = |text: String| out.push((Signal { text, good: false }, -1));
    let upcoming = r.listed_at > now;
    if !upcoming && r.volume_usd < 50_000.0 {
        bad(format!("thin: {} vol", fmt_usd(r.volume_usd)));
    }
    if let Some(m) = r.market_cap.filter(|m| *m < 2_000_000.0) {
        bad(format!("tiny mcap {}", fmt_usd(m)));
    }
    match r.change_pct {
        Some(c) if c >= 100.0 => bad(format!("pumped {c:+.0} %")),
        Some(c) if c <= -50.0 => bad(format!("dumped {c:+.0} %")),
        _ => {}
    }
    if let Some(s) = r.spread_bps.filter(|s| *s >= WIDE_SPREAD_BPS) {
        bad(format!("wide spread {}", fmt_spread(s)));
    }
    let score = out.iter().map(|(_, p)| p).sum();
    (out.into_iter().map(|(s, _)| s).collect(), score)
}

/// "12 bps (0.12 %)", "0.3 bps (0.003 %)": one-tick spreads are well under 1 bp.
pub fn fmt_spread(bps: f64) -> String {
    format!("{} ({})", fmt_bps(bps), fmt_pct(bps))
}

/// Whole bps from 10 up, one decimal below.
pub fn fmt_bps(bps: f64) -> String {
    if bps < 10.0 {
        format!("{bps:.1} bps")
    } else {
        format!("{bps:.0} bps")
    }
}

pub fn fmt_pct(bps: f64) -> String {
    if bps < 10.0 {
        format!("{:.3} %", bps / 100.0)
    } else {
        format!("{:.2} %", bps / 100.0)
    }
}

impl NewListing {
    /// The row as one line of plain text, for pasting elsewhere.
    pub fn summary_line(&self) -> String {
        let venues: Vec<String> = self
            .venues
            .iter()
            .map(|v| match v.volume_usd {
                Some(vol) => format!("{} {}", v.exchange.name(), fmt_usd(vol)),
                None => v.exchange.name().to_owned(),
            })
            .collect();
        let pick = |good: bool| {
            self.signals
                .iter()
                .filter(|s| s.good == good)
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut parts = vec![
            format!("{} ({})", self.symbol, self.name),
            format!("listed {} ({})", self.age, self.listed_date),
            venues.join(", "),
            format!("vol {}", fmt_usd(self.volume_usd)),
        ];
        if let Some(c) = self.change_pct {
            parts.push(format!("24h {c:+.1} %"));
        }
        if let Some(m) = self.market_cap {
            parts.push(format!("mcap {}", fmt_usd(m)));
        }
        if let Some(s) = self.spread_bps {
            parts.push(format!("spread {}", fmt_spread(s)));
        }
        let mut score = format!("score {}", self.score);
        for (label, list) in [("+", pick(true)), ("-", pick(false))] {
            if !list.is_empty() {
                score.push_str(&format!(" {label} {list}"));
            }
        }
        parts.push(score);
        parts.join(" | ")
    }
}

/// "$1.2M", "$340k", "$12.3B".
pub fn fmt_usd(v: f64) -> String {
    match v.abs() {
        a if a >= 1e9 => format!("${:.1}B", v / 1e9),
        a if a >= 1e6 => format!("${:.1}M", v / 1e6),
        a if a >= 1e3 => format!("${:.0}k", v / 1e3),
        _ => format!("${v:.0}"),
    }
}

/// "3 d ago", "5 h ago", "in 2 h".
fn age(listed_at: i64, now: i64) -> String {
    let d = now - listed_at;
    let span = |s: i64| match s {
        s if s >= 2 * DAY => format!("{} d", s / DAY),
        s if s >= 3600 => format!("{} h", s / 3600),
        s => format!("{} min", (s / 60).max(1)),
    };
    if d >= 0 {
        format!("{} ago", span(d))
    } else {
        format!("in {}", span(-d))
    }
}

// ---------------------------------------------------------------------------
// MEXC listing dates
// ---------------------------------------------------------------------------

fn dates_path() -> PathBuf {
    crate::trade::config_dir().join("mexc_listing_dates.json")
}

fn load_dates() -> MexcDates {
    std::fs::read_to_string(dates_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_dates(dates: &MexcDates) {
    let saved = std::fs::create_dir_all(crate::trade::config_dir()).and_then(|()| {
        std::fs::write(
            dates_path(),
            serde_json::to_string(dates).unwrap_or_default(),
        )
    });
    if let Err(e) = saved {
        tracing::warn!("scanner: could not save MEXC listing dates: {e}");
    }
}

/// `Some(Some(day))` for a young token, `Some(None)` when it is older than
/// the candles asked for, `None` when MEXC has no candle yet or failed.
async fn mexc_listing_day(http: &reqwest::Client, symbol: &str) -> Option<Option<i64>> {
    let url = format!(
        "https://api.mexc.com/api/v3/klines?symbol={symbol}&interval=1d&limit={MEXC_DATE_DAYS}"
    );
    let candles: Vec<Vec<serde_json::Value>> = get(http, &url).await.ok()?;
    let first_ms = candles.first()?.first()?.as_i64()?;
    Some((candles.len() < MEXC_DATE_DAYS).then_some(first_ms / 1000))
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

struct Scanner {
    http: reqwest::Client,
    src: Sources,
    dates: MexcDates,
    snap: ScanSnapshot,
    events: VecDeque<Event>,
    /// Symbols of every row seen this session; `None` before the first scan.
    seen: Option<HashSet<String>>,
    /// Last rows, to notice Binance picking a token up.
    prev: HashMap<String, NewListing>,
    out: watch::Sender<ScanSnapshot>,
}

pub async fn run(out: watch::Sender<ScanSnapshot>) {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("spreadwatch")
        .build()
        .expect("http client");
    let mut s = Scanner {
        http,
        src: Sources::default(),
        dates: load_dates(),
        snap: ScanSnapshot::default(),
        events: VecDeque::new(),
        seen: None,
        prev: HashMap::new(),
        out,
    };
    let mut scan = tokio::time::interval(SCAN_EVERY);
    scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut refresh = tokio::time::interval(REFRESH_EVERY);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = scan.tick() => s.scan().await,
            _ = refresh.tick() => s.rebuild(false),
        }
        if s.out.is_closed() {
            return;
        }
    }
}

impl Scanner {
    async fn scan(&mut self) {
        self.progress(Some("Scanning Gate, MEXC, Binance, Bybit and OKX…".into()));
        self.snap.errors = self.src.update(&self.http).await;
        if self.src.binance_spot.is_empty() {
            // Without it every token would look unlisted on Binance.
            self.progress(Some(
                "Waiting for Binance's symbol list; retrying next scan".into(),
            ));
            return;
        }
        self.date_mexc().await;
        self.snap.last_scan = Some(chrono::Local::now().format("%H:%M:%S").to_string());
        self.snap.progress = None;
        self.rebuild(true);
    }

    /// Dates the MEXC candidates not dated yet, a few requests at a time.
    async fn date_mexc(&mut self) {
        let todo: Vec<String> = self
            .src
            .mexc_candidates()
            .filter(|s| !self.dates.contains_key(&s.symbol))
            .map(|s| s.symbol.clone())
            .collect();
        if todo.is_empty() {
            return;
        }
        let total = todo.len();
        let http = self.http.clone();
        let mut results = futures_util::stream::iter(todo)
            .map(|symbol| {
                let http = http.clone();
                async move {
                    let day = mexc_listing_day(&http, &symbol).await;
                    (symbol, day)
                }
            })
            .buffer_unordered(MEXC_DATE_CONCURRENCY);
        let mut done = 0;
        while let Some((symbol, day)) = results.next().await {
            done += 1;
            if let Some(day) = day {
                self.dates.insert(symbol, day);
            }
            if done % 25 == 0 {
                self.progress(Some(format!("Finding MEXC listing dates: {done}/{total}")));
            }
        }
        save_dates(&self.dates);
    }

    fn progress(&mut self, text: Option<String>) {
        self.snap.progress = text;
        self.out.send_replace(self.snap.clone());
    }

    /// Rebuilds rows from the last data. After a scan, also logs tokens that
    /// are new this session and tokens Binance picked up.
    fn rebuild(&mut self, after_scan: bool) {
        if self.src.binance_spot.is_empty() {
            return;
        }
        let now = unix_now();
        let mut rows = build(&self.src, &self.dates, now);
        if after_scan {
            self.log_changes(&rows);
        }
        let seen = self.seen.as_ref();
        for r in &mut rows {
            r.fresh = seen.is_some_and(|s| !s.contains(&r.symbol))
                || self.prev.get(&r.symbol).is_some_and(|p| p.fresh);
        }
        self.prev = rows.iter().map(|r| (r.symbol.clone(), r.clone())).collect();
        if after_scan {
            self.seen
                .get_or_insert_with(HashSet::new)
                .extend(rows.iter().map(|r| r.symbol.clone()));
        }
        self.snap.rows = rows;
        self.snap.events = self.events.iter().cloned().collect();
        self.out.send_replace(self.snap.clone());
    }

    fn log_changes(&mut self, rows: &[NewListing]) {
        let mut log = |text: String| {
            tracing::info!("scanner: {text}");
            self.events.push_front(Event::now(EventKind::Listing, text));
        };
        let Some(seen) = self.seen.as_ref() else {
            log(format!(
                "First scan: {} tokens listed on Gate or MEXC in the last {NEW_WITHIN_DAYS} days are not on Binance spot",
                rows.len()
            ));
            self.events.truncate(MAX_EVENTS);
            return;
        };
        for r in rows {
            let venues = r
                .venues
                .iter()
                .map(|v| v.exchange.name())
                .collect::<Vec<_>>()
                .join(", ");
            if !seen.contains(&r.symbol) {
                let when = if r.listed_at > unix_now() {
                    format!("opens {}", r.age)
                } else {
                    format!("listed {}", r.age)
                };
                log(format!(
                    "{} ({}): new on {venues}, {when}, {} vol, score {}",
                    r.symbol,
                    r.name,
                    fmt_usd(r.volume_usd),
                    r.score
                ));
            }
            if let Some(p) = self.prev.get(&r.symbol) {
                if r.binance_futures && !p.binance_futures {
                    log(format!("{}: now on Binance futures", r.symbol));
                }
                if r.binance_alpha && !p.binance_alpha {
                    log(format!("{}: now on Binance Alpha", r.symbol));
                }
            }
        }
        for symbol in self.prev.keys() {
            if self.src.binance_spot.contains(symbol) {
                log(format!("{symbol}: now listed on Binance spot"));
            }
        }
        self.events.truncate(MAX_EVENTS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000;

    fn sources() -> Sources {
        let pair = |base: &str, name: &str, start: i64| GatePair {
            base: base.into(),
            base_name: name.into(),
            quote: "USDT".into(),
            buy_start: start,
        };
        let mexc = |base: &str| MexcSymbol {
            symbol: format!("{base}USDT"),
            status: "1".into(),
            base_asset: base.into(),
            quote_asset: "USDT".into(),
            full_name: Some(base.into()),
            is_spot_trading_allowed: true,
            concept_plates: None,
        };
        let mut src = Sources {
            binance_spot: ["SOL".to_owned()].into(),
            binance_futures: ["NEWT".to_owned()].into(),
            gate_pairs: vec![
                pair("SOL", "Solana", NOW - 900 * DAY),
                pair("NEWT", "Newt", NOW - 3 * DAY),
                pair("SOON", "Soon", NOW + 2 * 3600),
                pair("SUI5S", "SUI5xShort", NOW - DAY),
                pair("OLDG", "Old on Gate", NOW - 200 * DAY),
                pair("AAPLX", "Apple xStock", NOW - DAY),
            ],
            mexc_symbols: vec![mexc("NEWT"), mexc("MX1"), mexc("OLDM")],
            ..Default::default()
        };
        src.gate_currencies.insert(
            "AAPLX".into(),
            GateCurrency {
                currency: "AAPLX".into(),
                market_cap: None,
                category: vec!["stocks".into()],
            },
        );
        let ticker = |vol: f64, change: f64, bid: f64, ask: f64| Ticker {
            volume_usd: Some(vol),
            change_pct: Some(change),
            bid: Some(bid),
            ask: Some(ask),
        };
        // NEWT: 1.00 / 1.0012 on Gate, its busiest venue, is about a 12 bps spread.
        src.gate_tickers
            .insert("NEWT".into(), ticker(800_000.0, 12.0, 1.0, 1.0012));
        src.mexc_tickers
            .insert("NEWT".into(), ticker(400_000.0, 10.0, 1.0, 1.05));
        src.mexc_tickers
            .insert("MX1".into(), ticker(20_000.0, 250.0, 1.0, 1.02));
        src.bybit
            .insert("NEWT".into(), ticker(300_000.0, 11.0, 1.0, 1.002));
        src
    }

    #[test]
    fn finds_recent_listings_binance_spot_lacks() {
        let dates: MexcDates = [
            ("NEWTUSDT".into(), Some(NOW - 5 * DAY)),
            ("MX1USDT".into(), Some(NOW - DAY)),
            ("OLDMUSDT".into(), None),
        ]
        .into();
        let rows = build(&sources(), &dates, NOW);
        let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
        // SOL is on Binance, SUI5S leveraged, AAPLX a stock, OLDG and OLDM too old.
        assert_eq!(symbols, ["NEWT", "SOON", "MX1"]);

        let newt = &rows[0];
        // Earliest of Gate (3 d) and MEXC (5 d).
        assert_eq!(newt.listed_at, NOW - 5 * DAY);
        assert_eq!(newt.age, "5 d ago");
        assert_eq!(newt.volume_usd, 1_500_000.0);
        // From Gate, the busier venue.
        assert_eq!(newt.change_pct, Some(12.0));
        assert!(newt.binance_futures);
        let good: Vec<&str> = newt
            .signals
            .iter()
            .filter(|s| s.good)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(
            good,
            [
                "Binance futures",
                "on Bybit",
                "Gate + MEXC",
                "vol $1.5M",
                "tight spread 12 bps (0.12 %)"
            ]
        );
        assert_eq!(
            newt.summary_line(),
            format!(
                "NEWT (Newt) | listed 5 d ago ({}) | Gate $800k, MEXC $400k, Bybit $300k | vol $1.5M | 24h +12.0 % \
                 | spread 12 bps (0.12 %) | score 7 + Binance futures, on Bybit, Gate + MEXC, vol $1.5M, tight spread 12 bps (0.12 %)",
                newt.listed_date
            )
        );

        assert_eq!(rows[1].age, "in 2 h");
        let mx1 = &rows[2];
        assert!(mx1
            .signals
            .iter()
            .any(|s| !s.good && s.text.starts_with("pumped")));
        // 1.00 / 1.02 on MEXC: about 198 bps.
        assert!(mx1
            .signals
            .iter()
            .any(|s| !s.good && s.text == "wide spread 198 bps (1.98 %)"));
        assert!(mx1.score < 0);
    }

    #[test]
    fn undated_mexc_tokens_wait() {
        let rows = build(&sources(), &MexcDates::new(), NOW);
        assert!(rows.iter().all(|r| r.symbol != "MX1"));
    }

    #[test]
    fn parses_symbols_and_shapes() {
        assert_eq!(binance_base("PEPEUSDT"), Some("PEPE"));
        assert_eq!(binance_base("ETHBTC"), None);
        assert_eq!(futures_base("1000PEPEUSDT"), Some("PEPE"));
        assert_eq!(futures_base("1MBABYDOGEUSDT"), Some("BABYDOGE"));
        assert!(is_leveraged("SUI5S", "SUI5xShort"));
        assert!(!is_leveraged("B3S", "Base Three Social"));
        assert_eq!(fmt_usd(1_234_567.0), "$1.2M");
        assert_eq!(fmt_usd(340_000.0), "$340k");
        assert_eq!(age(NOW - 90 * 60, NOW), "1 h ago");
        assert_eq!(fmt_spread(0.28), "0.3 bps (0.003 %)");
        assert_eq!(fmt_spread(177.4), "177 bps (1.77 %)");
    }

    #[test]
    fn parses_exchange_responses() {
        // Trimmed from real responses.
        let a: AlphaList = serde_json::from_str(
            r#"{"code":"000000","data":[{"symbol":"GSTOCK","name":"Gstock","marketCap":"22553254.66","holders":"10079","listingCex":false}]}"#,
        )
        .unwrap();
        assert_eq!(a.data[0].holders.as_deref(), Some("10079"));
        let m: MexcInfo = serde_json::from_str(
            r#"{"symbols":[{"symbol":"METALUSDT","status":"1","baseAsset":"METAL","quoteAsset":"USDT","fullName":"Metal Blockchain","isSpotTradingAllowed":true,"conceptPlates":["Innovation"]}]}"#,
        )
        .unwrap();
        assert_eq!(m.symbols[0].base_asset, "METAL");
        let t: Vec<MexcTicker> = serde_json::from_str(
            r#"[{"symbol":"METALUSDT","priceChangePercent":"-0.0473","quoteVolume":"7308.89","count":null}]"#,
        )
        .unwrap();
        assert_eq!(num(&t[0].price_change_percent), Some(-0.0473));
        let o: OkxResp =
            serde_json::from_str(r#"{"data":[{"instId":"XASTS-USDT","volCcy24h":"12856.8"}]}"#)
                .unwrap();
        assert_eq!(o.data[0].inst_id, "XASTS-USDT");
        let b: BybitResp = serde_json::from_str(
            r#"{"result":{"list":[{"symbol":"BTCUSDT","turnover24h":"56824.9"}]}}"#,
        )
        .unwrap();
        assert_eq!(num(&b.result.list[0].turnover24h), Some(56824.9));
    }

    #[tokio::test]
    #[ignore = "hits every exchange's live API"]
    async fn live_scan() {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let mut src = Sources::default();
        let errors = src.update(&http).await;
        assert!(errors.is_empty(), "{errors:?}");
        let rows = build(&src, &MexcDates::new(), unix_now());
        for r in rows.iter().take(15) {
            let signals: Vec<&str> = r.signals.iter().map(|s| s.text.as_str()).collect();
            println!(
                "{:>10} {:>12} score {:>2} {:>7} {signals:?}",
                r.symbol,
                r.age,
                r.score,
                fmt_usd(r.volume_usd)
            );
        }
        assert!(!rows.is_empty());
    }
}
