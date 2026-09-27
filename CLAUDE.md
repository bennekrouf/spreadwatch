# Spreadwatch

## Release notes

Every change a user of Spreadwatch could notice gets a note in `CHANGELOG.md`,
in the same PR as the change. The notes are published as-is at
<https://mayorana.ch/en/apps/spreadwatch/releases> and in the GitHub Release
body.

- Add bullets under `## [Unreleased]`, grouped under `### Added`, `### Changed`,
  `### Fixed` or `### Removed`. If there is no `[Unreleased]` heading, create
  it directly above the newest version.
- Never write a version heading yourself. `scripts/release.sh` renames
  `[Unreleased]` to the version and date when it cuts the release.
- Write for someone using the app, not for a reviewer: what they will see or no
  longer run into, and why it matters. No function, module or file names from
  the codebase; the files, tabs and settings the user works with are fine.
- Wrap at 80 columns and match the tone of the existing entries.
- No user-visible change (tests, refactors, CI, docs)? Write no note and put the
  `no-notes` label on the PR. The `Release notes` check fails a PR that
  changes app files without either.

## Secrets

The hot wallet key and `trade.toml` (Jupiter API key, RPC URL with a token)
never go into the repository, not even as test fixtures. `tests/no_secrets.rs`
scans every tracked file and `.githooks/pre-commit` runs it before each commit;
use a placeholder like `"..."` in examples rather than weakening the scan.
