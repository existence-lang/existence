#!/bin/sh
# existence installer — downloads a prebuilt release binary for this machine.
#
#   curl -fsSL https://raw.githubusercontent.com/existence-lang/existence/main/install.sh | sh
#
# Environment:
#   EXISTENCE_VERSION      release tag to install (default: latest release, e.g. v0.4.1)
#   EXISTENCE_INSTALL_DIR  directory to install into
#                          (default: $HOME/.cargo/bin if it exists, else $HOME/.local/bin)
#   EXISTENCE_REPO         GitHub repo (default: existence-lang/existence)
#
# Supported: Linux (x86_64, aarch64; static musl builds), macOS (Intel, Apple
# Silicon), and Windows under Git Bash / MSYS2 (x86_64). On native Windows use
# install.ps1 instead.

set -eu

REPO="${EXISTENCE_REPO:-existence-lang/existence}"
VERSION="${EXISTENCE_VERSION:-}"
if [ -n "${EXISTENCE_INSTALL_DIR:-}" ]; then
  INSTALL_DIR="$EXISTENCE_INSTALL_DIR"
elif [ -d "$HOME/.cargo/bin" ]; then
  INSTALL_DIR="$HOME/.cargo/bin"
else
  INSTALL_DIR="$HOME/.local/bin"
fi

say() { printf '%s\n' "existence-install: $*" >&2; }
die() { say "error: $*"; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"; }

# --- fetch helper: curl preferred, wget fallback ---------------------------
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 --retry-delay 1 "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q "$1" -O "$2"; }
else
  die "need curl or wget"
fi

# --- detect platform --------------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"

case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) die "unsupported architecture: $arch" ;;
esac

ext=tar.gz
case "$os" in
  Linux)  target="${arch}-unknown-linux-musl" ;;
  Darwin) target="${arch}-apple-darwin" ;;
  MINGW*|MSYS*|CYGWIN*|Windows_NT)
    [ "$arch" = x86_64 ] || die "Windows builds are x86_64 only"
    target="x86_64-pc-windows-msvc"; ext=zip ;;
  *) die "unsupported OS: $os (use install.ps1 on native Windows)" ;;
esac

# --- resolve download URL ---------------------------------------------------
# With no version, use the releases/latest/download redirect: it needs no API
# call, so it is immune to the unauthenticated GitHub API rate limit that bites
# shared CI runners.
asset="existence-${target}.${ext}"
if [ -z "$VERSION" ]; then
  VERSION=latest
  url="https://github.com/${REPO}/releases/latest/download/${asset}"
else
  case "$VERSION" in v*) ;; *) VERSION="v${VERSION}" ;; esac
  url="https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
fi

# --- download + extract -----------------------------------------------------
tmp="$(mktemp -d 2>/dev/null || mktemp -d -t existence)"
trap 'rm -rf "$tmp"' EXIT

say "downloading ${asset} (${VERSION})"
fetch "$url" "$tmp/$asset" || die "download failed: $url"

case "$ext" in
  tar.gz) need tar; tar -xzf "$tmp/$asset" -C "$tmp" ;;
  zip)
    if command -v unzip >/dev/null 2>&1; then unzip -q "$tmp/$asset" -d "$tmp"
    else need tar; tar -xf "$tmp/$asset" -C "$tmp"; fi ;;
esac

bin_suffix=""
[ "$ext" = zip ] && bin_suffix=".exe"
[ -f "$tmp/existence${bin_suffix}" ] || die "archive did not contain the existence binary"

mkdir -p "$INSTALL_DIR"
for b in existence xist; do
  if [ -f "$tmp/${b}${bin_suffix}" ]; then
    install -m 755 "$tmp/${b}${bin_suffix}" "$INSTALL_DIR/${b}${bin_suffix}" 2>/dev/null \
      || { cp "$tmp/${b}${bin_suffix}" "$INSTALL_DIR/${b}${bin_suffix}" && chmod 755 "$INSTALL_DIR/${b}${bin_suffix}"; }
  fi
done

# --- verify -----------------------------------------------------------------
installed="$("$INSTALL_DIR/existence${bin_suffix}" --version 2>/dev/null || true)"
[ -n "$installed" ] || die "installed binary failed to run: $INSTALL_DIR/existence${bin_suffix}"
say "installed ${installed} -> ${INSTALL_DIR}"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "note: ${INSTALL_DIR} is not on your PATH; add it, e.g.:"
     say "  export PATH=\"${INSTALL_DIR}:\$PATH\"" ;;
esac
