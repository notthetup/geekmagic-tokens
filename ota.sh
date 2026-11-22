#!/bin/sh
# Build the firmware and push it to the SmallTV over Wi-Fi.
# Usage: WIFI_SSID=... WIFI_PASS=... ./ota.sh [device-ip]
set -eu
ip=${1:-10.0.3.250}
: "${WIFI_SSID:?set WIFI_SSID}" "${WIFI_PASS:?set WIFI_PASS}"
cargo build --release
mkdir -p dist
espflash save-image --chip esp32 --flash-size 8mb \
  target/xtensa-esp32-espidf/release/geekmagic-tokens dist/geekmagic-tokens.bin
curl -sS --max-time 120 --data-binary @dist/geekmagic-tokens.bin "http://$ip/update"
