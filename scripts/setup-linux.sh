#!/usr/bin/env bash
# setup-linux.sh — one-time setup for Spreadwatch on Linux (Debian/Ubuntu/Fedora/Arch)
#
# Installs the runtime libraries the app links (WebKitGTK, libxdo, OpenSSL 3)
# and adds a launcher with the app icon. Already-installed packages are skipped.
#
# Usage (from the extracted release archive):
#   sudo ./setup-linux.sh

set -euo pipefail

info()  { echo -e "\033[34m[info]\033[0m  $*"; }
ok()    { echo -e "\033[32m[ok]\033[0m    $*"; }
skip()  { echo -e "\033[33m[skip]\033[0m  $*"; }
err()   { echo -e "\033[31m[error]\033[0m $*"; exit 1; }

[[ $EUID -eq 0 ]] || err "Run with sudo: sudo ./setup-linux.sh"

# ── Detect distro ─────────────────────────────────────────────────────────────
if   command -v apt-get &>/dev/null; then DISTRO=debian
elif command -v dnf     &>/dev/null; then DISTRO=fedora
elif command -v pacman  &>/dev/null; then DISTRO=arch
else err "Unsupported distro — install WebKitGTK 4.1, libxdo and OpenSSL 3 manually"
fi
info "Detected distro family: $DISTRO"

# ── Runtime libraries ─────────────────────────────────────────────────────────
has_lib() { ldconfig -p 2>/dev/null | grep -q "$1"; }

case "$DISTRO" in
  debian)
    PKGS=()
    has_lib libwebkit2gtk-4.1.so.0 || PKGS+=(libwebkit2gtk-4.1-0)
    has_lib libxdo.so.3            || PKGS+=(libxdo3)
    # Ubuntu 24.04 renamed libssl3 to libssl3t64; install whichever exists.
    if ! has_lib libssl.so.3; then
      if apt-cache show libssl3t64 &>/dev/null; then PKGS+=(libssl3t64); else PKGS+=(libssl3); fi
    fi
    if [ ${#PKGS[@]} -gt 0 ]; then
      info "Installing: ${PKGS[*]}"
      apt-get update -qq
      apt-get install -y "${PKGS[@]}"
      ok "Runtime libraries installed"
    else
      skip "Runtime libraries already installed"
    fi
    ;;
  fedora)
    PKGS=()
    rpm -q webkit2gtk4.1 &>/dev/null || PKGS+=(webkit2gtk4.1)
    rpm -q libxdo &>/dev/null || rpm -q xdotool &>/dev/null || PKGS+=(xdotool)
    rpm -q openssl-libs &>/dev/null || PKGS+=(openssl-libs)
    if [ ${#PKGS[@]} -gt 0 ]; then
      info "Installing: ${PKGS[*]}"
      dnf install -y "${PKGS[@]}"
      ok "Runtime libraries installed"
    else
      skip "Runtime libraries already installed"
    fi
    ;;
  arch)
    PKGS=()
    pacman -Qi webkit2gtk-4.1 &>/dev/null || PKGS+=(webkit2gtk-4.1)
    pacman -Qi xdotool &>/dev/null || PKGS+=(xdotool)
    pacman -Qi openssl &>/dev/null || PKGS+=(openssl)
    if [ ${#PKGS[@]} -gt 0 ]; then
      info "Installing: ${PKGS[*]}"
      pacman -S --noconfirm --needed "${PKGS[@]}"
      ok "Runtime libraries installed"
    else
      skip "Runtime libraries already installed"
    fi
    ;;
esac

# ── Icon + launcher ───────────────────────────────────────────────────────────
BINARY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ICON_DIR="/usr/share/icons/hicolor/256x256/apps"
DESKTOP_FILE="/usr/share/applications/spreadwatch.desktop"

ICON_NAME="utilities-system-monitor"
if [[ -f "$BINARY_DIR/icon.png" ]]; then
  install -Dm644 "$BINARY_DIR/icon.png" "$ICON_DIR/spreadwatch.png"
  command -v gtk-update-icon-cache &>/dev/null && gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true
  ICON_NAME="spreadwatch"
  ok "Icon installed"
fi

# Rewritten on every run, so the launcher follows the archive if it moves.
cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Name=Spreadwatch
Comment=Cross-venue crypto spreads, lead/lag and new listings
Exec=$BINARY_DIR/spreadwatch
Icon=$ICON_NAME
Terminal=false
Type=Application
Categories=Finance;Office;
StartupWMClass=spreadwatch
EOF
command -v update-desktop-database &>/dev/null && update-desktop-database -q /usr/share/applications || true
ok "Launcher created ($DESKTOP_FILE)"

echo ""
echo "Setup complete. Start Spreadwatch from your app launcher, or run ./spreadwatch"
echo "Settings, including the hot wallet, go to ~/.config/spreadwatch (see trade.example.toml)."
