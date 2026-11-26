#!/usr/bin/env sh
set -eu

REPO="${AGY_T3_SYNC_REPO:-samudrareggi/agy-t3-sync}"
VERSION="${AGY_T3_SYNC_VERSION:-latest}"
INSTALL_DIR="${AGY_T3_SYNC_INSTALL_DIR:-$HOME/.local/bin}"
NO_AUTO_INSTALL="${AGY_T3_SYNC_NO_AUTO_INSTALL:-0}"

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: required command '$1' is not installed." >&2
    exit 1
  fi
}

need_cmd curl
need_cmd tar

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64|amd64) TARGET="linux-x86_64" ;;
      aarch64|arm64) TARGET="linux-aarch64" ;;
      *) echo "Unsupported Linux architecture: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  Darwin)
    case "$ARCH" in
      arm64|aarch64) TARGET="darwin-aarch64" ;;
      x86_64) TARGET="darwin-x86_64" ;;
      *) echo "Unsupported macOS architecture: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  *)
    echo "Unsupported OS: $OS (use install.ps1 on Windows)" >&2
    exit 1
    ;;
esac

ARCHIVE="agy-t3-sync-${TARGET}.tar.gz"
if [ "$VERSION" = "latest" ]; then
  BASE_URL="https://github.com/${REPO}/releases/latest/download"
else
  BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
fi

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

echo "Downloading ${ARCHIVE} from ${BASE_URL}..."
curl -fsSL "${BASE_URL}/${ARCHIVE}" -o "${TMP_DIR}/${ARCHIVE}"
curl -fsSL "${BASE_URL}/SHA256SUMS" -o "${TMP_DIR}/SHA256SUMS"

EXPECTED_SHA="$(awk -v file="$ARCHIVE" '$2 == file || $2 == ("*" file) { print $1; exit }' "${TMP_DIR}/SHA256SUMS")"
if [ -z "$EXPECTED_SHA" ]; then
  echo "Error: checksum for ${ARCHIVE} not found in SHA256SUMS." >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL_SHA="$(sha256sum "${TMP_DIR}/${ARCHIVE}" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  ACTUAL_SHA="$(shasum -a 256 "${TMP_DIR}/${ARCHIVE}" | awk '{print $1}')"
else
  echo "Error: neither sha256sum nor shasum found to verify download." >&2
  exit 1
fi

if [ "$EXPECTED_SHA" != "$ACTUAL_SHA" ]; then
  echo "Error: SHA-256 mismatch for ${ARCHIVE}!" >&2
  echo "  expected: $EXPECTED_SHA" >&2
  echo "  actual:   $ACTUAL_SHA" >&2
  exit 1
fi
echo "Checksum verified (${ACTUAL_SHA})."

tar -xzf "${TMP_DIR}/${ARCHIVE}" -C "${TMP_DIR}"
mkdir -p "$INSTALL_DIR"
install -m 755 "${TMP_DIR}/agy-t3-sync" "${INSTALL_DIR}/agy-t3-sync"
echo "Installed ${INSTALL_DIR}/agy-t3-sync"

case ":${PATH:-}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo "Note: ${INSTALL_DIR} is not currently in PATH. Add to your shell profile:"
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    ;;
esac

echo ""
"${INSTALL_DIR}/agy-t3-sync" doctor || true

if [ "$NO_AUTO_INSTALL" != "1" ]; then
  echo ""
  "${INSTALL_DIR}/agy-t3-sync" install
fi
