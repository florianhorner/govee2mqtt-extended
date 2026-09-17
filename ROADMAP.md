# Roadmap

What is planned for this fork and what is known to be broken. Items move up as they
are worked on. PRs and ideas are welcome: [open an issue](https://github.com/florianhorner/govee2mqtt-extended/issues).

---

## Known issues

User-visible problems with a workaround where one exists.

- On air purifiers such as the H7124, the gear-mode number entity offers 0 to 255 while
  only 1 to 3 (Low, Medium, High) are valid. Govee rejects the rest with a 400 error and
  nothing changes on the device. Use the fan card's speed slider or presets instead, and
  send only valid values from automations. The fix is to derive the bounds from the
  mode's own values and refuse out-of-range commands (see
  [wez/govee2mqtt#297](https://github.com/wez/govee2mqtt/issues/297)).
- On appliances with a nightlight, such as the H7124 and H7143, the Night Light entity can
  show off whenever an AWS IoT update is the newest state, because those updates carry no
  nightlight status. It recovers at the next Platform API poll, which can take 15 minutes
  or longer while IoT updates keep arriving. Where the device has a nightlight toggle
  switch, that switch stays correct; otherwise add a delay to automations that trigger on
  the light turning off. The fix is to carry the last known nightlight state over from the
  Platform API and to schedule polls by when Platform data was last fetched.
- Without an IoT connection, a fan or air purifier entity runs in optimistic mode, so Home
  Assistant shows the new speed even when the Platform API command failed or no API key is
  configured. The humidifier entity has the same pattern. The fix is to publish state only
  after the command is confirmed.
- Fan discovery (speed steps and preset modes) is republished only at startup, after a
  Home Assistant restart, or on `gv2mqtt/purge-caches`. If a later Platform API poll
  changes a device's work-mode metadata, Home Assistant keeps the old speed range while
  the bridge uses the new one, which can set a wrong speed or log invalid-preset warnings.
  Other entity types have the same gap, so the fix is a general republish-on-change path.
- With `disable_effects` set, or an `allowed_effects` list that leaves out every Music
  effect, the Music Sensitivity slider still appears although no music effect can be
  picked from the effect list. The entity should follow the resolved effect list, keeping
  in mind that the scene next and previous buttons bypass that filter.
- Dehumidifiers with a nightlight get a light entity carrying the dehumidifier's own name.
  On humidifiers, fans and purifiers that light is named Night Light. Adding the
  dehumidifier type fixes it; entity ids stay the same, but the display name changes for
  existing users.
- The MQTT, API and LAN badges in the web UI show whether a connection or data source was
  set up, not whether it is live right now. A dropped broker or an unreachable LAN device
  still shows as connected. Either rename the badges to what they measure or track
  last-seen freshness.

## Next

Small, scoped changes that would be accepted as PRs.

- Command topics such as the fan's `set-percentage` accept any device id, not only
  devices that expose the matching entity, so a mistyped automation can change the work
  mode of a different appliance. Command routes should reject devices that do not
  advertise the entity.
- When LAN and IoT state carry the same timestamp, the bridge picks the IoT state only
  because it was added last, which is why `lan_carry_over_preserves_iot_mode_observation_time`
  fails occasionally. A deterministic tie-break (LAN, then HTTP, then IoT) with injected
  timestamps fixes both the test and the behaviour.
- CI no longer parses `addon/config.yaml` beyond the version format since the move to
  Home Assistant's composable build actions. A typo in `image`, `arch` or the options
  schema only shows up in Supervisor. A small CI check or an add-on linter would catch it
  before a release.
- Workflow files under `.github/workflows` are not linted, so a syntax or expression
  mistake only shows up after a push, and the release path only runs on a real tag.
  Adding actionlint to pre-commit or CI catches these earlier; the existing shellcheck
  findings in `build.yml` need fixing or baselining first.
- The live two-factor test probe reports the same outcome when the request for a
  verification e-mail fails and when the e-mail was sent, so a failed send only surfaces
  later as a mailbox timeout. A separate outcome would make failures easier to diagnose.
  It changes the probe's documented output, so the validator and `docs/LIVE_2FA_TEST.md`
  change with it.

## Later

Larger changes, not committed to.

- When a device's capabilities change, several concurrent callers can each decide the
  scene catalog needs a refresh and send duplicate requests to Govee. A per-device refresh
  guard with a double-checked cache read would collapse them into one call. It sits next
  to the device state notification path, so it needs careful lock ordering and
  concurrency tests.
- The add-on image copies the `govee` binary out of the separately published
  `govee2mqtt` image instead of building from the tagged source. Building both from one
  path would make a release reproducible from its tag alone. The hard part is getting the
  cross-compiled binaries from the build job into the Home Assistant image build.
- This fork moved its add-on build to Home Assistant's composable build actions after the
  old builder stopped verifying base images. Upstream still uses the old builder, so
  offering the same change there would help upstream and shrink this fork's diff.
- Rebase on upstream from time to time to pick up dependency updates and new device
  support.
- Additional device support as community requests come in.

## Upstream tracker

Fixes and features submitted back to [wez/govee2mqtt](https://github.com/wez/govee2mqtt):

| What | Upstream status | Fork status |
|------|----------------|-------------|
| UTF-8 crash fix | [Merged via #606](https://github.com/wez/govee2mqtt/pull/606) | Included since 2026.03.16 |
| H60B0 (Uplighter) LAN support | [PR #629](https://github.com/wez/govee2mqtt/pull/629) closed unmerged | Included since 2026.03.22 |
| Panic hardening | [#617](https://github.com/wez/govee2mqtt/issues/617) filed | Included since 2026.03.22 |
| Exit code fix | [#618](https://github.com/wez/govee2mqtt/issues/618) filed | Included since 2026.03.22 |
| Scene quick-cycle | Fork-only (not submitted) | Included since 2026.03.26 |
| Fan and air purifier entities | Fork-only (not submitted) | Included since 2026.09.16 |
| Composable add-on build actions | Not submitted, see Later | Included since 2026.09.16 |

## Contributing

If you use Govee devices with Home Assistant and hit a bug or want a feature, this fork is
a good place to land it, especially when upstream review timelines are long. PRs welcome.
The bar is: `cargo build`, `cargo clippy -- -D warnings`, `cargo test`, `cargo fmt --check`
all pass, and `scripts/check_public_content.py --all` finds nothing. CI also runs a secret
scan and an editorial lint of the pull request body. See the
[public content rules](CONTRIBUTING.md#public-content) before pasting logs or evidence.
