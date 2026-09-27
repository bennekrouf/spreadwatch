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

## [Unreleased]

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
