# govee2mqtt

Rust project that bridges Govee smart home devices to MQTT / Home Assistant.

## Build & Test

```bash
cargo build --all
cargo test --all -- --show-output
cargo clippy --all -- -D warnings
cargo fmt --all -- --check
```

### Toolchain

Requires **Rust ≥ 1.85**. The crate itself is `edition = "2021"`, but transitive
dependencies are not: `clap`, `getrandom` and `uuid` are `edition2024`. On an older
default toolchain the build fails with `feature 'edition2024' is required` — fix it with
`rustup default stable`. There is deliberately no `rust-toolchain.toml`; CI installs
`dtolnay/rust-toolchain@stable`.

The first build takes a minute or two: `mosquitto-rs` and `openssl` compile vendored
OpenSSL, which needs `cc`, `perl` and `make` on the box.

## Project Structure

- `src/` — Rust source code
- `addon/` — Home Assistant app (add-on) configuration
- `scripts/` — Build and release scripts
- `docs/` — Documentation
- `test-data/` — Test fixtures

## Running the bridge locally

`govee serve` is headless and **requires an MQTT broker** — it exits without
`--mqtt-host` (or `$GOVEE_MQTT_HOST`). For local testing, run Mosquitto with an anonymous
listener (`listener 1883 127.0.0.1` + `allow_anonymous true`), then:

```bash
RUST_LOG=govee=info cargo run -- serve \
  --mqtt-host 127.0.0.1 --mqtt-port 1883 --http-port 8056
```

No Govee credentials or hardware are needed to smoke-test this. The bridge connects to
MQTT, publishes Home Assistant discovery for its own service device, and serves the web
UI; the device list stays empty, which is expected.

- **MQTT layout.** Discovery goes under the `homeassistant/` prefix (`--hass-discovery-prefix`).
  The bridge's own topics use `gv2mqtt/` — availability at `gv2mqtt/availability`, commands
  like `gv2mqtt/purge-caches`. Watch it all with `mosquitto_sub -h 127.0.0.1 -t '#' -v`.
- **HTTP.** Web UI on `http://localhost:8056/` (redirects to `/assets/index.html`), REST at
  `/api/devices`. The UI pulls `lit` and `timeago.js` from unpkg/jsdelivr, so it renders
  fully only with internet egress.
- **Credentials.** Real devices, scenes and cloud status need `GOVEE_EMAIL` /
  `GOVEE_PASSWORD` and/or `GOVEE_API_KEY` — see [`docs/CONFIG.md`](docs/CONFIG.md). None of
  them are required to build, test, or smoke-test.

## CI

PRs must pass `cargo build`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo fmt --check` (see `.github/workflows/pr.yml`).

The fork also runs Claude Code CI (`.github/workflows/claude.yml`).

## Pre-commit Hooks

The repo includes `.pre-commit-config.yaml` with local hooks for `cargo fmt` and `cargo clippy`. To enable:

```bash
pip install pre-commit
pre-commit install
```

<!-- BEGIN: commit-message-standards (managed by bootstrap-repo.sh — do not hand-edit) -->
## Commit message standards

This repo follows the [engineering-standards commit-message spec](https://github.com/florianhorner/engineering-standards/blob/main/specs/commit-message-spec.md).

**Quick rule:** Conventional Commits (`type(scope): subject`, ≤72 chars). A `Why:` body line is REQUIRED when type is `feat` AND >50 lines changed; otherwise optional.

**Local invocation:** Use the `/commit` skill in Claude Code / Conductor. Default behavior is dry-run (drafts a message and shows the validator output without committing); pass `--commit` to actually create the commit. Manual `git commit` works too — the local `commit-msg` hook validates either path.

**Per-repo cheat sheet:** [`./CONTRIBUTING.md`](./CONTRIBUTING.md) carries the 30-second cheat sheet, good/bad examples, banned patterns, exempt subjects, bot allowlist, and bypass policy. It is self-sufficient for cloud agents (Claude Code Cloud, Codex web) that only see repo-local files.

**Machine-readable rules:** [`.config/commit-rules.json`](.config/commit-rules.json) is a SHA-pinned vendored copy of the upstream `commit-rules.json`. The validator binary, commit-msg hook, and CI workflow all read this file. Do not hand-edit — re-run `bootstrap-repo.sh` to refresh.

**Bypass:** `git commit --no-verify` requires a `Policy-Override: <reason>` trailer to pass CI. Logged to `~/.commit-bypass.log` by the pre-push hook.
<!-- END: commit-message-standards -->
