# Music Mode with Colour Palettes

Govee's music mode makes a light react to sound picked up by the device's
own microphone. Two things control how it looks: the **style** (the motion
pattern, e.g. "Rhythm" or "Hopping") and the **palette** (which colours the
reactive points use).

The official APIs only get you half-way there:

- The **Platform API** can pick a style and a sensitivity, but its
  `musicMode.rgb` field is a single packed integer — one colour. There is
  no way to express a multi-colour palette through it, and `autoColor: 0`
  does not unlock one.
- The **LAN API**'s documented commands don't cover music mode at all.

This feature closes the gap: a command topic that programs music mode with
an arbitrary palette by speaking the device's internal protocol over LAN
(`ptReal` frames on UDP port 4003).

## Platform API controls

Platform-backed lights already expose each style as a `Music: <style>` effect
and a **Music Sensitivity** number in Home Assistant. To select the Platform
API's one fixed colour, include `rgb_color` with the effect:

```yaml
service: light.turn_on
target:
  entity_id: light.your_govee
data:
  effect: "Music: Rhythm"
  rgb_color: [18, 52, 86]
```

The colour makes the bridge send `autoColor: 0`; omitting it sends
`autoColor: 1` and lets the device choose colours. This path needs no opt-in,
but it carries only one colour. Use the LAN command below for a real palette.

## Enabling it (opt-in)

The topic only acts when the bridge runs with:

```sh
GOVEE_MUSIC_PALETTE=true
```

In the Home Assistant add-on, set the **Music Mode Palettes (LAN)**
(`music_palette`) option instead; the add-on exports the environment variable
for you. See the [configuration reference](CONFIG.md#music-mode).

It is off by default because the frames below are reverse-engineered and
only mapped for a handful of SKUs; see [Supported devices](#supported-devices).
The device must also be reachable over LAN ("LAN Control" enabled in the
Govee Home app — see [LAN.md](LAN.md)).

## Usage

Publish JSON to `gv2mqtt/<device-id>/set-music-palette`:

```json
{"style": "Rhythm", "colors": ["#ff7a00", "#1400c8", "#4a00e0"], "sensitivity": 99}
```

- `style`: one of the styles mapped for your SKU (Govee app spelling,
  case-insensitive).
- `colors`: 1 to 7 `#rrggbb` entries. The device cycles its reactive
  points through them.
- `sensitivity`: 0–100, optional (default 100).

Example with Home Assistant:

```yaml
service: mqtt.publish
data:
  topic: gv2mqtt/AA:BB:CC:DD:EE:FF:11:22/set-music-palette
  payload: '{"style": "Hopping", "colors": ["#ff0000", "#0000ff"], "sensitivity": 80}'
```

To leave music mode, set a colour or colour temperature on the light as
usual — a plain colour command exits music mode on every SKU tested.

Notes:

- The frames are sent **twice**, 300ms apart, like the Govee app does:
  the transport is UDP without acknowledgement, and the sequence is
  idempotent.
- Brightness is deliberately not part of the payload — use the light's
  normal brightness control.
- `sensitivity` here writes the device's LAN/BLE slot — the one the
  `aa 05 13` read-back reports. The Platform API's `musicMode.sensitivity`
  (used by the `Music: <style>` effects and any HA sensitivity preference
  built on it) is a different slot; the two do not reflect each other, and
  this topic is deliberately not bound to that entity.
- The LAN `devStatus` poll keeps reporting the last static colour while
  the device dances; the reliable "am I in music mode?" signal is the
  `mode` field from AWS IoT status pushes (see the `mode`/`mode_updated`
  fields on the state topic).

## Supported devices

Music styles are selected on the wire by a **profile id** that is
SKU-specific: the same style name maps to different bytes on different
SKUs, and ids are not portable. The mapped SKUs live in
[`src/music.rs`](../src/music.rs); at the time of writing:

| SKU | Device | Styles |
|-----|--------|--------|
| H607C | Floor lamp | Touching, Rhythm, Splash, Stippling, Hopping, Luminous, Blend, Fantasy, Spring |
| H6020 | Table lamp | Rhythm, Beat A, Gridding, Energic, Dandelion, Drifting |
| H60B0 | Uplighter floor lamp | Stippling, Hopping, Luminous, Rhythm, Flowing Light, Sprouting, Shiny |

The style ids above were captured from app traffic and visually verified
on the three SKUs. The palette frames themselves have live runtime
verification on H607C only (2- and 5-colour writes, confirmed by the
device's IoT echo and `aa 05 13` read-back — see `proof/`); on H6020 and
H60B0 they rely on the shared classic dialect and golden tests against
the Python reference the styles were validated with.
Some SKUs (e.g. H7025) use a different frame dialect with per-style
trailing bytes and are intentionally not supported yet.

## Mapping a new SKU

Contributions to the table are welcome. State the SKU, the firmware
version, how you captured the ids, and which style/palette combinations
you verified visually.

The read-back procedure:

1. Put the device in the target music style — use the Govee Home app, or
   this feature's own topic on a mapped SKU.
2. Watch the bridge's debug log for the next AWS IoT status push from the
   device. It carries an `op.command` frame of the form
   `aa 05 13 <profile> <sensitivity>` — that `<profile>` byte is the id
   for the active style.
3. Repeat per style, then verify each entry by sending a palette to it
   and checking the device visually.

Caveats learned the hard way:

- Setting the style via the **app** reliably updates the read-back frame.
  Setting it via the **Platform API** did not update `aa 05 13` on the
  H607C we tested — treat Platform-set read-back as unconfirmed and
  prefer the app when mapping.
- A wrong profile id is usually ignored silently, but on some dialects a
  malformed sequence can hang the light until it is power-cycled. Verify
  on hardware you can reach.
