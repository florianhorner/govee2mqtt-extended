# Configuration Options

There are three ways to configure Govee2MQTT, depending on how you installed it:

| Column | When to use |
|--------|-------------|
| **CLI** | Command-line flags, e.g. `govee serve --govee-email user@example.com` |
| **ENV** | Environment variables in a `.env` file or `docker-compose.yml` |
| **App Config** | The configuration panel in the Home Assistant app UI |

If you installed Govee2MQTT as a **Home Assistant app**, use the **App Config** column.
If you're running in **Docker**, use the **ENV** column.

## Govee Credentials

Govee2MQTT can run without any Govee credentials, but it will only discover
devices that have LAN control enabled. For the best experience, configure
your credentials before your first run:

- **Email + password** (recommended) — the same login you use in the Govee Home app. This is the only way for Govee2MQTT to learn your room names and assign devices to the right Home Assistant areas.
- **API key** (optional but recommended) — enables scene control, segment colors, and music modes. [Get a free key from Govee's developer portal](https://developer.govee.com/reference/apply-you-govee-api-key).

|CLI|ENV|App Config|Purpose|
|---|---|----------|-------|
|`--govee-email`|`GOVEE_EMAIL`|`govee_email`|Your Govee account email|
|`--govee-password`|`GOVEE_PASSWORD`|`govee_password`|Your Govee account password|
|`--api-key`|`GOVEE_API_KEY`|`govee_api_key`|Your Govee API key ([get one here](https://developer.govee.com/reference/apply-you-govee-api-key))|

*Concerned about sharing your credentials? See [Privacy](PRIVACY.md) for
details on how your data is used.*

## LAN API Control

Many Govee devices support local control over your home network, without
needing internet access. This is faster and more reliable than cloud control.

**Before you start:** You must enable the LAN API for each device individually
in the Govee Home app (device settings → LAN Control toggle).

The [Govee LAN API guide](https://app-h5.govee.com/user-manual/wlan-guide)
lists which devices support it.

### How discovery works

By default, Govee2MQTT finds devices using multicast — it sends a message to
a special network address and waits for devices to respond. This works
automatically on most networks.

**If your devices aren't found:** Some routers and Wi-Fi setups block
multicast traffic. Try these alternatives in order:

|CLI|ENV|App Config|What it does|
|---|---|----------|------------|
|`--broadcast-all`|`GOVEE_LAN_BROADCAST_ALL=true`|`broadcast_all`|Sends discovery to every network interface on your system. **Try this first** if multicast doesn't work.|
|`--scan`|`GOVEE_LAN_SCAN=10.0.0.1,10.0.0.2`|`scan`|Sends discovery directly to specific device IPs. Assign your Govee devices static IPs in your router first, then list them here (comma-separated).|
|`--no-multicast`|`GOVEE_LAN_NO_MULTICAST=true`|`no_multicast`|Disables the default multicast discovery. Only use this together with one of the alternatives above.|
|`--global-broadcast`|`GOVEE_LAN_BROADCAST_GLOBAL=true`|`global_broadcast`|Sends discovery to 255.255.255.255. Rarely helps if multicast already fails.|

[More about LAN API troubleshooting](LAN.md)

## MQTT Configuration

MQTT is the messaging protocol that connects Govee2MQTT to Home Assistant.
You need an MQTT broker (server) running — the
[Mosquitto app](https://www.home-assistant.io/integrations/mqtt/#configuration)
is the easiest option.

**Home Assistant app users:** If you installed Mosquitto as a Home Assistant
app, leave these fields blank — they are filled in automatically.

|CLI|ENV|App Config|Default|Purpose|
|---|---|----------|-------|-------|
|`--mqtt-host`|`GOVEE_MQTT_HOST`|`mqtt_host`|*(auto-detected)*|Host name or IP address of your MQTT broker|
|`--mqtt-port`|`GOVEE_MQTT_PORT`|`mqtt_port`|`1883`|Port number of your MQTT broker|
|`--mqtt-username`|`GOVEE_MQTT_USER`|`mqtt_username`|*(none)*|Username, if your broker requires authentication|
|`--mqtt-password`|`GOVEE_MQTT_PASSWORD`|`mqtt_password`|*(none)*|Password, if your broker requires authentication|
