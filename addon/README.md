# Govee2MQTT Bridge (Fork)

> Fork of [wez/govee2mqtt](https://github.com/wez/govee2mqtt) with device fixes, crash hardening, and scene cycling.
> Prefer upstream unless you need these specific changes.
>
> **Use this fork if** you need H60B0 support, 2FA login, scene quick-cycle, or can't wait for upstream fixes.
> When these changes land upstream, switch back — this fork is a staging ground, not a permanent alternative.

Control your Govee lights, LED strips, humidifiers, and other smart devices
directly from Home Assistant — including automations, dashboards, and
voice assistants.

## What this fork adds over upstream

- **H60B0 (Neon Rope Light 2)** LAN support
- **Panic hardening (in progress)** — critical `.expect()` panics replaced with graceful error handling
- **Exit code fix** — silent `exit(0)` → `exit(1)` so HA properly restarts on failure
- **Scene quick-cycle** — Next/Previous buttons and scene info sensor
- **Undocumented API login** with 2FA support
- **Security vulnerability patches**

See the [full changelog](https://github.com/florianhorner/govee2mqtt-extended#what-this-fork-adds) for upstream status of each change.

## What you'll need

- The **Mosquitto broker** app installed and running in Home Assistant
- Your **Govee account credentials** (email and password from the Govee Home app)
- A **Govee API key** (optional, [get one free](https://developer.govee.com/reference/apply-you-govee-api-key)) for scene control and segment colors

## Setup

See the [installation guide](https://github.com/florianhorner/govee2mqtt-extended/blob/main/docs/ADDON.md) for step-by-step instructions.

## Maintained by

[@florianhorner](https://github.com/florianhorner) — built on the original work by [wez](https://github.com/wez).
