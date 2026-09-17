# Changelog

## [2026.09.16-ca472450] - 2026-09-16

Govee fans and air purifiers now show up as fan cards. Lights with music mode get a sensitivity slider, two-factor sign-in emails you the code on its own, and switching an H7124's fan on or off leaves its nightlight alone.

### Before you update

Air Quality and Filter Life on air purifiers now publish a plain number. If a template reads them through `value_json.value`, drop the `.value` part or it will come back empty. Temperature and humidity sensors now show unknown when a reading is missing, instead of holding the last value.

If you have ever posted an add-on log in a GitHub issue or a forum thread, clear it and consider changing your Govee password. All earlier builds of this fork, from 2026.03.16 through 2026.08.06-bf75797f, copied Govee's raw login response into the log when a login failed or came back in a shape the bridge couldn't parse. When that happened to a successful login, the logged response included your account's authentication token. This version keeps it out of the log.

### What's new

- Fans and air purifiers get a fan card with an on/off button, plus a speed slider and preset modes when the device lists them. Your existing entities stay where they are. This needs a Govee API key.
- On an H7124, the fan's power button no longer switches the nightlight too. The nightlight may flicker while the bridge puts it back, and if that fails the error shows up in the add-on log.
- Air Quality moves onto the main device card, and both purifier sensors record long-term statistics.
- Lights with music mode get a Music Sensitivity slider, which applies to the next `Music:` effect you pick. This also needs a Govee API key.
- Turn on `music_palette` to give music mode your own colours over LAN. It's off by default, and nobody has confirmed the palette writes on anything other than an H607C yet.
- If your account uses two-factor authentication, the bridge asks Govee to email you the code. Start without a code, paste it into `govee_2fa_code`, restart, then clear the field.
- Effect and scene lists no longer contain blank entries that failed when picked.
- A device that stops answering on your network gets 3 status queries instead of up to about 29, and one device with broken metadata can no longer stop the others from registering.

### Known issues

- Music Sensitivity goes back to the default when the add-on restarts.
- Without an AWS IoT connection, the fan card can show a speed the device didn't accept.
- If a platform data refresh changes a fan's work modes, Home Assistant keeps the old speed range until you purge caches or restart the add-on.

### Thanks

Paulo Silveira ([@peas](https://github.com/peas)), the first outside contributor to get pull requests merged into this fork, wrote the LAN music palette and the IoT work-mode reporting, and reported the two-factor sign-in trap with a working workaround. [@PhotonSpheres](https://github.com/PhotonSpheres) spotted that the H60B0 is a floor lamp, and [@VIDIVERSE](https://github.com/VIDIVERSE) reproduced the two-factor failure.

The [full release notes](https://github.com/florianhorner/govee2mqtt-extended/releases/tag/2026.09.16-ca472450) list the MQTT topics, entity IDs and template details.
