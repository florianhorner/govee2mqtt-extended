> **Fork of [wez/govee2mqtt](https://github.com/wez/govee2mqtt)** with
> additional device support, crash fixes, and scene cycling.
> Maintained by [@florianhorner](https://github.com/florianhorner).
> If you installed this fork for the UTF-8 crash fix (now
> [merged upstream](https://github.com/wez/govee2mqtt/pull/606)),
> you can [switch back](#switch-back-to-upstream).
> See [What this fork adds](#what-this-fork-adds) for details.

# Govee2MQTT: Govee-to-Home-Assistant Bridge

Control your [Govee](https://govee.com) lights, humidifiers, and other smart devices from
[Home Assistant](https://www.home-assistant.io/) — including automations, dashboards, and voice assistants.

Govee2MQTT acts as a bridge: it talks to your Govee devices and makes them
available in Home Assistant through [MQTT](https://www.home-assistant.io/integrations/mqtt/)
(a standard messaging protocol that Home Assistant uses to communicate with devices).

## Getting started

Choose the installation method that matches your Home Assistant setup:

* **[Install as a Home Assistant App](docs/ADDON.md)** — recommended for HAOS and Supervised installations (most users)
* **[Run in Docker](docs/DOCKER.md)** — for Home Assistant Container or Core installations
* **[Configuration reference](docs/CONFIG.md)** — all available settings

## What you'll need

1. **Govee devices** with Wi-Fi (Bluetooth-only devices are not supported yet)
2. **An MQTT broker** — the [Mosquitto app](https://github.com/home-assistant/addons/blob/master/mosquitto/DOCS.md) is the easiest option
3. **Your Govee account credentials** (the email and password you use in the Govee Home app) — recommended for room names, scenes, and full device support
4. **A Govee API key** (optional but recommended) — [get one free here](https://developer.govee.com/reference/apply-you-govee-api-key). Enables scene control, segment colors, and music modes.

## Features

* **Local-first control** — Devices with [LAN API support](https://app-h5.govee.com/user-manual/wlan-guide) respond faster and work even when your internet is down.
* **Scenes and modes** — DIY scenes, music modes, and Tap-to-Run shortcuts from the Govee app all work in Home Assistant.
* **Real-time status** — Devices report state changes (on/off, color, brightness) within seconds via LAN or Govee's cloud.
* **Broad device support** — Lights, LED strips, humidifiers, heaters, fans, purifiers, and kettles.

**What the "Requires" column means:**

* **API Key** — You've [applied for a free Govee API key](https://developer.govee.com/reference/apply-you-govee-api-key) and entered it in the configuration.
* **Govee Account** — You've entered your Govee email and password. This connects to Govee's cloud for real-time updates and Tap-to-Run support.
* **LAN API** — You've [enabled the LAN API](https://app-h5.govee.com/user-manual/wlan-guide) on supported devices in the Govee Home app.

|Feature|Requires|Where to find it in Home Assistant|
|-------|--------|----------------------------------|
|DIY Scenes|API Key|Effects list on the light entity|
|Music Modes|API Key|Effects list on the light entity|
|Tap-to-Run / One Click|Govee Account|Scenes list, and under the "Govee to MQTT" device|
|Live Status Updates|LAN API and/or Govee Account|Automatic — devices update within seconds|
|Segment Color|API Key|`Segment 001`, `002`, etc. light entities under your device|

## Have a question?

* [Is my device supported?](docs/SKUS.md)
* [Frequently Asked Questions](docs/FAQ.md)
* [LAN API troubleshooting](docs/LAN.md)
* [Privacy policy](docs/PRIVACY.md)

---

## What this fork adds

This fork of [wez/govee2mqtt](https://github.com/wez/govee2mqtt)
adds device support, stability fixes, and features I needed for my setup:

* **H60B0 (Neon Rope Light 2)** — added as LAN-capable device
* **Panic hardening (in progress)** — critical `.expect()` panics replaced with graceful error handling
* **Exit code fix** — silent `exit(0)` changed to `exit(1)` so Home Assistant properly restarts the app on failure
* **Scene quick-cycle** — Next/Previous buttons and scene info sensor
* **CI improvements** — clippy gate, pre-commit hooks, automated testing

**Upstream status:**
- ✅ UTF-8 fix — [merged via #606](https://github.com/wez/govee2mqtt/pull/606) on 2026-03-25
- ⏳ H60B0 device support — [PR #629](https://github.com/wez/govee2mqtt/pull/629) pending
- ⏳ Panic hardening + exit code fix — [#617](https://github.com/wez/govee2mqtt/issues/617), [#618](https://github.com/wez/govee2mqtt/issues/618) filed, PRs planned
- 🆕 Scene quick-cycle buttons + catalog — fork-only feature, not submitted upstream

## Switch back to upstream

The UTF-8 crash fix is now upstream in release `2026.03.25-ab9deb66`. If you only installed this fork for that fix, you can switch back:

1. **In Home Assistant**, go to **Settings → Apps → App store** (three-dot menu → Repositories).
2. **Remove** this fork's repo URL: `https://github.com/florianhorner/govee2mqtt-extended`
3. **Add** the upstream repo URL: `https://github.com/wez/govee2mqtt`
4. **Refresh** and update/reinstall the Govee2MQTT app.
5. **Restart** the app. Verify your Govee devices come back online.

**Note:** If you want the additional fixes in this fork (H60B0 support, panic hardening, exit code fix), stay on this fork until those are merged upstream.

## Credits

This fork is maintained by [@florianhorner](https://github.com/florianhorner).

This work is based on wez's earlier work with [Govee LAN
Control](https://github.com/wez/govee-lan-hass/).

The AWS IoT support was made possible by the work of @bwp91 in
[homebridge-govee](https://github.com/bwp91/homebridge-govee/).

The UTF-8 fix was originally authored by [theg1nger](https://github.com/wez/govee2mqtt/pull/606).

## Want to show your support?

* [Sponsor wez on Github](https://github.com/sponsors/wez)
* [Sponsor wez on Patreon](https://patreon.com/WezFurlong)
* [Sponsor wez on Ko-Fi](https://ko-fi.com/wezfurlong)
* [Sponsor wez via liberapay](https://liberapay.com/wez)
