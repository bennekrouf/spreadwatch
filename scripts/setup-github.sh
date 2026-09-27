#!/usr/bin/env bash
# setup-github.sh — one-time GitHub setup for the release pipeline.
#
# Sets the 17 secrets release.yml reads, using the same signing identities and
# deploy key as ais-runner, and creates the `no-notes` label the Release notes
# check looks for. Secrets are write-only on GitHub, so they cannot be copied
# from the ais-runner repository: this reads them from the original files, or
# asks you to paste them (the input is hidden).
#
# Usage:
#   MACOS_P12=~/path/DeveloperID.p12 \
#   APPSTORE_P8=~/path/AuthKey_XXXXXXXXXX.p8 \
#   ./scripts/setup-github.sh
#
# Optional:
#   REPO=owner/name            default: bennekrouf/spreadwatch
#   DIST_KEY_FILE=path         default: ~/.ssh/ais-dist (the rrsync deploy key)
#   ONLY="NAME1 NAME2"         set only these secrets (e.g. after rotating one)
#
# Re-running is safe: `gh secret set` overwrites, and the label is upserted.

set -euo pipefail

REPO="${REPO:-bennekrouf/spreadwatch}"
DIST_KEY_FILE="${DIST_KEY_FILE:-$HOME/.ssh/ais-dist}"
ONLY="${ONLY:-}"

info() { echo -e "\033[34m[info]\033[0m  $*"; }
ok()   { echo -e "\033[32m[ok]\033[0m    $*"; }
err()  { echo -e "\033[31m[error]\033[0m $*" >&2; exit 1; }

command -v gh >/dev/null || err "Install the GitHub CLI first: brew install gh"
gh auth status >/dev/null 2>&1 || err "Run: gh auth login"
gh repo view "$REPO" >/dev/null || err "Cannot see $REPO — create it first: gh repo create $REPO --public"

wanted() { [[ -z "$ONLY" || " $ONLY " == *" $1 "* ]]; }

# A secret whose value is a whole file, as-is (multi-line keys).
from_file() {
    local name=$1 file=$2
    wanted "$name" || return 0
    [[ -f "$file" ]] || err "$name: $file not found"
    gh secret set "$name" -R "$REPO" < "$file"
    ok "$name (from $file)"
}

# A secret whose value is a file, base64-encoded on one line — how the
# workflow expects the .p12 and .p8 (it decodes them back).
from_file_b64() {
    local name=$1 file=$2
    wanted "$name" || return 0
    [[ -n "$file" && -f "$file" ]] || err "$name: set the path to the file (see Usage at the top)"
    base64 < "$file" | tr -d '\n' | gh secret set "$name" -R "$REPO"
    ok "$name (base64 of $file)"
}

# A short value typed or pasted at the prompt. Input is hidden.
prompted() {
    local name=$1 hint=$2 value
    wanted "$name" || return 0
    read -r -s -p "  $name — $hint: " value; echo
    [[ -n "$value" ]] || err "$name is empty"
    printf '%s' "$value" | gh secret set "$name" -R "$REPO"
    ok "$name"
}

info "Setting secrets on $REPO"

# ── macOS: Developer ID signing + notarization (7) ───────────────────────────
from_file_b64 MACOS_CERTIFICATE "${MACOS_P12:-}"
prompted MACOS_CERTIFICATE_PWD "password of that .p12"
prompted MACOS_SIGNING_IDENTITY 'e.g. Developer ID Application: Name (TEAMID) — see: security find-identity -v -p codesigning'
prompted KEYCHAIN_PASSWORD "any random string (CI creates a throwaway keychain with it)"
from_file_b64 APP_STORE_CONNECT_KEY "${APPSTORE_P8:-}"
prompted APP_STORE_CONNECT_KEY_ID "the 10-character Key ID of that .p8"
prompted APP_STORE_CONNECT_ISSUER_ID "Issuer ID from App Store Connect → Users and Access → Integrations"

# ── Windows: Azure Trusted Signing (6) ───────────────────────────────────────
prompted AZURE_TENANT_ID "tenant of the signing app registration"
prompted AZURE_CLIENT_ID "client (application) id of that app registration"
prompted AZURE_CLIENT_SECRET "its client secret (check its expiry date in Azure)"
prompted TRUSTED_SIGNING_ENDPOINT "e.g. https://weu.codesigning.azure.net/"
prompted TRUSTED_SIGNING_ACCOUNT "Trusted Signing account name"
prompted TRUSTED_SIGNING_CERT_PROFILE "certificate profile name"

# ── mayorana.ch publish over rsync (4) ───────────────────────────────────────
from_file DIST_SSH_KEY "$DIST_KEY_FILE"
prompted DIST_SSH_USER "deploy user on the VPS"
prompted DIST_SSH_HOST "VPS host name"
if wanted DIST_SSH_KNOWN_HOSTS; then
    read -r -p "  DIST_SSH_KNOWN_HOSTS — host to pin (same as DIST_SSH_HOST): " HOST
    KNOWN=$(ssh-keygen -F "$HOST" 2>/dev/null | grep -v '^#' || true)
    if [[ -z "$KNOWN" ]]; then
        info "$HOST is not in ~/.ssh/known_hosts; fetching its keys with ssh-keyscan"
        KNOWN=$(ssh-keyscan -H "$HOST" 2>/dev/null)
    fi
    [[ -n "$KNOWN" ]] || err "Could not get host keys for $HOST"
    printf '%s\n' "$KNOWN" | gh secret set DIST_SSH_KNOWN_HOSTS -R "$REPO"
    ok "DIST_SSH_KNOWN_HOSTS"
fi

# ── Label for PRs with no user-visible change ────────────────────────────────
if [[ -z "$ONLY" ]]; then
    gh label create no-notes -R "$REPO" --force --color C5DEF5 \
        --description "No user-visible change: skips the Release notes check" >/dev/null
    ok "label no-notes"
fi

echo
gh secret list -R "$REPO"
