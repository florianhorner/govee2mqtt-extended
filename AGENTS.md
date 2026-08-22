# AGENTS.md

## Cursor Cloud specific instructions

`govee2mqtt` is a single Rust binary (`govee`) that bridges Govee smart-home devices to
MQTT / Home Assistant. Standard build/test/lint/run commands live in
[`CLAUDE.md`](./CLAUDE.md) and `Cargo.toml` — reference those rather than duplicating them.

### Toolchain (important, non-obvious)

- The project requires **Rust ≥ 1.85** because transitive dependencies use `edition2024`.
  The base VM's `rustup` default is pinned to an older toolchain (1.83), which fails to
  build with `feature 'edition2024' is required`. The startup update script runs
  `rustup default stable` to fix this; if you ever see the `edition2024` error, run
  `rustup default stable` yourself.
- First build is slow (~1–2 min): `mosquitto-rs` and `openssl` compile with vendored
  OpenSSL (needs `cc`/`perl`/`make`, all preinstalled).

### Running the bridge end-to-end (`govee serve`)

- `govee serve` is headless. It **requires an MQTT broker** (`--mqtt-host`). For local
  testing install Mosquitto (system package, intentionally NOT in the update script):
  `sudo apt-get install -y mosquitto mosquitto-clients`, then run a broker with an
  anonymous listener (e.g. `mosquitto -c` with `listener 1883 127.0.0.1` +
  `allow_anonymous true`).
- Smoke test without any Govee credentials/hardware:
  `RUST_LOG=govee=info ./target/debug/govee serve --mqtt-host 127.0.0.1 --mqtt-port 1883 --http-port 8056`.
  It connects to MQTT, publishes Home Assistant discovery for its own "Govee to MQTT"
  service device, and serves a web UI. The device list is empty without credentials —
  that is expected.
- MQTT layout: discovery is published under the `homeassistant/` prefix; the bridge's own
  topics use the `gv2mqtt/` prefix (availability `gv2mqtt/availability`, command topics
  like `gv2mqtt/purge-caches`). Verify with
  `mosquitto_sub -h 127.0.0.1 -t '#' -v`.
- HTTP: web UI on `http://localhost:8056/` (redirects to `/assets/index.html`); REST API
  at `/api/devices`. The web UI loads some assets from public CDNs (unpkg/jsdelivr), so it
  renders best with internet egress.
- Full functionality (real devices, scenes, rooms, cloud status) needs Govee credentials:
  `GOVEE_EMAIL` / `GOVEE_PASSWORD` and/or `GOVEE_API_KEY` (see `docs/CONFIG.md`). These are
  not required to build, test, or smoke-test the bridge.
