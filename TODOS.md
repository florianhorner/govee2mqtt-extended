# TODOs

## Singleflight per-device scene-catalog refresh
**What:** When `should_refresh_scene_catalog()` returns true, concurrent callers can all pass the
check and hit the Govee platform/undoc APIs before the first refresh stores its result, racing to
write the same cache.
**Why:** A capability-signature change plus a burst of LAN/IoT state notifications for one device
produces a short API storm (a few redundant calls). `cache.rs` is a read-through TTL cache with no
in-flight dedup, so it does not collapse the concurrent calls.
**Pros:** Removes redundant Govee API calls on the rare capability-change path.
**Cons:** Requires a new per-device refresh lock with a double-checked cache re-read. It sits next
to `notify_of_state_change` (which carries a deadlock warning), so it MUST be added carefully with
dedicated concurrency tests — the lock must never be held across the `devices_by_id` lock. The
storm is small (1-3 calls) and rare, so the cure is riskier than the disease unless done with care.
**Context:** `device_list_scenes_categorized` / `fetch_scene_catalog` in `state.rs`. Bounded today
by the signature settling after the first refresh + the 300s `cache.rs` soft-TTL.
**Effort:** Small-medium. Do as its own focused change, not a ride-along.

## Track real liveness for web UI status badges
**What:** `list_devices` (`http.rs`) reports `mqtt_connected` (a HassClient handle exists),
`api_available` (metadata loaded at some point), and `lan_active` (an IP was ever discovered). The
web UI renders these as live status badges.
**Why:** They overstate liveness — they don't reflect a dropped broker, stale LAN device, or a
device that's currently unreachable, which misleads production debugging.
**Options:** rename to discovered/loaded semantics (cheap, honest), or track real connection/last-seen
freshness (more work).
**Files:** `src/service/http.rs`, `assets/components/devices.js`. Ships with PR2 (cosmetic bundle).
**Effort:** Small (rename) to medium (real freshness).

## Nothing validates `addon/config.yaml` after the builder migration
**What:** The legacy `home-assistant/builder` parsed `addon/config.yaml` (it needed `version` and
`image`), so a malformed file failed CI. `build-image` never reads it. The release job now only
asserts that `version:` is non-empty.
**Why:** A typo in `image:`, `arch:`, or the options schema now reaches Supervisor instead of CI.
**Pros:** Restores a check that silently disappeared.
**Cons:** Either a hand-rolled assert (narrow) or a third-party HA add-on linter action (broader,
one more pinned dependency to keep current).
**Context:** Found by the Codex outside voice during `/plan-eng-review` (2026-08-20). Cheapest form:
extend the existing `Apply tag to version` step in the `addon` job to also assert `image:` matches
the matrix image names.
**Effort:** Small.

## `BUILD_FROM` and cosign identity live in two places
**What:** `addon/build.yaml` pins `build_from:` per arch for Supervisor's local builds; the workflow
now hardcodes the same strings as `BUILD_FROM` build-args. Editing one does not update the other.
**Why:** CI and a local Supervisor build can silently produce different artifacts.
**Pros:** Reading `build_from.<arch>` out of `addon/build.yaml` with `yq` makes drift impossible
rather than merely commented.
**Cons:** Adds a parse step and a `yq` dependency to the job.
**Context:** Found by the Codex outside voice during `/plan-eng-review` (2026-08-20). Deferred from
that PR to keep the diff to the migration itself. Related: `addon/build.yaml`'s
`cosign.identity: https://github.com/florianhorner/govee2mqtt/.*` cannot match this repo
(`govee2mqtt-extended` — the regexp needs a literal `/` after `govee2mqtt`), so cache verification
has been failing silently. `build-image`'s default identity fixes it by accident; the stale value
should still be corrected and the file commented to say CI no longer reads it.
**Effort:** Small.

## Remaining checkouts still persist git credentials
**What:** `persist-credentials: false` was added to the `addon` and `test-addon` checkouts
on PR #49, but the `build` job in `.github/workflows/build.yml` and the job in
`.github/workflows/pr.yml` still use the `actions/checkout` default (`true`).
**Why:** zizmor flags it as `artipacked` — the token is written to a file under
`$RUNNER_TEMP` and stays available to every later step in the job. Those jobs run
repository-controlled code (`scripts/build-cross.sh`, `cargo build`) and need no
authenticated git after checkout.
**Pros:** Consistent posture across the repo; stops CodeRabbit/zizmor re-raising it on
every PR that touches these files.
**Cons:** The `build` job is load-bearing (cross-compiles and publishes the standalone
image). Low risk, but it is a working job being changed for a lint finding, so it wants its
own PR and its own green run rather than a ride-along.
**Context:** Raised by CodeRabbit on PR #49 (2026-08-21). Fixed there only for the two jobs
that PR rewrote; deliberately not extended to jobs the PR did not touch.
**Effort:** Small.

## No workflow linting in CI or pre-commit
**What:** `.pre-commit-config.yaml` runs `cargo fmt`, `cargo clippy`, and a docs-only check. Nothing
lints `.github/workflows/*.yml`.
**Why:** A YAML or expression slip in a workflow is only discovered by pushing and burning a full CI
cycle — and the release job cannot be exercised at all except by cutting a real tag.
**Pros:** `actionlint` catches syntax, bad `runs-on` labels, and shellcheck issues in `run:` blocks
before the push.
**Cons:** Adds a hook and a binary that every contributor needs. Note the repo already has three
pre-existing shellcheck findings in `build.yml` (in `build` and `merge`), so adding the hook means
either fixing or baselining those first.
**Context:** Raised during `/plan-eng-review` (2026-08-20). `actionlint` was run manually against the
migrated `build.yml` and added no new findings.
**Effort:** Small, plus the pre-existing findings.

## Pin the add-on source image by digest, not by tag
**What:** The `addon` job passes `GOVEE_IMAGE=ghcr.io/florianhorner/govee2mqtt:${{ github.ref_name }}`.
A tag is a mutable pointer, so the pin narrows the window rather than closing it.
**Why:** If the workflow is re-run, or the tag is force-moved, `merge` can republish
`govee2mqtt:<tag>` between the two architecture legs of the `addon` matrix. The amd64 and
aarch64 add-on images could then package different commits, both signed. A digest cannot
move.
**Pros:** Closes the remaining window completely and makes the release reproducible from
the tag. Also lets the add-on verify the source image signature by digest.
**Cons:** The `merge` job exposes no digest output today, so this means adding a job output
to the publish job that ships the standalone image. That job is load-bearing and was kept
out of scope for the pin itself, so it wants its own PR and its own green run.
**Context:** Raised by the Codex adversarial pass during `/ship` of the `GOVEE_IMAGE` pin
(2026-08-22), ranked P1 there. Downgraded to P2 here because the tag pin is already a strict
improvement on the `:latest` it replaced, and the residual needs a re-run or a force-moved
tag to bite. Two neighbouring findings from the same pass were checked and do not hold:
branch pushes cannot reach `merge` (`on.push.branches` is `[main]` only), and a tag
containing `/` cannot match the `20*` trigger glob.
**Related:** CI never exercises the override path at all, since the `addon` job is tag-only.
A second `test-addon` build passing a known-existing tag would cover argument forwarding and
the source-image lookup without waiting for a release.
**Effort:** Small-medium.

## Add-on packaging still copies from a separate mutable image
**What:** The add-on image is assembled by copying `/app/govee` out of a second image
(`ghcr.io/florianhorner/govee2mqtt`) rather than building from the tagged source.
**Why:** The `ARG GOVEE_IMAGE` pin stops a tag build from packaging the wrong commit, but the
add-on still depends on an image produced by a separate job, so a release cannot be rebuilt from
the tag alone.
**Pros:** Reproducible releases, one build path instead of two, no cross-image coupling.
**Cons:** Real redesign. The Rust binary is cross-compiled by `scripts/build-cross.sh` in the `build`
job, so the add-on Dockerfile would need the artifact plumbed in — which the existing comment in
`build.yml` explicitly calls out as unsolved ("if you know how to get the bits above funnelled into
the hass build below, and it isn't eye-bleedingly-gnarly, please submit a PR").
**Context:** Raised as the strategic finding by the Codex outside voice during `/plan-eng-review`
(2026-08-20).
**Effort:** Large.

## Offer the builder migration upstream to `wez/govee2mqtt`
**What:** Issue #35 notes upstream has the identical add-on build config and is presumably hitting
the same cosign break. Once the migration is proven green here, the workflow change transfers
directly.
**Why:** The fix is upstream toolchain churn, not fork-specific. Upstreaming it reduces the fork's
delta.
**Pros:** Shrinks the diff this fork carries; helps upstream.
**Cons:** Upstream may have already fixed it differently — check before spending time.
**Context:** Raised during `/plan-eng-review` (2026-08-20). **Draft only.** Per fork-safety policy no
agent opens a PR against a repo outside `florianhorner/*`; the patch and PR text get drafted to
`.context/` and Florian sends it.
**Depends on / blocked by:** The migration must be green on a real release here first.
**Effort:** Small (the diff already exists), plus review latency upstream.

## Live-2FA probe can't distinguish "email request failed" from "email sent"
**What:** `classify_2fa_login_error` (`src/undoc_api.rs`) extracts only `(status, code_was_set)`
from a 2FA login error, never the message. So when the login path builds
`build_2fa_request_failed_error()` (the code-request transport genuinely failed, no email sent)
versus the normal 454-with-email-sent case, the `undoc live2fa-login` probe emits the identical
JSON either way: `{"schema":1,"outcome":"two_factor","status":454,"code_configured":false}`.
`scripts/live_2fa.py` still fails correctly either way — `wait_for_total`/`assert_quiet` verify
against the real mailbox over IMAP independently of the probe's claim — but a real transport
failure surfaces as the generic `email_wait_timed_out` instead of a reason that tells a human the
failure was on our send call, not on Govee's delivery.
**Why:** Diagnostic quality only, not a correctness bug — flagged by the adversarial pass during
`/ship` on PR #47 (confidence 7, classified FIXABLE, no false PASS produced).
**Pros:** A future live-debugging session gets a precise reason instead of inferring it from a
timeout, which is exactly the class of ambiguity this harness exists to eliminate.
**Cons:** A real wire-contract change — a new probe outcome value, `validate_probe_payload`'s
schema, and `docs/LIVE_2FA_TEST.md`'s documented contract ("returns only the outcome, status
454/455, and whether a code was configured") all move together. `classify_2fa_login_error`'s own
docstring makes its narrow (status, code_was_set)-only contract deliberate, so this is a scope
decision, not just an oversight.
**Context:** `src/undoc_api.rs` (`classify_2fa_login_error`, `build_2fa_request_failed_error`,
`handle_2fa_status`) is also being substantially reworked by #52 (`fix/login-response-redaction`,
open) in this exact region — do this after #52 lands to build on its final shape instead of
conflicting with it mid-flight.
**Effort:** Small-medium.

## `lan_carry_over_preserves_iot_mode_observation_time` is flaky
**What:** `service::device::test::lan_carry_over_preserves_iot_mode_observation_time` failed once
in roughly twenty `cargo test --all --all-features` runs and then passed 15/15 consecutive reruns
and in isolation. The assertion that fires was not captured, so the mechanism below is a
hypothesis, not a confirmed diagnosis.
**Why:** `Device::device_state()` (`src/service/device.rs`) collects the LAN, HTTP and IoT
projections, runs `candidates.sort_by_key(|a| a.updated)` and pops the last one. `sort_by_key` is
stable, so when two projections carry the *same* `updated` instant the original vector order
decides the winner, and IoT is pushed last. The test calls `set_iot_device_status` immediately
followed by `set_lan_device_status`; if both `Utc::now()` reads land on one instant, `device_state()`
returns the IoT projection and `assert_eq!(state.source, "LAN API")` fails.
**Pros:** If the hypothesis holds, this is a real product-level ambiguity, not just a test bug —
on a timestamp tie the bridge silently prefers a stale IoT projection over a fresh LAN poll.
Breaking the tie explicitly (prefer LAN > HTTP > IoT on equal `updated`) fixes both.
**Cons:** Confirming it needs a deterministic repro — inject the timestamps rather than reading the
wall clock, or assert the tie-break directly with hand-stamped `updated` values. Guessing at the
fix without that repro risks papering over a different race.
**Context:** Pre-existing on `origin/main`; neither the test nor `device_state()` is touched by
`feat/music-sensitivity-and-palette-wiring`. Found while running its gates (2026-08-29).
**Effort:** Small.

## Nothing asserts the MQTT router's route table
**What:** ~~`rebuild_router` registrations had no table-level check.~~ Closed: an
`mqtt_routes!` X-macro owns all 18 bindings. `bind_mqtt_command_routes` is the only
registration site; tests record that path without a broker.
`mqtt_command_routes_register_even_when_music_palette_is_off` fails if a route is
wrapped in a runtime `if` (the `GOVEE_MUSIC_PALETTE` opt-in stays in the handler).
`mqtt_route_handlers_match_their_patterns` catches same-arity handler swaps.
**Why:** Verified by mutation during pre-merge review: removing the
`gv2mqtt/:id/set-music-sensitivity` registration entirely left 238/238 tests passing. The test that
looks like it covers this, `command_and_state_topics_match_the_registered_mqtt_route`, never reads
`rebuild_router`; it round-trips `MusicSensitivityNumber::new`'s own `replacen` against the same
constant, so it cannot fail independently.
**Pros:** Closes pairing and registration-site gating for all 18 routes. Deleting a route still
requires also deleting its expected-list entry to stay silent — same dual-update limit as any
expected list.
**Cons:** Not reachable from a plain unit test against a live `MqttRouter` (subscribe needs a
broker). The recorder bind path is the substitute.
**Context:** Pre-existing — all 16 routes on `origin/main` have the same exposure; the music
sensitivity branch adds the 17th and 18th (`set-music-sensitivity` and
`clear-music-sensitivity`). Closed by `feat/conditional-mqtt-route-guard`.
**Effort:** Medium.

## `GOVEE_DISABLE_EFFECTS` leaves a live but useless sensitivity slider
**What:** `src/hass_mqtt/light.rs` empties the effect list when `GOVEE_DISABLE_EFFECTS=true` (and
filters it via `GOVEE_ALLOWED_EFFECTS`), but the Music Sensitivity guard in
`src/hass_mqtt/enumerator.rs` only checks `has_music_mode_options()` and `avoid_platform_api()`.
With effects disabled there is no `Music:` effect to select, so the slider is writable and stores a
value that can never be applied.
**Why:** Same failure the existing guard was written to prevent (a control that silently does
nothing), reached through a different door. Found during pre-merge review.
**Pros:** A one-condition fix makes the entity's presence honest in every configuration.
**Cons:** Not actually one condition. `GOVEE_ALLOWED_EFFECTS` can filter out every `Music:` entry
while leaving others, so a correct guard has to inspect the resolved effect list rather than the
env var. It also has to account for the scene next/prev buttons, which bypass the filter and can
still reach a Music effect — so with those in play the slider is not strictly dead.
**Context:** `light.rs` effect-list construction vs the `MusicSetting` arm in `enumerator.rs`.
Deliberately not fixed alongside the feature: it is a new conditional that needs its own tests and
interacts with scene cycling.
**Effort:** Small-medium.
