# Installing as a Home Assistant App

If you are running **HAOS** or **Supervised** Home Assistant, your
installation supports Home Assistant apps (formerly called "add-ons").

If you installed Home Assistant using a different method (e.g. Docker,
Core), you cannot install apps and should follow the
[Docker guide](DOCKER.md) instead.

## What you'll do

Installation takes about 5 minutes:

1. Enable Advanced Mode in your profile
2. Install and start the Mosquitto MQTT broker
3. Enable the MQTT integration
4. Add this repository and install Govee2MQTT
5. Enter your Govee credentials and start the app

## Step 1: Enable Advanced Mode

**Go to** your user profile (click the profile icon in the bottom left).
**Scroll down** and **turn on** "Advanced Mode" — this makes Govee2MQTT
visible in the app list.

![image](https://github.com/wez/govee-lan-hass/assets/117777/444c399d-0a91-41bf-804e-efcbabe17635)

## Step 2: Set up MQTT

1. **Go to** the Apps section: https://my.home-assistant.io/redirect/supervisor
2. **Click** the **"Apps"** button in the bottom right corner
3. **Find** "Mosquitto Broker", click it, **install** it, then **start** it
4. **Go to** Settings → Devices & Services — you should see a prompt to
   enable the MQTT integration. **Click** it and enable it.

## Step 3: Install Govee2MQTT

1. **Go to** the Apps section: https://my.home-assistant.io/redirect/supervisor
2. **Click** the **"Apps"** button in the bottom right corner
3. **Click** the three-dot menu (⋮) in the top right corner:

![image](https://github.com/wez/govee-lan-hass/assets/117777/c425615b-d7be-4ff2-a0d9-c8b7cfb8b63e)

4. **Click** "Repositories"
5. **Enter** `https://github.com/wez/govee2mqtt` and click **"Add"**
   > **Extended fork:** Use `https://github.com/florianhorner/govee2mqtt-extended` instead for additional fixes and device support.
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
3. **Fill in** at least your Govee email, password, and API key
4. **Click** "Save" (bottom right)
5. **Click** the "Info" tab, then **click** "Start"

## Verify

1. **Check** the "Logs" tab (top right) to see startup diagnostics
2. After a few seconds, your devices should appear under the MQTT
   integration in Settings → Devices & Services
