#!/usr/bin/env bash
# Builds a self-contained NetWeevil archive: one executable with the web
# console embedded, plus a start script. Hand the archive to someone and they
# can extract it and double-click the start script; nothing else to install.
#
#   scripts/package_release.sh            # dist/netweevil-<version>-<os>-<arch>.tar.gz (or .zip on Windows)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
NAME="netweevil-${VERSION}-${OS}-${ARCH}"
OUT="$ROOT/dist/$NAME"

echo "==> Building the web console"
(cd frontend && pnpm install --frozen-lockfile && pnpm build)
echo "==> Building the executable with the console embedded"
cargo build --release -p netweevil-cli

rm -rf "$OUT"
mkdir -p "$OUT"
cp target/release/netweevil "$OUT/"
cp LICENSE LICENSE-MIT LICENSE-APACHE "$OUT/" 2>/dev/null || true
cat > "$OUT/start-netweevil.sh" <<'START'
#!/usr/bin/env bash
# Starts NetWeevil and opens the console. Data goes to ~/NetWeevil
# (set NETWEEVIL_WORKSPACE to change that). Stop with Ctrl+C.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="${NETWEEVIL_WORKSPACE:-$HOME/NetWeevil}"
mkdir -p "$WORKSPACE"
cd "$WORKSPACE"
exec "$HERE/netweevil" bootstrap --bind "${NETWEEVIL_BIND:-127.0.0.1:8080}"
START
chmod +x "$OUT/start-netweevil.sh" "$OUT/netweevil"
cat > "$OUT/README.txt" <<'TXT'
NetWeevil

1. Run start-netweevil.sh (macOS/Linux). Your browser opens the console.
2. Use Setup (bottom left): add a street network (OpenStreetMap .osm.pbf
   or Overture .parquet), create the default profiles, optionally add GTFS.
3. Everything NetWeevil creates lives in ~/NetWeevil/.netweevil. Your source
   files are never modified.

Command line: ./netweevil --help
TXT
(cd dist && tar -czf "$NAME.tar.gz" "$NAME")
echo "==> dist/$NAME.tar.gz"
