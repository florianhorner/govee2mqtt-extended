# Changelog

This changelog summarizes user-facing changes in each add-on version.

## [2026.09.15-2dad7cf2] - 2026-09-15 03:06

### ⚠️ Breaking Changes

- Purifier `airQuality` and `filterLifeTime` sensors now publish scalar values instead of JSON wrappers. Remove any `value_json.value` template used to read these sensors.

### ⚡ Performance

- Reduce LAN traffic to unresponsive devices with bounded status-query retries and a per-device polling circuit breaker ([#40](https://github.com/florianhorner/govee2mqtt-extended/issues/40)).

### 🐛 Bug Fixes

- Identify the H60B0 Uplighter as a LAN-capable floor lamp instead of a light strip ([#48](https://github.com/florianhorner/govee2mqtt-extended/issues/48)).
- Request a Govee verification email when login requires two-factor authentication ([#46](https://github.com/florianhorner/govee2mqtt-extended/issues/46)).
- Keep authenticated response bodies out of error messages ([#52](https://github.com/florianhorner/govee2mqtt-extended/issues/52)).
- Keep Home Assistant command topics and handlers aligned, and reject malformed route values ([#55](https://github.com/florianhorner/govee2mqtt-extended/issues/55)).

### 🚀 Features

- Add native Home Assistant fan entities for Govee fans and air purifiers, with speed, preset, and oscillation controls when device metadata supports them ([#56](https://github.com/florianhorner/govee2mqtt-extended/issues/56)).
- Add a music sensitivity slider, reset control, and opt-in LAN palette commands ([#53](https://github.com/florianhorner/govee2mqtt-extended/issues/53)).
- Expose the work mode reported through Govee's IoT status messages and when it was observed ([#38](https://github.com/florianhorner/govee2mqtt-extended/issues/38), [#44](https://github.com/florianhorner/govee2mqtt-extended/issues/44)).
