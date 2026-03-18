#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="${PREFIX:-$HOME/.local/bin}"

need_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: required command not found: $cmd" >&2
    return 1
  fi
}

echo "==> Checking dependencies"
need_cmd cargo
need_cmd tmux

echo "==> Building release binaries"
cd "$ROOT_DIR"
cargo build --release

echo "==> Installing to $PREFIX"
mkdir -p "$PREFIX"
install -m 0755 "$ROOT_DIR/target/release/br" "$PREFIX/br"
install -m 0755 "$ROOT_DIR/target/release/b" "$PREFIX/b"
install -m 0755 "$ROOT_DIR/target/release/cx" "$PREFIX/cx"

echo "==> Done"
echo "Installed:"
echo "  $PREFIX/br"
echo "  $PREFIX/b"
echo "  $PREFIX/cx"
echo
echo "If needed, add this to your shell rc:"
echo "  export PATH=\"$PREFIX:\$PATH\""

