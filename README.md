# geekmagic-tokens

Rust firmware that turns a GeekMagic SmallTV Pro into a standalone Codex usage display. It joins Wi‑Fi, keeps its own Codex login, and polls the 5-hour session and weekly limits once a minute. No computer is needed once it is set up.

![Codex screen](docs/screen.png)

- Bars show percent used, with a reset countdown in the corner (`3h 20m`, `4d 1h`).
- A tap on the top of the case switches between the Codex and Claude Code screens. Claude Code is a placeholder until Anthropic offers an API the device can poll (see [docs/plan.md](docs/plan.md)).
- The footer shows the device IP, or `Updated Xm ago` once the last reading is more than 3 minutes old.

## Hardware

SmallTV Pro (ESP32, 8 MB flash, 240×240 ST7789, capacitive touch pad on the top). USB‑C is power only; the serial port is a UART header inside the case, needed only for recovery.

| GPIO | Use |
|---|---|
| 18, 23 | SPI clock, MOSI |
| 3 | Display chip select (also UART0 RX, so serial input stops once the display starts) |
| 2, 4 | Display DC, reset |
| 21 | Backlight |
| 32 (T9) | Touch pad |

Some board revisions wire the display to HSPI (clock 14, MOSI 13). If the panel stays blank after flashing, change those two pins in `src/main.rs`.

## Setup

One-time host setup installs the Xtensa Rust toolchain, `espflash` and `ldproxy`:

```sh
./setup.sh
```

Then create `.env` (gitignored, since the values end up in the firmware):

```sh
WIFI_SSID=your-network
WIFI_PASS=your-password
DEVICE_TOKEN=$(openssl rand -hex 16)   # paste the generated value
```

`DEVICE_TOKEN` protects the endpoints that change the device. The build fails without it.

## Installing

**First time, from the stock firmware.** Build the image, then upload `dist/geekmagic-tokens.bin` through the stock updater at `http://<device-ip>/update` (the stock firmware shows its IP on screen):

```sh
./ota.sh <device-ip>   # builds dist/geekmagic-tokens.bin; the final upload fails on stock, which is fine
```

**After that,** every build goes over Wi‑Fi:

```sh
./ota.sh                # defaults to 10.0.3.250
```

The device reboots into the new image in a few seconds and shows its IP once it is back on Wi‑Fi.

## Signing in to Codex

The device needs its own Codex login. Don't copy your laptop's `~/.codex/auth.json`: OpenAI rotates refresh tokens on every refresh, so two holders of one login keep signing each other out.

```sh
CODEX_HOME=.device-auth codex login --device-auth   # approve the code at auth.openai.com/codex/device
. ./.env
curl -H "X-Device-Token: $DEVICE_TOKEN" --data-binary @.device-auth/auth.json http://10.0.3.250/auth
rm .device-auth/auth.json   # the device rotates the token straight away, so this copy is dead
```

The device stores the refresh token in NVS, refreshes it itself, and saves each new one before using it. Repeat these steps if the screen says `Sign in again`.

## Endpoints

| Method and path | Token | What it does |
|---|---|---|
| `GET /status` | no | Uptime, last redraw, Wi‑Fi join stats, poll count, last error, current reading |
| `GET /debug/screen.bmp` | no | Renders what the panel currently shows |
| `POST /update` | yes | Raw app image (body) → OTA write and reboot |
| `POST /auth` | yes | Codex `auth.json` (body) → stored in NVS, polls immediately |
| `POST /debug/pause?secs=N&age=S` | yes | Stops polling for N seconds and backdates the reading by S seconds |
| `POST /debug/drop?secs=N` | yes | Drops Wi‑Fi and holds off reconnecting for N seconds |

Send the token as `X-Device-Token: $DEVICE_TOKEN`.

## How it works

```mermaid
flowchart TB
    subgraph main thread
        T[touch.rs<br/>tap detect] --> L[draw loop]
        L --> D[display.rs<br/>ST7789, 8-row strips] --> U[ui.rs]
    end
    subgraph net thread
        N[net.rs<br/>Wi-Fi + HTTP server] --> C[codex.rs<br/>refresh + poll]
    end
    C -- shared State --> L
    N -- /auth, reconnect --> C
    Mac -. ota.sh / curl .-> N
    C <-- HTTPS --> O[auth.openai.com<br/>chatgpt.com]
```

- `main.rs` sets up the panel and touch pad, then runs the draw loop: redraw on a tap, on new data, and every 30 s so countdowns tick. Wi‑Fi starts on a background thread so the button works while it connects.
- `net.rs` joins Wi‑Fi (retrying until it has an address), rejoins after drops, and serves the endpoints.
- `codex.rs` refreshes the token at `auth.openai.com/oauth/token` and reads `chatgpt.com/backend-api/wham/usage`, the same endpoints and client ID the Codex CLI uses. `primary_window` is the session and `secondary_window` the weekly limit.
- `display.rs` drives the ST7789 directly and renders 8 rows at a time to leave RAM for TLS.
- `ui.rs` draws the screens with `embedded-graphics`.

The quota endpoint is internal to ChatGPT and could change without notice. If it does, `/status` will show the parse or HTTP error.

## Logos

`assets/codex.raw` and `assets/claude-code.raw` are 32×32 big-endian RGB565 images drawn on black, generated from the SVGs next to them (from [Lobe Icons](https://github.com/lobehub/lobe-icons)):

```sh
swift scripts/logo.swift assets/codex.svg 32 assets/codex.raw preview.png
```

## Recovery

If OTA is unreachable (bad image, wrong Wi‑Fi), flash over the UART header with a USB serial adapter: hold GPIO0 low at power-on, then

```sh
espflash flash --chip esp32 target/xtensa-esp32-espidf/release/geekmagic-tokens
```

To go back to stock, the vendor image is `firmware/PRO_V3.3.75EN` in [GeekMagicClock/smalltv-pro](https://github.com/GeekMagicClock/smalltv-pro); check it against the published MD5 before flashing.

## Troubleshooting

- **Usage request fails with `ESP_FAIL`:** the access token is about 1.8 KB, so the HTTP client needs `buffer_size_tx` of 4 KB (already set in `codex.rs`).
- **Build changes to `sdkconfig.defaults` don't apply:** `ESP_IDF_SDKCONFIG_DEFAULTS` in `.cargo/config.toml` points at it; touching the file triggers an ESP-IDF rebuild, which needs network access to Espressif and PyPI.
- **Xtensa LLVM crash deserializing JSON:** use `f64`, not `f32`, for floats.
