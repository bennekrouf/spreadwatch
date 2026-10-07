# Changelog

What changed in each release of **Spreadwatch**, the desktop app that watches
crypto spreads across exchanges and Solana on-chain.

The public version of this page — with the download for each release — lives at
<https://mayorana.ch/en/apps/spreadwatch/releases>. It is generated from this
file by `scripts/changelog_to_json.py`, so this file is the only place a
release note is written.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each heading is dated on the day its tag was pushed.

## [0.1.6] - 2026-10-07

### Changed

- The publisher information for Spreadwatch has been updated to 'Mayorana'. This
  change affects the metadata associated with the application, such as the
  installer and executable details.

## [0.1.5] - 2026-10-06

### Added

- Spreadwatch now shares anonymous usage statistics: whether it is installed and
  opened, its version and your operating system. It is on by default; a note at
  the bottom of the window tells you once, and nothing is sent before you have
  seen it. **Turn off** there stops it for good and deletes anything not yet
  sent. Your files, data and accounts are never part of it. It also stays off if
  `DO_NOT_TRACK`, `DISABLE_UPDATE_CHECK` or `MAYORANA_NO_TELEMETRY` is set.

## [0.1.4] - 2026-10-02

### Changed

- Packaging only — no user-visible change.

## [0.1.3] - 2026-09-27

### Added

- The version you are running now shows at the right end of the tab bar, so
  it is at hand when you report a problem or check whether an update
  installed.

## [0.1.2] - 2026-09-27

### Changed

- Packaging only — no user-visible change.

## [0.1.1] - 2026-09-27

### Added

- First public release, for macOS (Apple Silicon and Intel), Windows and Linux.
- Follow any asset by ticker and see it quoted against USDT on Binance, Bybit,
  OKX, MEXC and Gate, plus Jupiter for Solana tokens, with the best
  cross-venue edges net of fees and which venue's price trails the others.
- A New listings tab: tokens Gate or MEXC just listed that Binance spot does
  not, scored by volume, other listings, market cap and Binance's interest.
- SOL/USDT swaps through Jupiter from a hot wallet the app creates, with a dry
  run at every start and hard per-trade and daily limits.
- The app tells you when a newer version is available, with a link to it.

### Changed

- The app was called SOL Spread. Its settings folder, with your hot wallet,
  watchlist and `trade.toml`, moves from `~/.config/sol-spread` to
  `~/.config/spreadwatch` the first time you start it; nothing to do by hand.
  A `keypair_path` in `trade.toml` that still points into the old folder keeps
  working, and the trade panel asks you to update it.
