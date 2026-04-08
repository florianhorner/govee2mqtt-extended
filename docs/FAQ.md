# Frequently Asked Questions

## Why can't I turn off a Segment?

Govee's API only supports setting brightness and color for segments — not
power state. However, Home Assistant assumes every light entity has a power
toggle, so one appears in the UI even though it has no effect.

This is a Home Assistant limitation and cannot be removed from the light
entity UI.

## Why is my control over a Segment limited?

Govee2MQTT passes your control requests directly to the Govee device.
What happens after that depends on the device's firmware. Some devices are
more flexible than others — for example, some cannot set segment brightness
to 0, while others tie segment brightness to the main light's brightness.

Govee2MQTT has no way to override this device-specific behavior.

## How do I enable Video Effects for a Light?

Govee's API does not expose video effects directly. To make them available
in Home Assistant:

1. Open the **Govee Home** app and create a **Tap-to-Run** shortcut or a
   saved **Snapshot** that activates the desired video effect.
2. In Home Assistant, go to the **"Govee to MQTT"** device in the MQTT
   integration and click **"Purge Caches"**.

After purging:
* **Tap-to-Run** shortcuts appear as Scene entities in Home Assistant.
* **Snapshots** appear in the Effects list on the device itself.

## My devices appear greyed out / unavailable in Home Assistant

This usually means Home Assistant had trouble registering the device entity
via MQTT. To troubleshoot:

1. Check the **Home Assistant logs** for entries mentioning `gv2mqtt` or
   `mqtt` — these often explain the root cause.
2. Try deleting the affected device(s) from the MQTT integration, then
   clicking **"Purge Caches"** on the "Govee to MQTT" device.

If the issue persists, please [file an issue](https://github.com/wez/govee2mqtt/issues)
with the relevant log entries.

<img src="https://github.com/wez/govee2mqtt/assets/117777/565d8580-f068-4ec3-8c16-11d2808688bf" width="50%">

## Is my device supported?

See [Supported Devices](SKUS.md) for the full list.

## Can you add support for device HXXXX?

Govee2MQTT supports any device that Govee exposes through its APIs — there
is very little device-specific code in the bridge itself. If your device has
Wi-Fi and a Govee API, it should work automatically.

If it doesn't, [file an issue](https://github.com/wez/govee2mqtt/issues) with
your device SKU and we can investigate whether a quirks entry is needed.
See [Supported Devices](SKUS.md) for more details.

## The device MAC addresses in the logs don't match my network MACs

Govee device IDs are **not** network MAC addresses. They are internal
identifiers that may include parts of the BLE MAC but are longer than a
standard MAC address. This is expected behavior.

## "This device should be available via the LAN API, but didn't respond to probing yet"

See [LAN API Troubleshooting](LAN.md) for common causes and solutions.

## "devices not belong you" error in logs

This error comes from Govee's Platform API when it encounters BLE-only
devices with no Wi-Fi support. Please [file an issue](https://github.com/wez/govee2mqtt/issues)
with your device SKU so we can add it to the quirks database.

