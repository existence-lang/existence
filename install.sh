#!/bin/sh
# install.sh — existence installer
#
# Downloads the latest prebuilt binaries from GitHub Releases and installs them
# to ~/.cargo/bin/ (if it exists) or ~/.local/bin/ (created if needed).
#
# Supports: Linux (x86_64, aarch64), macOS (x86_64, aarch64)
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/existence-lang/existence/main/install.sh | sh
#
# Options (via environment variables):
#   EXISTENCE_INSTALL_DIR  Override install directory (default: ~/.cargo/bin or ~/.local/bin)
#   EXISTENCE_VERSION      Install a specific version (default: latest)

set -eu

REPO="existence-lang/existence"
GITHUB_API="https://api.github.com/repos/${REPO}/releases/latest"
GITHUB_DL="https://github.com/${REPO}/releases/download"

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------

detect_target() {
  OS="$(uname -s)"
  ARCH="$(uname -m)"

  case "$OS" in
    Linux)
      case "$ARCH" in
        x86_64 | amd64)  echo "x86_64-unknown-linux-gnu" ;;
        aarch64 | arm64) echo "aarch64-unknown-linux-gnu" ;;
        *) unsupported "$OS" "$ARCH" ;;
      esac
      ;;
    Darwin)
      case "$ARCH" in
        x86_64 | amd64)  echo "x86_64-apple-darwin" ;;
        aarch64 | arm64) echo "aarch64-apple-darwin" ;;
        *) unsupported "$OS" "$ARCH" ;;
      esac
      ;;
    *) unsupported "$OS" "$ARCH" ;;
  esac
}

unsupported() {
  echo "ERROR: Unsupported platform: $1 $2" >&2
  echo "  See https://github.com/${REPO}/releases for manual download." >&2
  exit 1
}

# ---------------------------------------------------------------------------
# Install directory selection
# ---------------------------------------------------------------------------

select_install_dir() {
  if [ -n "${EXISTENCE_INSTALL_DIR:-}" ]; then
    echo "$EXISTENCE_INSTALL_DIR"
  elif [ -d "${HOME}/.cargo/bin" ]; then
    echo "${HOME}/.cargo/bin"
  else
    echo "${HOME}/.local/bin"
  fi
}

# ---------------------------------------------------------------------------
# Dependency checks
# ---------------------------------------------------------------------------

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "ERROR: Required command not found: $1" >&2
    exit 1
  fi
}

need_cmd curl
need_cmd grep
need_cmd sed
need_cmd tar

# ---------------------------------------------------------------------------
# Resolve release tag
# ---------------------------------------------------------------------------

echo "Installing existence..."
echo ""

if [ -n "${EXISTENCE_VERSION:-}" ]; then
  TAG="$EXISTENCE_VERSION"
  echo "Requested version: ${TAG}"
else
  echo "Fetching latest release..."
  TAG="$(curl -s "${GITHUB_API}" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"

  if [ -z "${TAG}" ]; then
    echo "ERROR: Could not determine latest release tag." >&2
    echo "  Check your internet connection or visit: https://github.com/${REPO}/releases" >&2
    exit 1
  fi
  echo "Latest release: ${TAG}"
fi

# ---------------------------------------------------------------------------
# Build download URL
# Release assets: existence-{target}.tar.gz
# ---------------------------------------------------------------------------

TARGET="$(detect_target)"
ARCHIVE_NAME="existence-${TARGET}.tar.gz"
DOWNLOAD_URL="${GITHUB_DL}/${TAG}/${ARCHIVE_NAME}"

echo "Target   : ${TARGET}"
echo "Archive  : ${ARCHIVE_NAME}"

# ---------------------------------------------------------------------------
# Select and prepare install directory
# ---------------------------------------------------------------------------

INSTALL_DIR="$(select_install_dir)"

if [ ! -d "${INSTALL_DIR}" ]; then
  echo "Creating install directory: ${INSTALL_DIR}"
  mkdir -p "${INSTALL_DIR}"
fi

# ---------------------------------------------------------------------------
# Download and extract
# ---------------------------------------------------------------------------

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo ""
echo "Downloading ${DOWNLOAD_URL}..."
curl -fSL --progress-bar "${DOWNLOAD_URL}" -o "${TMP_DIR}/${ARCHIVE_NAME}"

echo "Extracting..."
tar xzf "${TMP_DIR}/${ARCHIVE_NAME}" -C "${TMP_DIR}"

# Move binaries to install directory
mv "${TMP_DIR}/existence" "${INSTALL_DIR}/"
mv "${TMP_DIR}/xist" "${INSTALL_DIR}/"
chmod +x "${INSTALL_DIR}/existence" "${INSTALL_DIR}/xist"

# ---------------------------------------------------------------------------
# Verify installation
# ---------------------------------------------------------------------------

echo ""
echo "Verifying installation..."
if "${INSTALL_DIR}/existence" --version && "${INSTALL_DIR}/xist" --version; then
  echo ""
  echo "existence installed successfully to ${INSTALL_DIR}"
else
  echo "ERROR: Installed binaries failed to run." >&2
  echo "  The downloaded binaries may not be compatible with this system." >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# PATH guidance
# ---------------------------------------------------------------------------

case ":${PATH}:" in
  *":${INSTALL_DIR}:"*)
    ;;
  *)
    echo ""
    echo "NOTE: ${INSTALL_DIR} is not in your PATH."
    echo "      Add it with:"
    echo ""
    echo "        export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo ""
    echo "      Add to ~/.bashrc or ~/.zshrc to make permanent."
    ;;
esac
