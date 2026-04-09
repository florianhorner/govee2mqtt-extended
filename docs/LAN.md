# LAN API: Local Device Control

The LAN API lets Govee2MQTT control your devices directly over your home
network — no internet required. This means faster response times and
continued operation even if your internet goes down.

**Not all devices support the LAN API.** Check
[Govee's LAN API guide](https://app-h5.govee.com/user-manual/wlan-guide)
for a list of supported devices.

## Prerequisites

Before troubleshooting, make sure you've done these:

1. **Enable LAN API on each device** — In the Govee Home app, go to
   the device's settings (gear icon) and turn on "LAN Control."
2. **Govee2MQTT must be on the same network** as your Govee devices.
3. **UDP ports 4001, 4002, and 4003** must not be blocked by your
   router or firewall.

## If your devices aren't found

Govee2MQTT discovers devices by sending a message to a special network
address (multicast). Some routers and Wi-Fi setups block this traffic.

**Try these in order:**

### 1. Enable "Broadcast to Each Network Interface"

This sends discovery messages to every network adapter on your system
instead of using multicast. In most cases, this solves the problem.

Set `broadcast_all` to `true` in the app config (or `GOVEE_LAN_BROADCAST_ALL=true`
in Docker).

### 2. Target specific device IPs

If broadcasting doesn't work, you can tell Govee2MQTT exactly where your
devices are:

1. Assign a **static IP address** to each Govee device in your router's
   DHCP settings (this prevents the IP from changing).
2. Enter those IPs in the `scan` field (comma-separated), e.g.
   `10.0.0.50,10.0.0.51`.

### 3. Check your router settings

- Some routers block multicast traffic between Wi-Fi and wired networks.
  Look for "multicast" or "IGMP" settings in your router's admin panel.
- If your Govee devices are on a separate network or VLAN, make sure
  your firewall allows UDP traffic on ports 4001-4003 between networks.
- Don't confuse "multicast DNS" (mDNS/Bonjour) with general multicast —
  having mDNS working doesn't guarantee other multicast traffic works.

## Technical details

For those who want the full picture:

- Govee devices with LAN API listen on **UDP port 4001** and join
  multicast group **239.255.255.250**.
- Govee2MQTT binds to **UDP port 4002** to receive responses.
- When a device responds, it sends to the **source IP** of the discovery
  packet but always uses **port 4002** (not the originating port).
  Your network must allow this return traffic.

See [Configuration → LAN API Control](CONFIG.md#lan-api-control) for all
available settings.
