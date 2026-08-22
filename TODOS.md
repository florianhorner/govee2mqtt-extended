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
