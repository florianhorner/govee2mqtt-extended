# TODOs

## Cache device_list_scenes() results
**What:** Make the existing flat `device_list_scenes()` read from the same catalog cache as `device_list_scenes_categorized()`.
**Why:** The existing function hits the Govee API on every call with no caching, same as the new categorized version. Once the catalog cache exists (Fix #1 in the scene-cycle PR), the flat function should read from it too.
**Pros:** Fewer API calls globally, consistent behavior, reduces rate-limit risk for all scene interactions.
**Cons:** Minor additional scope, needs cache invalidation on device re-enumeration.
**Context:** `state.rs:600` already has a TODO comment: "some plumbing to maintain offline scene controls for preferred-LAN control". This aligns with that goal.
**Depends on:** Scene Quick-Cycle PR's catalog cache implementation (Fix #1).

## Enrich Platform API scenes with undoc API icons/hints
**What:** For devices that go through Platform API (which only returns scene names), try the undocumented API as a secondary source to add icon URLs and hint text.
**Why:** Platform API `EnumOption` (platform_api.rs:960) has no icon or hint fields. Undoc API has both. Enriching would give more devices the full v2 Scene Deck card experience with thumbnails and descriptions.
**Pros:** More devices get visual scene previews instead of text-only fallback.
**Cons:** ~20 lines of code, risk of name mismatches between the two APIs (Platform vs undoc may use different scene name strings for the same scene). Needs careful matching logic.
**Context:** `fetch_scene_catalog` in state.rs:670-720 tries Platform API first; if it succeeds it returns immediately without trying undoc API. A hybrid approach would try both and merge by scene name.
**Depends on:** Scene Deck v2 (hint field in SceneCatalogEntry).

## P2: Web UI improvements
**What:** The web UI (`assets/`) is a bare device table with no status info, help links, or grouping.
**Improvements:**
- Add a subtitle: "Devices discovered by Govee2MQTT"
- Show connection status indicators (MQTT connected, API available, LAN active)
- Group devices by room when room data is available
- Link "Missing" status devices to LAN troubleshooting docs
**Effort:** Small-medium. Mostly JS/HTML changes in `assets/components/devices.js` and `assets/index.html`.

## P3: Standardize Rust error messages
**What:** Error messages use inconsistent patterns — some are clear (`"Unable to control X for {device}"`), others are cryptic (`"no lan client"`, `"cannot find device {id}!?"`).
**Improvements:**
- `"no lan client"` → `"LAN control unavailable: no LAN client connected"`
- `"cannot find device {id}!?"` → `"Device not found: {id}"`
- `"Don't know how to {command}"` → `"Unsupported command '{command}' for {id}"`
- `"Undoc API client is not available"` → `"Govee cloud API unavailable — is your email/password configured?"`
**Files:** `src/service/state.rs`, `src/service/hass.rs`, `src/service/http.rs`
**Effort:** Small. ~15 string replacements across 3 files.

## P3: CONFIG.md table improvements
**What:** Config reference tables lack a "Default" column, making it unclear what happens when fields are left blank.
**Improvements:** Add a Default column to each table showing the default value or "required" / "auto-detected".
**File:** `docs/CONFIG.md`
**Effort:** Small.
**Status:** Partially done — MQTT table now has a Default column. Govee credentials and LAN tables still need it.

## P3: DOCKER.md section structure
**What:** The `.env` skeleton has inline comments but no section grouping, and doesn't explain *why* credentials are recommended.
**Improvements:**
- Add section headers (`## Govee Credentials`, `## MQTT`, `## Display`)
- Explain why email/password are "strongly recommended" (room names, cloud-only devices)
**File:** `docs/DOCKER.md`
**Effort:** Small.
