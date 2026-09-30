#!/usr/bin/env bash
set -euo pipefail

BIN_SRC="/home/m00n/projects/wakezilla/repo/target/release/wakezilla"
BIN_DST="/usr/local/bin/wakezilla"
SERVICE="wakezilla"

if [[ $EUID -ne 0 ]]; then
  echo "This script must run as root. Re-run with:" >&2
  echo "  sudo bash $0" >&2
  exit 1
fi

if [[ ! -f "$BIN_SRC" ]]; then
  echo "ERROR: built binary not found at $BIN_SRC" >&2
  echo "Build it first: (cd /home/m00n/projects/wakezilla/repo && PATH=\"\$HOME/.cargo/bin:\$PATH\" WAKEZILLA_SKIP_FRONTEND_BUILD=1 cargo build --release -p wakezilla)" >&2
  exit 1
fi

ts="$(date +%Y%m%d-%H%M%S)"
if [[ -f "$BIN_DST" ]]; then
  echo "Backing up current binary -> ${BIN_DST}.bak-${ts}"
  cp -a "$BIN_DST" "${BIN_DST}.bak-${ts}"
fi

echo "Stopping ${SERVICE} ..."
systemctl stop "$SERVICE"

echo "Installing new binary -> ${BIN_DST}"
# install writes a new file instead of overwriting in place, so it works while
# another process (the local wakezilla-client) is running the old binary.
if ! install -m 755 "$BIN_SRC" "$BIN_DST"; then
  echo "ERROR: install failed; restarting ${SERVICE} with the old binary" >&2
  systemctl start "$SERVICE"
  exit 1
fi

echo "Starting ${SERVICE} ..."
systemctl start "$SERVICE"

if systemctl is-enabled --quiet wakezilla-client 2>/dev/null; then
  echo "Restarting wakezilla-client (same binary) ..."
  systemctl restart wakezilla-client
fi

sleep 1
echo "----- status -----"
systemctl --no-pager status "$SERVICE" | head -10
