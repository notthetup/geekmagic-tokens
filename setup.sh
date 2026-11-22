#!/bin/sh
# One-time host setup: Xtensa Rust toolchain plus flashing and linking tools.
set -eu
cargo install --locked espup espflash ldproxy
[ -f "$HOME/export-esp.sh" ] || espup install --targets esp32 --export-file "$HOME/export-esp.sh"
[ -f "$(dirname "$0")/.env" ] || echo "Now create .env with WIFI_SSID, WIFI_PASS and DEVICE_TOKEN"
