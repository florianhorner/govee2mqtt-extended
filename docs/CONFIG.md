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
|`--govee-2fa-code`|`GOVEE_2FA_CODE`|`govee_2fa_code`|One-time verification code, only needed if your account has 2FA. See below.|
|`--api-key`|`GOVEE_API_KEY`|`govee_api_key`|Your Govee API key ([get one here](https://developer.govee.com/reference/apply-you-govee-api-key))|

*Concerned about sharing your credentials? See [Privacy](PRIVACY.md) for
details on how your data is used.*

### Two-factor authentication (2FA)

If your Govee account has two-factor authentication enabled, login will fail with a
clear error in the add-on logs ("Govee account requires 2FA verification...").
To get past it:

1. **Trigger a fresh code.** Sign in to the Govee Home mobile app on your phone with
   the same account. Govee emails a 6-digit verification code to the address on file.
   The code is valid for **about 15 minutes** — work fast.
2. **Set the code.** In Home Assistant, open the add-on configuration panel and paste
   the code into `govee_2fa_code`. In Docker, set `GOVEE_2FA_CODE` in your `.env`.
3. **Restart Govee2MQTT.** Restart the Home Assistant add-on or the Docker
   container so it retries the login with the code attached.

If you see status **454** in the logs, either no code was set or the supplied
code was rejected/expired — go back to step 1.
If you see status **455**, the code was rejected (expired or wrong) — generate
a fresh one and update the config.

You can leave `govee_2fa_code` set after a successful login; Govee remembers the
device. If your token later expires and Govee demands a new code, you'll see the
454 message again — repeat the steps above.

> **Note on token refresh:** Govee's session tokens last days, sometimes weeks.
> When the token eventually expires the add-on re-runs the login. If Govee
> requires a fresh 2FA challenge per login (rather than remembering the device),
> the add-on will fail with status 454 or 455 in the logs and you'll need to
> generate and paste a new code. This is rare in practice but worth knowing if
> Home Assistant suddenly stops seeing your Govee devices long after you set
> things up — check the add-on logs first.

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
