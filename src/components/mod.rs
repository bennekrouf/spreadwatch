mod event_log;
mod lag;
mod pro;
mod routes;
mod scanner;
mod trade;
mod venue_card;
mod watchlist;

pub use event_log::EventLog;
pub use lag::LagPanel;
pub use pro::{Pro, ProButton, ProWindow};
pub use routes::RoutesPanel;
pub use scanner::NewListingsPanel;
pub use trade::TradePanel;
pub use venue_card::VenueCard;
pub use watchlist::Watchlist;

/// Two independent views: the watchlist token's spreads, and the scanner
/// for new listings on Gate and MEXC that Binance doesn't list yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Market,
    NewListings,
}

/// "+1.16 bps (+0.0116 %)", or a dash when there is nothing to show yet.
pub fn fmt_bps_pct(v: Option<f64>) -> String {
    v.map_or("–".into(), |v| {
        format!("{v:+.2} bps ({:+.4} %)", v / 100.0)
    })
}

/// About six significant digits: 84 909.95, 124.085, 0.0000037045.
pub fn fmt_px(v: Option<f64>) -> String {
    let Some(v) = v else { return "–".into() };
    let digits = if v > 0.0 {
        5 - v.log10().floor() as i32
    } else {
        2
    };
    format!("{v:.*}", digits.clamp(2, 10) as usize)
}
