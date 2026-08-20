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

|CLI|ENV|App Config|Default|Purpose|
|---|---|----------|-------|-------|
|`--govee-email`|`GOVEE_EMAIL`|`govee_email`|*(none)*|Your Govee account email|
|`--govee-password`|`GOVEE_PASSWORD`|`govee_password`|*(none)*|Your Govee account password|
|`--govee-2fa-code`|`GOVEE_2FA_CODE`|`govee_2fa_code`|*(none)*|Emailed verification code, only needed if your account has 2FA. See below.|
|`--api-key`|`GOVEE_API_KEY`|`govee_api_key`|*(none)*|Your Govee API key ([get one here](https://developer.govee.com/reference/apply-you-govee-api-key))|

*Concerned about sharing your credentials? See [Privacy](PRIVACY.md) for
details on how your data is used.*

### Two-factor authentication (2FA)

If your Govee account requires two-factor authentication:

1. **Start without a code.** Leave `govee_2fa_code` or `GOVEE_2FA_CODE` unset and
   start Govee2MQTT. On status **454**, it requests a code by email. Retries for
   the same account reuse that request for 15 minutes.
2. **Set the code.** In Home Assistant, paste it into `govee_2fa_code`. In
   Docker, set `GOVEE_2FA_CODE` in your `.env`. Govee codes expire after
   **about 15 minutes**.
3. **Restart Govee2MQTT.** Restart the Home Assistant add-on or Docker container
   to retry login with the code. After login succeeds, remove the code from the
   saved configuration. The running process also clears its copy.

If Govee returns status **454** or **455** with a configured code, clear it and
restart without one. This clears the cached email request, so the next no-code
454 can request another. Codes from Govee's web store (Shopify) use a different
`clientId` and may fail here with status 455. Use the code requested by
Govee2MQTT.

**If no email arrives.** The logs will carry a warning starting
`Could not request a Govee 2FA verification code` — the request itself failed, so
no code was sent. The 2FA instructions above still apply and `govee_2fa_code` is
still the setting to fill in; you just need a code from somewhere else. Restart
to retry the request. Do **not** follow the "remove your Govee API credentials"
suggestion that appears further down that log message — that disables cloud
control entirely and is not the fix for a 2FA prompt.

> **Note on token refresh:** Govee's session tokens last days, sometimes weeks.
> When a later login requires 2FA, repeat the steps above. If Home Assistant
> stops seeing Govee devices, check the add-on logs for status 454 or 455.

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

|CLI|ENV|App Config|Default|What it does|
|---|---|----------|-------|------------|
|`--broadcast-all`|`GOVEE_LAN_BROADCAST_ALL=true`|`broadcast_all`|*(off)*|Sends discovery to every network interface on your system. **Try this first** if multicast doesn't work.|
|`--scan`|`GOVEE_LAN_SCAN=10.0.0.1,10.0.0.2`|`scan`|*(none)*|Sends discovery directly to specific device IPs. Assign your Govee devices static IPs in your router first, then list them here (comma-separated).|
|`--no-multicast`|`GOVEE_LAN_NO_MULTICAST=true`|`no_multicast`|*(off)*|Disables the default multicast discovery. Only use this together with one of the alternatives above.|
|`--global-broadcast`|`GOVEE_LAN_BROADCAST_GLOBAL=true`|`global_broadcast`|*(off)*|Sends discovery to 255.255.255.255. Rarely helps if multicast already fails.|

[More about LAN API troubleshooting](LAN.md)

### Polling behavior on congested networks

Govee2MQTT polls every LAN device for its status at least every 30 seconds
(the pass over all devices is serial, so unresponsive devices stretch the
cycle) and after each command. On a congested 2.4 GHz network, retries for
unresponsive devices used to pile up (up to ~29 packets per device per
status query), making the congestion worse. Two mechanisms bound this; both
are tunable without rebuilding:

|CLI|ENV|App Config|Default|What it does|
|---|---|----------|-------|------------|
|`--lan-query-attempts`|`GOVEE_LAN_QUERY_ATTEMPTS=3`|`lan_query_attempts`|`3`|How many times a status query is sent before giving up. Clamped to 1–100.|
|`--lan-query-backoff-ms`|`GOVEE_LAN_QUERY_BACKOFF_MS=350`|`lan_query_backoff_ms`|`350`|Wait after the first attempt, in milliseconds. Doubles on each retry, capped at 3000 ms (the cap is fixed). Defaults give waits of 350 ms → 700 ms → 1400 ms, ~2.5 s total.|
|`--lan-breaker-threshold`|`GOVEE_LAN_BREAKER_THRESHOLD=3`|`lan_breaker_threshold`|`3`|After this many consecutive timeouts, background polling of that device is suspended (circuit breaker). `0` disables the breaker.|
|`--lan-breaker-cooldown`|`GOVEE_LAN_BREAKER_COOLDOWN=300`|`lan_breaker_cooldown`|`300`|Suspension length in seconds, clamped to 30–900. Doubles on repeated failure, capped at 900 s.|

The tradeoff: lowering attempts makes a congested network recover faster but
makes a slow-to-respond device more likely to report stale state. If you have
a device that reliably answers only after several seconds, raise
`lan_query_attempts` to 4–5. (Above 3 attempts a single query cycle exceeds
the ~5 s post-command confirmation window, so the confirmation poll makes one
full pass instead of re-checking until the commanded value sticks.)

The circuit breaker only affects **background polling**. Commands you send
(turn on, brightness, color) always go out, and their confirmation polls
always run. A suspended device that shows any sign of life — a discovery
response, for example — is granted an immediate status probe instead of
waiting out the cooldown, and the first successful status reply fully
resets the breaker; recovery after an outage therefore takes at most about
a minute. A device that answers discovery but keeps dropping status queries
stays suspended, at the cost of one probe per discovery cycle. Note that a
timed-out confirmation poll still counts toward the breaker threshold even
though it is never blocked: unreachability is evidence no matter which poll
observed it.

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

## Effects (Scenes)

By default Govee2MQTT publishes each light's full list of scene "effects" to
Home Assistant. Very long effect lists can overflow the Google Home
integration, which rejects oversized MQTT discovery payloads. These options
let you trim or disable the published effect list. They are environment-only
(no CLI flag).

|CLI|ENV|App Config|Default|Purpose|
|---|---|----------|-------|-------|
|*(none)*|`GOVEE_DISABLE_EFFECTS=true`|`disable_effects`|*(off)*|Stop publishing effect lists for every device. Use this if a long effect list breaks the Google Home integration.|
|*(none)*|`GOVEE_ALLOWED_EFFECTS=Aurora,Rainbow`|`allowed_effects`|*(all)*|Comma-separated allowlist; only effects matching these names are published. Keeps some effects while shrinking the payload.|

`disable_effects` wins: if it is set, no effects are published regardless of
`allowed_effects`.
