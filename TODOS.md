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
