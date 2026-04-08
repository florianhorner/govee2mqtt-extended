> **✅ The UTF-8 crash fix has been [merged upstream](https://github.com/wez/govee2mqtt/pull/606) and released as `2026.03.25-ab9deb66`.**
> If you installed this fork as a workaround, you can now [switch back to upstream](#switch-back-to-upstream).
>
> This fork continues as a **maintained community fork** with additional fixes and device support not yet upstream.
> See [What this fork adds](#what-this-fork-adds) below.

# Govee2MQTT: Govee-to-Home-Assistant Bridge

This repo provides a `govee` executable whose primary purpose is to act
as a bridge between [Govee](https://govee.com) devices and Home Assistant,
via the [Home Assistant MQTT Integration](https://www.home-assistant.io/integrations/mqtt/).

## What this fork adds

| Commit | File | Change |
|--------|------|--------|
| `6f2f5cc` | `addon/config.yaml`, `.github/`, `README.md` | Brand fork with CI, addon images, custom addon images, and config |
| `da4aeb1` | `.github/workflows/`, tests | Add Claude Code CI, regression tests, and fork fixes |
| `f41ac85` | `src/service/quirks.rs` | Add Govee H60B0 (Neon Rope Light 2) as LAN-capable light |
| `261eb48` | `src/hass_mqtt/*.rs`, `src/service/hass.rs` | Replace `.expect()` panics with graceful handling; fix silent `exit(0)` → `exit(1)` so HA restarts the addon on failure |
| `0666c35` | `src/hass_mqtt/*.rs`, `src/service/*.rs` | Scene quick-cycle: Next/Previous buttons, scene info sensor, categorized catalog endpoint with caching |
| `2b87f42` | `.github/workflows/pr.yml`, `.pre-commit-config.yaml`, `src/**` | Add clippy CI gate (`-D warnings`), pre-commit hooks (fmt + clippy), fix all existing clippy warnings |

**Upstream status:**
- ✅ UTF-8 fix — [merged via #606](https://github.com/wez/govee2mqtt/pull/606) on 2026-03-25
- ⏳ H60B0 device support — [PR #629](https://github.com/wez/govee2mqtt/pull/629) pending
- ⏳ Panic hardening + exit code fix — [#617](https://github.com/wez/govee2mqtt/issues/617), [#618](https://github.com/wez/govee2mqtt/issues/618) filed, no PR yet
- 🆕 Scene quick-cycle buttons + catalog — fork-only feature, not submitted upstream

## Features

* Robust LAN-first design. Not all of Govee's devices support LAN control,
  but for those that do, you'll have the lowest latency and ability to
  control them even when your primary internet connection is offline.
* Support for per-device modes and scenes.
* Support for the undocumented AWS IoT interface to your devices, provides
  low latency status updates.
* Support for the new [Platform
  API](https://developer.govee.com/reference/get-you-devices) in case the AWS
  IoT or LAN control is unavailable.

**What the "Requires" column means:**

* **API Key** — You've [applied for a free Govee API key](https://developer.govee.com/reference/apply-you-govee-api-key) and entered it in the configuration.
* **Govee Account** — You've entered your Govee email and password. This connects to Govee's cloud IoT service for real-time updates and Tap-to-Run support.
* **LAN API** — You've [enabled the LAN API](https://app-h5.govee.com/user-manual/wlan-guide) on supported devices in the Govee Home app, and your network allows the required UDP traffic.

|Feature|Requires|Where to find it in Home Assistant|
|-------|--------|----------------------------------|
|DIY Scenes|API Key|Effects list on the light entity|
|Music Modes|API Key|Effects list on the light entity|
|Tap-to-Run / One Click|Govee Account|Scenes list, and under the "Govee to MQTT" device|
|Live Status Updates|LAN API and/or Govee Account|Automatic — devices update within seconds|
|Segment Color|API Key|`Segment 001`, `002`, etc. light entities under your device|

## Usage

* [Installing as a Home Assistant App](docs/ADDON.md) - for HAOS and Supervised installations
* [Running it in Docker](docs/DOCKER.md)
* [Configuration](docs/CONFIG.md)

## Have a question?

* [Is my device supported?](docs/SKUS.md)
* [Check out the FAQ](docs/FAQ.md)

## Switch back to upstream

The UTF-8 crash fix is now upstream in release `2026.03.25-ab9deb66`. If you only installed this fork for that fix, you can switch back:

1. **In Home Assistant**, go to **Settings → Apps** (three-dot menu → Repositories).
2. **Remove** this fork's repo URL: `https://github.com/florianhorner/govee2mqtt-extended`
3. **Add** the upstream repo URL: `https://github.com/wez/govee2mqtt`
4. **Refresh** and update/reinstall the Govee2MQTT app.
5. **Restart** the app. Verify your Govee devices come back online.

**Note:** If you want the additional fixes in this fork (H60B0 support, panic hardening, exit code fix), stay on this fork until those are merged upstream.

## Want to show your support or gratitude?

It takes significant effort to build, maintain and support users of software
like this. If you can spare something to say thanks, it is appreciated!

* [Sponsor wez on Github](https://github.com/sponsors/wez)
* [Sponsor wez on Patreon](https://patreon.com/WezFurlong)
* [Sponsor wez on Ko-Fi](https://ko-fi.com/wezfurlong)
* [Sponsor wez via liberapay](https://liberapay.com/wez)

## Credits

This work is based on wez's earlier work with [Govee LAN
Control](https://github.com/wez/govee-lan-hass/).

The AWS IoT support was made possible by the work of @bwp91 in
[homebridge-govee](https://github.com/bwp91/homebridge-govee/).

The UTF-8 fix was originally authored by [theg1nger](https://github.com/wez/govee2mqtt/pull/606).
