//! The repository must not carry a wallet key or an API key.
//!
//! Anyone who reads a Solana keyfile can spend from that wallet, and a pushed
//! secret is public whatever happens to the history afterwards. `.gitignore`
//! keeps the usual file names out, but it does not apply to a file that is
//! already tracked, nor to a key pasted into a test or a doc. So every tracked
//! file is scanned on every `cargo test`, and `.githooks/pre-commit` runs this
//! before each commit.
//!
//! Scope is deliberately narrow — key material and the settings that carry
//! API keys — so it stays quiet enough that nobody is tempted to delete it.
//! When it fires on something genuinely fine, use a placeholder like `"..."`
//! rather than adding an exception.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Settings and headers whose value is an API key. Matched case-insensitively.
const KEY_SETTINGS: &[&str] = &[
    "jupiter_api_key",
    "api-key",
    "api_key",
    "apikey",
    "x-api-key",
];

/// RPC providers that put the access token in the URL path.
const TOKEN_IN_PATH_HOSTS: &[&str] = &["quiknode.pro/", "rpcpool.com/"];

/// Words that make a long base58 string a private key rather than a
/// transaction signature (which has the same length and is public).
const SECRET_WORDS: &[&str] = &["secret", "private", "keypair", "seed", "wallet"];

/// Paths exempt from the scan: this file describes the shapes it forbids.
fn is_exempt(path: &str) -> bool {
    path == "tests/no_secrets.rs"
}

fn is_base58(c: char) -> bool {
    c.is_ascii_alphanumeric() && !matches!(c, '0' | 'O' | 'I' | 'l')
}

/// A Solana CLI keyfile: a JSON array of 64 bytes. Checked on the whole file
/// with whitespace removed, since a pretty-printed one spans many lines.
/// Any run of 32+ small integers counts: half a key is still a leak.
fn keyfile_bytes_in(text: &str) -> Option<String> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    for (at, _) in compact.match_indices('[') {
        let body = &compact[at + 1..];
        let end = body.find(']').unwrap_or(body.len());
        let items: Vec<&str> = body[..end].split(',').collect();
        let bytes = items.iter().take_while(|s| s.parse::<u8>().is_ok()).count();
        if bytes >= 32 {
            return Some(format!("array of {bytes} byte values"));
        }
    }
    None
}

/// A base58 private key, as wallets like Phantom export it (87–88 chars),
/// introduced by a word that says it is secret.
fn base58_secret_in(line: &str) -> Option<String> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        if !is_base58(chars[i].1) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_base58(chars[i].1) {
            i += 1;
        }
        let len = i - start;
        if (80..=90).contains(&len) {
            let from = chars[start.saturating_sub(60)].0;
            let run_up = line[from..chars[start].0].to_ascii_lowercase();
            if SECRET_WORDS.iter().any(|w| run_up.contains(w)) {
                let at = chars[start].0;
                return Some(format!("{}…", &line[at..at + 8]));
            }
        }
    }
    None
}

/// The value following `marker`: skips `=`, `:`, quotes and spaces, then
/// takes the key-shaped run after them.
fn value_after(rest: &str) -> &str {
    let rest = rest.trim_start_matches([' ', '"', '\'', '=', ':']);
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// An API key assigned to one of [`KEY_SETTINGS`], or a token in an RPC URL.
fn api_key_in(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    for marker in KEY_SETTINGS.iter().chain(TOKEN_IN_PATH_HOSTS) {
        for (at, _) in lower.match_indices(marker) {
            let value = value_after(&line[at + marker.len()..]);
            // Short values are placeholders, variable names or test fixtures.
            if value.len() >= 16 {
                return Some(format!("{marker} {}…", &value[..6]));
            }
        }
    }
    None
}

#[test]
fn detectors_catch_what_they_are_for() {
    let key_bytes: Vec<String> = (0..64).map(|i| ((i * 37) % 256).to_string()).collect();
    assert!(keyfile_bytes_in(&format!("[{}]", key_bytes.join(","))).is_some());
    assert!(keyfile_bytes_in(&format!("[\n  {}\n]", key_bytes.join(",\n  "))).is_some());
    assert!(keyfile_bytes_in("[1, 2, 3, 600]").is_none());

    let b58: String = "5Kd3NBUAdUnhyzenEwVLy9pBKxSwXvE9FMPyR4UKZvpe".repeat(2);
    assert!(base58_secret_in(&format!("private_key = \"{b58}\"")).is_some());
    assert!(base58_secret_in(&format!("signature: {b58}")).is_none());

    let key = format!("{}{}", "a1b2c3d4", "e5f6a7b8c9d0");
    assert!(api_key_in(&format!("jupiter_api_key = \"{key}\"")).is_some());
    assert!(api_key_in(&format!("https://mainnet.helius-rpc.com/?api-key={key}")).is_some());
    assert!(api_key_in(&format!("https://x.solana-mainnet.quiknode.pro/{key}/")).is_some());
    assert!(api_key_in("# jupiter_api_key = \"...\"").is_none());
    assert!(api_key_in("rb.header(\"x-api-key\", key)").is_none());
}

#[test]
fn no_wallet_key_or_api_key_in_any_tracked_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .expect("git ls-files");
    assert!(out.status.success(), "git ls-files failed");

    let mut findings: BTreeSet<String> = BTreeSet::new();
    for path in String::from_utf8_lossy(&out.stdout).split('\0') {
        if path.is_empty() || is_exempt(path) {
            continue;
        }
        // Binary and non-UTF-8 files carry nothing this can read. A file
        // staged for deletion is gone from disk and skipped the same way.
        let Ok(text) = std::fs::read_to_string(root.join(path)) else {
            continue;
        };
        if let Some(hit) = keyfile_bytes_in(&text) {
            findings.insert(format!("{path}  wallet keyfile: {hit}"));
        }
        for (n, line) in text.lines().enumerate() {
            let n = n + 1;
            if let Some(hit) = base58_secret_in(line) {
                findings.insert(format!("{path}:{n}  base58 private key: {hit}"));
            }
            if let Some(hit) = api_key_in(line) {
                findings.insert(format!("{path}:{n}  API key: {hit}"));
            }
        }
    }

    assert!(
        findings.is_empty(),
        "\n{} tracked file(s) carry a wallet key or an API key:\n\n{}\n\n\
         Nothing here belongs in this repository. Keys live in the settings \
         folder (~/.config/spreadwatch, %LOCALAPPDATA%\\Spreadwatch on Windows); \
         use a placeholder like \"...\" in examples. If a real wallet key was \
         ever committed, move its funds to a new wallet: rewriting history does \
         not un-publish it.\n",
        findings.len(),
        findings.into_iter().collect::<Vec<_>>().join("\n")
    );
}
