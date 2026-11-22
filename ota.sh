#!/bin/sh
# Build the firmware and push it to the SmallTV over Wi-Fi.
# Usage: ./ota.sh [device-ip]   (reads WIFI_SSID, WIFI_PASS, DEVICE_TOKEN from .env)
set -eu
cd "$(dirname "$0")"
ip=${1:-10.0.3.250}
set -a; . ./.env; set +a
[ -f "$HOME/export-esp.sh" ] && . "$HOME/export-esp.sh"
cargo build --release
mkdir -p dist
espflash save-image --chip esp32 --flash-size 8mb \
  target/xtensa-esp32-espidf/release/geekmagic-tokens dist/geekmagic-tokens.bin
curl -sS --fail-with-body --max-time 120 -H "X-Device-Token: $DEVICE_TOKEN" \
  --data-binary @dist/geekmagic-tokens.bin "http://$ip/update"
