# Supported Devices

Govee2MQTT works with Govee devices that have **Wi-Fi** connectivity.
Bluetooth-only devices (no Wi-Fi chip) are not supported yet.

> **How to find your device's SKU:** Look on the device box, in the Govee
> Home app (device settings), or on the device label itself. It looks like
> `H6072`, `H7160`, etc.

## How control works

Govee2MQTT can talk to your devices in three ways. The more methods
available, the better your experience:

| Method | What it means for you |
|--------|----------------------|
| **LAN API** (local) | Control over your home network — fastest, works without internet. You must [enable it](https://app-h5.govee.com/user-manual/wlan-guide) per device in the Govee Home app. |
| **Platform API** (cloud) | Control via Govee's cloud servers — needed for scenes, effects, and devices without LAN support. Requires a [Govee API key](https://developer.govee.com/reference/apply-you-govee-api-key). |
| **Cloud IoT** (cloud) | Real-time status updates — your devices report changes (on/off, color) within seconds. Requires your Govee account email and password. |

Only devices with LAN API support can be controlled locally without
internet. Currently, only lights support the LAN API — appliances like
humidifiers and heaters require cloud access. This is a hardware
limitation, not a Govee2MQTT limitation.

## Device compatibility

|Device type|LAN API|Platform API (cloud)|Cloud IoT|
|-----------|-------|--------------------|---------|
|**Lights / LED Strips**|Newer Wi-Fi models — enable in the Govee Home app for local control of color, brightness, and power|Most Wi-Fi models — required for scenes and effects|Most Wi-Fi models — enables fast status updates|
|**Humidifiers**|Not supported|Most models work, but control may be limited. Some night lights can't be fully controlled due to Govee firmware bugs.|H7160 only (night light control)|
|**Kettles**|Not supported|Tested with H7171, H7173|Not supported|
|**Heaters, Fans, Purifiers**|Not supported|Tested with H7101, H7102, H7111, H7121, H7130, H7131, H713A, H7135|Not supported|
|**Plugs**|Not supported|Limited — Govee's API for plugs has known bugs|Not supported|

## My device isn't listed

Govee2MQTT works as a bridge to Govee's APIs — it doesn't contain much
device-specific code. If your device has Wi-Fi and connects to the Govee
Home app, there's a good chance it already works.

If it doesn't, [file an issue](https://github.com/wez/govee2mqtt/issues)
with your device SKU and we can investigate.
