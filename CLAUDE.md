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

Requires **Rust ≥ 1.85**. The crate itself is `edition = "2021"`; the `uuid` dependency
sets the Rust 1.85 floor, while some packages pulled in by `clap` and `getrandom` use
edition 2024. On an older default toolchain the build fails with
`feature 'edition2024' is required` — fix it with `rustup default stable`. There is
deliberately no `rust-toolchain.toml`; CI installs `dtolnay/rust-toolchain@stable`.

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
- **Credentials.** LAN-enabled devices can be discovered and controlled without Govee
  credentials when LAN Control is enabled. Configure `GOVEE_EMAIL` / `GOVEE_PASSWORD`
  and/or `GOVEE_API_KEY` for cloud-backed functionality and richer metadata — see
  [`docs/CONFIG.md`](docs/CONFIG.md). None of them are required to build, test, or
  smoke-test.

## CI

PRs must pass `cargo build`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo fmt --check` (see `.github/workflows/pr.yml`).

The fork also runs Claude Code CI (`.github/workflows/claude.yml`).

### Add-on image builds

`.github/workflows/build.yml` builds the Home Assistant add-on with HA's composable
actions (`home-assistant/builder/actions/build-image@2026.06.0`), one job per arch on a
native runner (`ubuntu-24.04` / `ubuntu-24.04-arm`). The deprecated monolithic
`home-assistant/builder` action is not usable: no builder image was published for
`2026.06.0`, and every older one bundles a cosign too old to read the signatures HA
applied to its base images on 2026-06-16.

Two things the composable actions do **not** do, so the workflow does them itself:

- **They never read `addon/build.yaml`.** `BUILD_FROM`, the OCI labels, and the
  base-image cosign identity are passed as workflow inputs. `addon/build.yaml` still
  matters for Supervisor's local builds, so the two must be kept in sync by hand.
- **They gate base-image cosign verification on `push == true`.** The PR job
  (`test-addon`) therefore runs an explicit `cosign-verify` step; without it the check
  would silently skip on exactly the runs that are meant to catch it.

**The published add-on image tag is the `version:` in `addon/config.yaml`, not the git
tag name.** `scripts/apply-tag.sh` derives it from the HEAD commit and the release job
reads it back out of the file. Supervisor pulls
`ghcr.io/florianhorner/govee2mqtt-{arch}:<that version>`, so publishing anything else
404s every install and update.

**The add-on copies its binary out of the standalone `govee2mqtt` image, and the release
job pins which one.** `addon/Dockerfile` takes `ARG GOVEE_IMAGE`, defaulting to
`:latest`; the `addon` job overrides it with `ghcr.io/florianhorner/govee2mqtt:${{ github.ref_name }}`.
That pin matters because the `merge` job only refreshes `latest` on `main`, so a tag build
reading `:latest` would package whatever `main` last published rather than the tagged
commit. The job passes and the image is signed either way, so nothing flags the mismatch.

The two image families take their tags from different places: the standalone
`govee2mqtt` image is tagged by `merge` with the **git tag name**, while the add-on
`govee2mqtt-{arch}` images are tagged with `addon/config.yaml`'s `version:`. They have
diverged before (git tag `2026.03.22` published `govee2mqtt-amd64:2026.03.22-ba238f5e`), so
`GOVEE_IMAGE` must use `github.ref_name` and the add-on tag must not.

`test-addon` deliberately does not pass `GOVEE_IMAGE`, so CI still exercises the Dockerfile
default, which is the path a Supervisor local build takes. A green `test-addon` does not
cover the publish path, which runs only on a tag.

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
