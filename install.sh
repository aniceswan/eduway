#!/usr/bin/env bash
# Install eduway without Rust. Downloads the latest GitHub release binary.
# Usage:
#   ./install.sh                                  # needs EDUWAY_REPO set below or via env
#   EDUWAY_REPO=myuser/eduway ./install.sh
set -euo pipefail

REPO="${EDUWAY_REPO:-OWNER/eduway}"
if [[ "$REPO" == OWNER/* ]]; then
  echo "Set dulu repo GitHub kamu:"
  echo "  EDUWAY_REPO=username/namarepo ./install.sh"
  echo "(push repo ini ke GitHub, push tag v0.1.0, tunggu Actions hijau, baru install.sh bisa download)"
  exit 1
fi

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
  Linux)  ASSET="eduway-Linux.tar.gz"; BIN_DIR="$HOME/.local/bin" ;;
  Darwin) ASSET="eduway-macOS.tar.gz"; BIN_DIR="$HOME/.local/bin" ;;
  MINGW*|MSYS*|CYGWIN*) ASSET="eduway-Windows-Setup.exe"; BIN_DIR="$HOME/bin" ;;
  *) echo "OS tidak dikenal: $OS"; exit 1 ;;
esac
[[ "$ARCH" != x86_64 && "$ARCH" != arm64 && "$ARCH" != aarch64 ]] && echo "arsitektur $ARCH belum ada build-nya" && exit 1

URL="https://github.com/$REPO/releases/latest/download/$ASSET"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
echo "download $URL ..."
curl -fsSL "$URL" -o "$TMP/pkg"
mkdir -p "$BIN_DIR"
if [[ "$ASSET" == *.tar.gz ]]; then
  tar xzf "$TMP/pkg" -C "$TMP"
  cp "$TMP/eduway" "$BIN_DIR/"
  chmod +x "$BIN_DIR/eduway"
elif [[ "$ASSET" == *.exe ]]; then
  cp "$TMP/pkg" "$BIN_DIR/eduway-setup.exe"
  echo "Windows: jalankan $BIN_DIR/eduway-setup.exe untuk install"
  exit 0
else
  cp "$TMP/pkg" "$BIN_DIR/eduway.exe"
fi

# linux desktop entry + icon
if [[ "$OS" == "Linux" && -f "$TMP/eduway.desktop" ]]; then
  mkdir -p "$HOME/.local/share/applications" "$HOME/.local/share/icons/hicolor/256x256/apps"
  cp "$TMP/eduway.desktop" "$HOME/.local/share/applications/eduway.desktop"
  [[ -f "$TMP/icon-256.png" ]] && cp "$TMP/icon-256.png" "$HOME/.local/share/icons/hicolor/256x256/apps/eduway.png"
fi

export PATH="$BIN_DIR:$PATH"
echo "ok: $(eduway pwd 2>/dev/null || echo installed) -> $BIN_DIR/eduway"
echo "jalanin: eduway"
