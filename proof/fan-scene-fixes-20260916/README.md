# Fan power and executable-scene fixes

**Status: local fixes validated; hardware retest pending.** Both defects identified by the earlier hardware run are addressed in source. That run's evidence is kept outside this repository and binds `2dad7cf2`, not this patch.

## Changes

| Defect | Root cause and fix | Preserved behavior |
|---|---|---|
| H7124 fan power changes the nightlight | Fan power used the whole-device switch. A dedicated registered fan command route and shared ON/OFF/percentage-zero helper read the current nightlight from the Platform API, change power, then restore the observed light value. Unknown readback fails before actuator writes; restoration is attempted even after a failed power response. | Generic master-power switch and other models keep their existing behavior. Fan unique IDs, speeds, presets and oscillation are unchanged. |
| Scene cycling selects an empty entry | The Platform API name list injected an empty sentinel that activation rejects. It now excludes empty names; the shared categorized catalog also filters empty entries/categories on cached, fresh and error-fallback paths before evaluating empty-result recovery. | Exact non-empty names, a genuine scene named `None`, ordering, media, groups and existing fallback/cache rules survive. The API still rejects empty scene activation before a control request. |

Source: `src/hass_mqtt/fan.rs`, `src/hass_mqtt/command_routes.rs`, `src/service/hass.rs`, `src/platform_api.rs`, `src/service/state.rs`. No CLI flags, environment variables or add-on configuration changed.

A sixth file, `src/cache.rs`, changes **test builds only**: global cache initialization/purge uses in-memory SQLite, so production-path regression fixtures cannot open or remove a developer's persistent cache. Release-build initialization and purge remain unchanged.

## Verification

| Check | Result | Artifact |
|---|---|---|
| Fan defect reproduction | 5 expected failures; 3 preserved-behavior passes | `fan-regression-red.log` |
| Scene defect reproduction | 13 expected failures; real-None wire test passes | `scene-regression-red-complete.log` |
| Final fan regression suite | **10 passed** | `fan-regression-green-final.log` |
| Final scene regression suite | **14 passed** | `scene-regression-green.log` |
| `cargo test --all -- --show-output` | **362 passed** | `fix-tests-default.log` |
| `cargo test --all --all-features -- --show-output` | **363 passed** | `fix-tests-all-features.log` |
| `cargo build --all` | PASS | `fix-build.log` |
| `cargo clippy --all -- -D warnings` | PASS | `fix-clippy-default.log` |
| `cargo clippy --all --all-features -- -D warnings` | PASS | `fix-clippy-all-features.log` |
| `cargo fmt --all -- --check` | PASS | `fix-format-check.log` |
| `python3 -m unittest scripts/test_live_2fa.py` | **22 passed** | `fix-python-tests.log` |
| Focused read-only fan review | No concrete additional finding | `review.md` |

The **24 new regression tests** exercise actual handlers, HTTP requests and catalog/cache paths. Fan cases include ON, explicit OFF and zero-speed, both prior nightlight values, Low/Turbo context, stale cached state, missing metadata/client/readback, and read/control/restoration failures. Scene cases cover platform and undoc sources, cached/error/empty refreshes, metadata preservation, real `None`, inactive/unknown state, both cycling directions and wraparound, single-scene lists and empty-only selector suppression.

The fan red run used the new route/test scaffolding with `fan_power` still delegating to the old whole-device power operation. It demonstrates the original control defect, not a pristine-HEAD full-suite result. The scene red run preceded the production catalog changes. Original failure output remains retained; two additional fan precondition guards were added before final validation.

Rust suites ran sequentially because existing LAN tests share a fixed UDP port. The final scene and full Rust suites used `GOVEE_CACHE_DIR=/dev/null/govee-regression-cache`, an unusable filesystem location, to establish that test cache access stays in memory. New HTTP fixtures use loopback servers; scene metadata is seeded under unique synthetic keys. Python tests are unit tests, not a live 2FA login.

Stable rustfmt reports the repository's existing nightly-only `imports_granularity` option as unsupported; formatting checks still exit successfully. Build/toolchain details are in `toolchain.log`.

## Proof identity

- `changes.patch` records the six-file source patch against `2dad7cf29565877852a4865172a4f1738e58b942`.
- `SOURCE_SHA256SUMS` binds the exact source files tested; `manifest.json` records base/patch identity and scope.
- Refreshed 2026-09-17: the bundle was written at `97ae81a`, one commit before the follow-up `5aeedbb` changed `src/cache.rs`, `src/hass_mqtt/fan.rs` and `src/service/state.rs`. `SOURCE_SHA256SUMS` and `changes.patch` were regenerated against the merged commit `ca4724505e1bf2ef2ef18a031babbfec260bc070` so the verify command below holds for the shipped tree; the logs and test counts are unchanged and still reproduce there.
- `SHA256SUMS` covers every deliverable in this directory. Logs redact the private workspace path; all device identifiers in new test source are synthetic fixture data.
- The earlier hardware evidence is kept outside this repository. It proves the unpatched runtime, **not this patch**.

From the repository root:

```bash
shasum -a 256 -c proof/fan-scene-fixes-20260916/SOURCE_SHA256SUMS
cd proof/fan-scene-fixes-20260916
shasum -a 256 -c SHA256SUMS
```

## Limits

**No real hardware was tested with the patched binary.** Earlier H7124/H60B0 hardware evidence supplied the reproduction; local regressions model that observed behavior. Physical preservation, transient behavior and vendor delivery ordering remain unverified for this patch.

H7124 preservation uses separate API requests, not an atomic fan-only hardware command. A transient nightlight flash is possible; if restoration fails, the command reports an error and the light may remain changed. Extra read/restore requests are limited to the observed H7124 model.

This does not fix unrelated nested vendor 400 responses or a card sending a nonexistent `None`/named effect. Treating `None` as an unconditional clear command would break real scenes with that name. No scene-card UI or household automation was changed.

Shared-path coverage includes percentage-zero and cached/failed-refresh catalogs, not only the first failing controls. A Coordinator device snapshot can be older than queued commands, so the fan helper obtains a new vendor observation under the control permit. Executable catalogs are normalized before the empty-result fallback decision; otherwise an empty sentinel can suppress recovery and inflate counts.
