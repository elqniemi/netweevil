#!/usr/bin/env bash
# Starts NetWeevil for people who just want to use it: builds what is missing
# (once), then opens the console in the browser. Data goes to ~/NetWeevil
# unless NETWEEVIL_WORKSPACE is set.
#
#   ./scripts/start.sh            # first run builds the console and the binary
#   NETWEEVIL_WORKSPACE=/data ./scripts/start.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKSPACE="${NETWEEVIL_WORKSPACE:-$HOME/NetWeevil}"
BIN="$ROOT/target/release/netweevil"
BIND="${NETWEEVIL_BIND:-127.0.0.1:8080}"

say() { printf '\n==> %s\n' "$*"; }

need_console=0
if [ ! -f "$ROOT/frontend/dist/index.html" ]; then need_console=1; fi
if [ ! -x "$BIN" ] || [ "$need_console" = 1 ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    cat <<MSG

NetWeevil needs to be built once, and the Rust toolchain is not installed.
Install it from https://rustup.rs (one command, a few minutes), then run this
script again. Or download a prebuilt release archive and use its start script.
MSG
    exit 1
  fi
  if [ "$need_console" = 1 ]; then
    if command -v pnpm >/dev/null 2>&1; then
      say "Building the web console (frontend/dist)"
      (cd "$ROOT/frontend" && pnpm install --frozen-lockfile && pnpm build)
    elif command -v npm >/dev/null 2>&1; then
      say "Building the web console with npm (pnpm not found)"
      (cd "$ROOT/frontend" && npm install && npm run build)
    else
      echo "Node.js (with pnpm or npm) is not installed; the console cannot be built."
      echo "Install Node.js from https://nodejs.org and run this script again."
      echo "Continuing with the API only."
    fi
  fi
  say "Building NetWeevil (release, first time takes several minutes)"
  (cd "$ROOT" && cargo build --release -p netweevil-cli)
fi

mkdir -p "$WORKSPACE"
cd "$WORKSPACE"
say "Starting NetWeevil in $WORKSPACE"
exec "$BIN" bootstrap --bind "$BIND"
