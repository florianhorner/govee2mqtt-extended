# Roadmap

What's planned for this fork. Items move up as they're worked on.
PRs and ideas welcome — [open an issue](https://github.com/florianhorner/govee2mqtt-extended/issues).

---

## Now (in progress)

- **Documentation overhaul** — fix typos, consistent naming (`Govee2MQTT` everywhere), friendlier addon config descriptions, clearer README feature table
- **Upstream sync** — rebase on latest upstream to pick up dependency updates and new device support

## Next (committed, scoped)

- **Fix hardcoded `"uuid"` request IDs** — Platform API methods send a literal `"uuid"` string instead of a generated UUID. Could cause server-side dedup issues. One-line fix x4 (`src/platform_api.rs`)
- **Error message consistency** — standardize Rust log messages to `"{action} failed for {device}: {reason}"` pattern. Replace cryptic messages like `"no lan client"` and `"cannot find device {id}!?"`
- **Upstream PR: panic hardening + exit code fix** — [#617](https://github.com/wez/govee2mqtt/issues/617), [#618](https://github.com/wez/govee2mqtt/issues/618) are filed, need clean PRs

## Later (ideas, not committed)

- **Web UI improvements** — connection status indicators, room grouping, help links for "Missing" devices
- **Config description defaults** — add a "Default" column to config tables so users know what happens when fields are blank
- **Docker `.env` grouping** — organize the env skeleton with section headers (`## Govee credentials`, `## MQTT`, `## Display`)
- **Additional device support** — community-requested SKUs as they come in

## Upstream tracker

Fixes and features submitted back to [wez/govee2mqtt](https://github.com/wez/govee2mqtt):

| What | Upstream status | Fork status |
|------|----------------|-------------|
| UTF-8 crash fix | [Merged via #606](https://github.com/wez/govee2mqtt/pull/606) | Included since 2026.03.16 |
| H60B0 device support | [PR #629](https://github.com/wez/govee2mqtt/pull/629) pending | Included since 2026.03.22 |
| Panic hardening | [#617](https://github.com/wez/govee2mqtt/issues/617) filed | Included since 2026.03.22 |
| Exit code fix | [#618](https://github.com/wez/govee2mqtt/issues/618) filed | Included since 2026.03.22 |
| Scene quick-cycle | Fork-only (not submitted) | Included since 2026.03.26 |

## Contributing

If you use Govee devices with Home Assistant and hit a bug or want a feature, this fork is a good place to land it — especially if upstream review timelines are long. PRs welcome. The bar is: `cargo build`, `cargo clippy -- -D warnings`, `cargo test`, `cargo fmt --check` all pass.
