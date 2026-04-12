# Installing as a Home Assistant App

If you are running **HAOS** or **Supervised** Home Assistant, your
installation supports Home Assistant apps (formerly called "add-ons").

> **Not sure which installation type you have?** Check Settings → About
> in Home Assistant. If you see "Home Assistant OS" or "Supervised," you're
> in the right place. Otherwise, use the [Docker guide](DOCKER.md).

## What you'll do

Installation takes about 5 minutes:

1. Enable Advanced Mode in your profile
2. Install the Mosquitto MQTT broker (the messaging service Govee2MQTT uses to talk to Home Assistant)
3. Enable the MQTT integration
4. Add this repository and install Govee2MQTT
5. Enter your Govee credentials and start the app

## Step 1: Enable Advanced Mode

**Go to** your user profile (click the profile icon in the bottom left).
**Scroll down** and **turn on** "Advanced Mode" — this makes Govee2MQTT
visible in the app list.

![image](https://github.com/wez/govee-lan-hass/assets/117777/444c399d-0a91-41bf-804e-efcbabe17635)

## Step 2: Set up MQTT

MQTT is a messaging protocol — think of it as a mailbox that Govee2MQTT
and Home Assistant both check. The Mosquitto broker app runs this mailbox.

1. **Go to** **Settings → Apps**: https://my.home-assistant.io/redirect/supervisor
2. **Select** **"App store"**
3. **Find** "Mosquitto Broker", click it, **install** it, then **start** it
4. **Go to** Settings → Devices & Services — you should see a prompt to
   enable the MQTT integration. **Click** it and enable it.

## Step 3: Install Govee2MQTT

1. **Go to** **Settings → Apps**: https://my.home-assistant.io/redirect/supervisor
2. **Select** **"App store"**
3. **Click** the three-dot menu (⋮) in the top right corner:

![image](https://github.com/wez/govee-lan-hass/assets/117777/c425615b-d7be-4ff2-a0d9-c8b7cfb8b63e)

4. **Click** "Repositories"
5. **Enter** `https://github.com/wez/govee2mqtt` and click **"Add"**
   > **Using this fork?** Use `https://github.com/florianhorner/govee2mqtt-extended` instead for additional fixes and device support. See the [README](../README.md#what-this-fork-adds) for details.
6. You should see:

![image](https://github.com/wez/govee-lan-hass/assets/117777/a2603e2d-dec1-4711-8d94-c957bf4a7a01)

7. **Click** "Close"
8. You should now see the Govee2MQTT app:

![image](https://github.com/wez/govee-lan-hass/assets/117777/4e70f5e4-d54e-4e95-94db-b1d4a562eab1)

9. **Click** on it, then click **"Install"**

## Step 4: Configure and start

1. **Click** the **"Configuration"** tab at the top of the screen

![image](https://github.com/wez/govee-lan-hass/assets/117777/fd2953b5-a576-4ab4-a903-0330a749ae97)

2. **Check** "Show unused optional configuration options"
3. **Fill in** your credentials:
   - **Govee Email** and **Password** — the same ones you use in the Govee Home app. Recommended for full device support, room names, and scenes.
   - **Govee API Key** (optional but recommended) — enables scene control, segment colors, and music modes. [Get a free key here](https://developer.govee.com/reference/apply-you-govee-api-key).
   - **MQTT fields** — leave blank if you installed Mosquitto in Step 2 (they are filled in automatically).
4. **Click** "Save" (bottom right)
5. **Click** the "Info" tab, then **click** "Start"

## What to expect after setup

Once Govee2MQTT starts, here's what happens in Home Assistant:

**Under Settings → Devices & Services → MQTT**, you'll see:

- A **"Govee to MQTT"** device — this is the bridge itself, not a Govee device. It has a "Purge Caches" button and a version sensor.
- **One device per Govee product** — each light, humidifier, etc. appears as its own device with controls matching what Govee's API supports.

**For each light**, you'll typically see:

- A **light entity** with on/off, brightness, color, and color temperature controls
- An **effects list** with your DIY scenes and music modes (requires API key)
- **"Scene Next" / "Scene Previous" buttons** for cycling through scenes (this fork only)
- **Segment entities** (e.g. "Segment 001") if your device supports segment control (requires API key)

**For humidifiers, heaters, fans**, etc.: a climate or humidifier entity with the controls Govee's API exposes for that model.

**Status updates** arrive within a few seconds when you control devices from the Govee app or physical buttons. Devices with LAN API enabled update the fastest.

## Verify

1. **Check** the "Logs" tab (top of screen) to see startup diagnostics
2. After a few seconds, your devices should appear under the MQTT
   integration in **Settings → Devices & Services**

**If your devices don't appear:**
- Check the Logs tab for error messages
- Make sure the LAN API is enabled in the Govee Home app for each device (Settings → gear icon on the device → LAN Control)
- See the [FAQ](FAQ.md) and [LAN troubleshooting](LAN.md) for common issues
