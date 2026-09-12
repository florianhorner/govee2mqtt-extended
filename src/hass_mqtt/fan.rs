//! Home Assistant `fan` entity for Govee fans and air purifiers.
//!
//! Modelled on [`crate::hass_mqtt::humidifier`], which solves the same problem
//! for a different HA domain. Power and oscillation reuse the generic switch
//! route and presets reuse `set-work-mode`, so the only new MQTT route this
//! adds is `set-percentage` -- speed is the one control no existing route can
//! express for every device shape (see
//! [`crate::hass_mqtt::work_mode::SpeedAxis`]).
//!
//! <https://www.home-assistant.io/integrations/fan.mqtt/>

use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::command_routes::{
    instantiate_route, CommandTopic, FAN_SET_PERCENTAGE_ROUTE, SET_WORK_MODE_ROUTE,
    SWITCH_COMMAND_ROUTE,
};
use crate::hass_mqtt::instance::{publish_entity_config, EntityInstance};
use crate::hass_mqtt::work_mode::{ParsedWorkMode, SpeedAxis};
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{availability_topic, topic_safe_id, HassClient, IdParameter};
use crate::service::state::StateHandle;
use anyhow::anyhow;
use async_trait::async_trait;
use mosquitto_rs::router::{Params, Payload, State};
use serde::Serialize;
use serde_json::{json, Value as JsonValue};

/// The instance name Govee uses for a fan or purifier's oscillation toggle.
const OSCILLATION_INSTANCE: &str = "oscillationToggle";

/// Home Assistant's sentinel for "this axis has no value right now".
///
/// Published to whichever of percentage/preset is NOT currently in effect.
/// Govee's `workMode` is a single mutually-exclusive integer, but HA does not
/// clear one axis when the other is set (home-assistant/core#50435), so
/// without an explicit reset the entity reports a speed AND a preset at once.
const RESET_PAYLOAD: &str = "None";

/// HA reserves `speed_range_min - 1` for off. With `speed_range_min: 1` that
/// is 0, and the fan card publishes it when the user drags the slider to zero.
const SPEED_RANGE_MIN: u8 = 1;

/// One outbound state publish, carrying which axis it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FanPublish {
    Power(&'static str),
    Percentage(String),
    PresetMode(String),
}

/// <https://www.home-assistant.io/integrations/fan.mqtt/>
#[derive(Serialize, Clone, Debug)]
pub struct FanConfig {
    #[serde(flatten)]
    pub base: EntityConfig,

    /// Power. Routed to the generic switch handler rather than owning a route.
    pub command_topic: CommandTopic,
    pub state_topic: String,

    pub optimistic: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub percentage_command_topic: Option<CommandTopic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percentage_state_topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_range_min: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_range_max: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_reset_percentage: Option<&'static str>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_mode_command_topic: Option<CommandTopic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_mode_state_topic: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub preset_modes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_reset_preset_mode: Option<&'static str>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub oscillation_command_topic: Option<CommandTopic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oscillation_state_topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_oscillation_on: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_oscillation_off: Option<&'static str>,
}

#[derive(Clone)]
pub struct Fan {
    fan: FanConfig,
    state: StateHandle,
    device_id: String,
    /// Resolved once at discovery time so `notify_state` reads back ordinals
    /// with exactly the mapping the command topic advertises.
    axis: Option<SpeedAxis>,
}

impl Fan {
    /// Build the fan entity for a device, or `Ok(None)` when there is nothing
    /// controllable to expose.
    ///
    /// Returns `Ok(None)` rather than `Err` for missing or unparseable
    /// `workMode` metadata. `enumerate_all_entites` isolates a device's
    /// enumeration failure but still drops **all** of that device's entities,
    /// so an `Err` here would cost a device the switch and sensors it already
    /// had. Upstream wez/govee2mqtt#713 is exactly this case: an H1310 ceiling
    /// fan has a power toggle and no usable `workMode`, and must gain a working
    /// on/off fan rather than lose everything.
    pub async fn new(device: &ServiceDevice, state: &StateHandle) -> anyhow::Result<Option<Self>> {
        let Some(info) = &device.http_device_info else {
            return Ok(None);
        };
        // Without a power switch there is no fan to speak of: HA requires
        // `command_topic`, and we have nothing to point it at.
        if info.capability_by_instance("powerSwitch").is_none() {
            return Ok(None);
        }

        let id = topic_safe_id(device);
        let use_iot = device.iot_api_supported() && state.get_iot_client().await.is_some();

        let command_topic = instantiate_route(
            SWITCH_COMMAND_ROUTE,
            &[("id", &id), ("instance", "powerSwitch")],
        )?;

        // A device whose workMode cannot be parsed still gets an on/off fan.
        let parsed = ParsedWorkMode::with_device(device).ok();
        let (axis, preset_modes) = match &parsed {
            Some(parsed) => {
                let controls = parsed.classify_fan_controls();
                let mut presets: Vec<String> = controls
                    .presets
                    .iter()
                    .map(|mode| mode.name.clone())
                    // Home Assistant rejects the ENTIRE fan payload when
                    // `preset_modes` contains `payload_reset_preset_mode`, and
                    // these names are raw vendor strings.
                    .filter(|name| name != RESET_PAYLOAD)
                    .collect();
                presets.sort();
                (controls.axis, presets)
            }
            None => (None, vec![]),
        };

        // `classify_fan_controls` already bounds the axis to something Home
        // Assistant and the `u8` command encoding can both express. Degrade
        // rather than error if that ever stops holding: an `Err` here would
        // propagate through `advise_hass_of_light_state`, which re-enumerates
        // on EVERY state change, costing the device not just its fan but every
        // entity it has -- permanently, not just at discovery.
        let axis = axis.filter(|axis| {
            let expressible = u8::try_from(axis.max_ordinal()).is_ok() && axis.max_ordinal() >= 2;
            if !expressible {
                log::warn!(
                    "{device} produced a {n}-step fan speed axis, which Home \
                     Assistant cannot express; falling back to a preset-only fan",
                    n = axis.max_ordinal()
                );
            }
            expressible
        });

        let (
            percentage_command_topic,
            percentage_state_topic,
            speed_range_max,
            payload_reset_percentage,
        ) = match &axis {
            Some(axis) => (
                Some(instantiate_route(FAN_SET_PERCENTAGE_ROUTE, &[("id", &id)])?),
                Some(format!("gv2mqtt/fan/{id}/notify-percentage")),
                Some(axis.speed_range_max()),
                Some(RESET_PAYLOAD),
            ),
            None => (None, None, None, None),
        };

        let (preset_mode_command_topic, preset_mode_state_topic, payload_reset_preset_mode) =
            if preset_modes.is_empty() {
                (None, None, None)
            } else {
                (
                    Some(instantiate_route(SET_WORK_MODE_ROUTE, &[("id", &id)])?),
                    Some(format!("gv2mqtt/fan/{id}/notify-preset-mode")),
                    Some(RESET_PAYLOAD),
                )
            };

        // Oscillation reuses the generic toggle route, which already handles
        // any capability instance. Only advertise it when the device has one:
        // HA renders the control unconditionally once the topic is present.
        let oscillates = info.capability_by_instance(OSCILLATION_INSTANCE).is_some();
        let (
            oscillation_command_topic,
            oscillation_state_topic,
            payload_oscillation_on,
            payload_oscillation_off,
        ) = if oscillates {
            (
                Some(instantiate_route(
                    SWITCH_COMMAND_ROUTE,
                    &[("id", &id), ("instance", OSCILLATION_INSTANCE)],
                )?),
                Some(format!("gv2mqtt/fan/{id}/notify-oscillation")),
                // `mqtt_switch_command` parses ON/OFF, not HA's
                // `oscillate_on`/`oscillate_off` defaults.
                Some("ON"),
                Some("OFF"),
            )
        } else {
            (None, None, None, None)
        };

        Ok(Some(Self {
            fan: FanConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    // The fan IS the device's primary control, so it takes the
                    // device name rather than adding a suffix.
                    name: None,
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: format!("gv2mqtt-{id}-fan"),
                    entity_category: None,
                    icon: None,
                },
                command_topic,
                state_topic: format!("gv2mqtt/fan/{id}/state"),
                optimistic: !use_iot,
                percentage_command_topic,
                percentage_state_topic,
                speed_range_min: speed_range_max.map(|_| SPEED_RANGE_MIN),
                speed_range_max,
                payload_reset_percentage,
                preset_mode_command_topic,
                preset_mode_state_topic,
                preset_modes,
                payload_reset_preset_mode,
                oscillation_command_topic,
                oscillation_state_topic,
                payload_oscillation_on,
                payload_oscillation_off,
            },
            state: state.clone(),
            device_id: device.id.to_string(),
            axis,
        }))
    }

    /// Which topics to publish, **in the order they must reach the broker**.
    ///
    /// Govee's `workMode` is one mutually-exclusive integer, so a device is
    /// either on a speed or in a preset, never both. Home Assistant does not
    /// clear one axis when the other is set (home-assistant/core#50435), so the
    /// inactive axis is explicitly reset -- and the reset is emitted BEFORE the
    /// live value, so HA never observes an instant where both are populated.
    ///
    /// Returned as a list rather than published inline so both the decision and
    /// the ordering are testable without a broker.
    fn state_publishes(
        &self,
        is_on: bool,
        reported: Option<(i64, Option<i64>)>,
        parsed: Option<&ParsedWorkMode>,
    ) -> Vec<FanPublish> {
        let mut percentage = None;
        let mut preset = None;

        if is_on {
            if let Some((work_mode, mode_value)) = reported {
                percentage = self
                    .axis
                    .as_ref()
                    .and_then(|axis| axis.ordinal_for_state(work_mode, mode_value));

                if percentage.is_none() {
                    preset = parsed
                        .and_then(|parsed| {
                            parsed
                                .mode_for_value(&json!(work_mode))
                                .map(|mode| mode.name.clone())
                        })
                        // Publishing a preset that is not in `preset_modes`
                        // makes HA log an error on every single update.
                        .filter(|name| self.fan.preset_modes.contains(name));
                }
            }
        }

        let mut publishes = vec![FanPublish::Power(if is_on { "ON" } else { "OFF" })];

        if percentage.is_none() {
            publishes.push(FanPublish::Percentage(RESET_PAYLOAD.to_string()));
        }
        if preset.is_none() {
            publishes.push(FanPublish::PresetMode(RESET_PAYLOAD.to_string()));
        }
        if let Some(ordinal) = percentage {
            publishes.push(FanPublish::Percentage(ordinal.to_string()));
        }
        if let Some(name) = preset {
            publishes.push(FanPublish::PresetMode(name));
        }

        publishes
    }

    /// Map one publish onto the topic it belongs to.
    ///
    /// Separate from `notify_state` so the wiring is testable: swapping the
    /// percentage and preset topics here would be invisible to every
    /// `state_publishes` test, because none of them reach this mapping.
    fn topic_and_payload<'a>(&'a self, publish: &'a FanPublish) -> Option<(&'a str, &'a str)> {
        match publish {
            FanPublish::Power(payload) => Some((self.fan.state_topic.as_str(), payload)),
            FanPublish::Percentage(payload) => Some((
                self.fan.percentage_state_topic.as_deref()?,
                payload.as_str(),
            )),
            FanPublish::PresetMode(payload) => Some((
                self.fan.preset_mode_state_topic.as_deref()?,
                payload.as_str(),
            )),
        }
    }

    /// The reported `(workMode, modeValue)` pair, when the device has told us.
    fn reported_work_mode(device: &ServiceDevice) -> Option<(i64, Option<i64>)> {
        let cap = device.get_state_capability_by_instance("workMode")?;
        let work_mode = cap.state.pointer("/value/workMode")?.as_i64()?;
        let mode_value = cap
            .state
            .pointer("/value/modeValue")
            .and_then(|v| v.as_i64());
        Some((work_mode, mode_value))
    }
}

#[async_trait]
impl EntityInstance for Fan {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("fan", state, client, &self.fan.base, &self.fan).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) = self.state.device_by_id(&self.device_id).await else {
            log::warn!(
                "Device {} not found in state, skipping notify",
                self.device_id
            );
            return Ok(());
        };

        let is_on = device.device_state().map(|s| s.on).unwrap_or(false);
        let parsed = ParsedWorkMode::with_device(&device).ok();

        for publish in
            self.state_publishes(is_on, Self::reported_work_mode(&device), parsed.as_ref())
        {
            // An axis with no advertised state topic is skipped rather than
            // published to a topic Home Assistant never subscribed to.
            let Some((topic, payload)) = self.topic_and_payload(&publish) else {
                continue;
            };
            client.publish(topic, payload).await?;
        }

        if let Some(topic) = &self.fan.oscillation_state_topic {
            if let Some(cap) = device.get_state_capability_by_instance(OSCILLATION_INSTANCE) {
                if let Some(payload) = oscillation_payload(&cap.state) {
                    client.publish(topic, payload).await?;
                }
            }
        }

        Ok(())
    }
}

/// Govee reports a toggle as an integer; anything non-zero is on.
///
/// `None` when the capability carries no integer under `/value`, so a
/// malformed reading leaves the previous state rather than forcing "OFF".
fn oscillation_payload(cap_state: &JsonValue) -> Option<&'static str> {
    let value = cap_state.pointer("/value")?.as_i64()?;
    Some(if value != 0 { "ON" } else { "OFF" })
}

/// Resolve a Home Assistant fan percentage ordinal into a Govee work-mode
/// command.
///
/// Split out of the MQTT handler so the mapping is testable without a broker
/// or a live device.
pub fn fan_percentage_command(axis: &SpeedAxis, ordinal: i64) -> anyhow::Result<(i64, i64)> {
    anyhow::ensure!(
        ordinal > 0,
        "fan speed ordinal must be positive, got {ordinal}"
    );
    let ordinal = usize::try_from(ordinal)?;
    let (work_mode, mode_value) = axis.command_for_ordinal(ordinal).ok_or_else(|| {
        anyhow!(
            "fan speed ordinal {ordinal} is outside 1..={max}",
            max = axis.max_ordinal()
        )
    })?;

    // `humidifier_set_parameter` casts both fields to `u8` for the BLE/IoT
    // encoding. Refuse out-of-range values rather than letting the cast wrap
    // and command a mode the user never asked for.
    anyhow::ensure!(
        u8::try_from(work_mode).is_ok() && u8::try_from(mode_value).is_ok(),
        "fan speed ordinal {ordinal} maps to workMode {work_mode}/modeValue \
         {mode_value}, which does not fit the u8 command encoding"
    );

    Ok((work_mode, mode_value))
}

/// What an inbound percentage ordinal means for the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanSpeedAction {
    /// Home Assistant published `speed_range_min - 1`, i.e. the slider was
    /// dragged to zero. That is a power-off request, not an invalid speed.
    PowerOff,
    SetSpeed {
        work_mode: i64,
        mode_value: i64,
    },
}

/// Decide what an ordinal means, without needing a device or a broker.
///
/// Kept out of the MQTT handler so the 0-means-off rule is covered. Folded
/// into the handler it was invisible to the suite: deleting the branch
/// entirely left every test green while silently turning slide-to-off into an
/// error.
pub fn fan_speed_action(axis: Option<&SpeedAxis>, ordinal: i64) -> anyhow::Result<FanSpeedAction> {
    if ordinal == 0 {
        return Ok(FanSpeedAction::PowerOff);
    }
    let axis = axis.ok_or_else(|| anyhow!("device has no fan speed axis to set"))?;
    let (work_mode, mode_value) = fan_percentage_command(axis, ordinal)?;
    Ok(FanSpeedAction::SetSpeed {
        work_mode,
        mode_value,
    })
}

pub async fn mqtt_fan_set_percentage(
    Payload(ordinal): Payload<i64>,
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    log::info!("mqtt_fan_set_percentage: {id}: {ordinal}");
    let device = state.resolve_device_for_control(&id).await?;

    // `with_device` fails when there is no workMode capability at all, but an
    // on/off-only fan can still legitimately receive ordinal 0.
    let axis = ParsedWorkMode::with_device(&device)
        .ok()
        .and_then(|work_modes| work_modes.classify_fan_controls().axis);

    match fan_speed_action(axis.as_ref(), ordinal)? {
        FanSpeedAction::PowerOff => state.device_power_on(&device, false).await,
        FanSpeedAction::SetSpeed {
            work_mode,
            mode_value,
        } => {
            state
                .humidifier_set_parameter(&device, work_mode, mode_value)
                .await
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::platform_api::{
        from_json, DeviceCapability, DeviceCapabilityKind, DeviceParameters, EnumOption,
        HttpDeviceInfo,
    };
    use crate::service::state::State as ServiceState;
    use std::collections::HashMap;
    use std::sync::Arc;

    const DEVICE_ID: &str = "AA:BB:CC:DD:EE:FF:11:22";

    fn empty_state() -> StateHandle {
        Arc::new(ServiceState::new())
    }

    #[derive(serde::Deserialize)]
    struct DeviceListFixture {
        data: Vec<HttpDeviceInfo>,
    }

    /// Pull one SKU's real metadata out of the shared device-list fixture.
    fn device_info_for(sku: &str) -> HttpDeviceInfo {
        let resp: DeviceListFixture =
            from_json(include_str!("../../test-data/list_devices_issue4.json")).unwrap();
        resp.data
            .into_iter()
            .find(|device| device.sku == sku)
            .unwrap_or_else(|| panic!("{sku} is in list_devices_issue4.json"))
    }

    /// The H7124 purifier this feature was built for, captured from a live
    /// unit's Platform API metadata.
    fn h7124() -> ServiceDevice {
        let resp: DeviceListFixture =
            from_json(include_str!("../../test-data/purifier-h7124.json")).unwrap();
        let mut device = ServiceDevice::new("H7124", DEVICE_ID);
        device.http_device_info = Some(resp.data.into_iter().next().unwrap());
        device
    }

    /// A `ServiceDevice` carrying real fixture capabilities, so the entity is
    /// built from metadata Govee actually sent rather than a hand-made shape.
    fn device_from_fixture(sku: &str) -> ServiceDevice {
        let mut device = ServiceDevice::new(sku, DEVICE_ID);
        device.http_device_info = Some(device_info_for(sku));
        device
    }

    fn on_off(instance: &str) -> DeviceCapability {
        DeviceCapability {
            kind: DeviceCapabilityKind::OnOff,
            instance: instance.to_string(),
            parameters: Some(DeviceParameters::Enum {
                options: vec![
                    EnumOption {
                        name: "on".to_string(),
                        value: 1.into(),
                        extras: HashMap::new(),
                    },
                    EnumOption {
                        name: "off".to_string(),
                        value: 0.into(),
                        extras: HashMap::new(),
                    },
                ],
            }),
            alarm_type: None,
            event_state: None,
        }
    }

    /// A device with only the capabilities named -- no workMode at all. This is
    /// the upstream #713 (H1310 ceiling fan) shape.
    fn device_with_capabilities(caps: Vec<DeviceCapability>) -> ServiceDevice {
        let mut device = ServiceDevice::new("H1310", DEVICE_ID);
        let mut info = device_info_for("H7121");
        info.sku = "H1310".to_string();
        info.capabilities = caps;
        device.http_device_info = Some(info);
        device
    }

    async fn config_for(device: &ServiceDevice) -> serde_json::Value {
        let fan = Fan::new(device, &empty_state())
            .await
            .expect("fan construction must not fail")
            .expect("device yields a fan entity");
        serde_json::to_value(&fan.fan).expect("FanConfig serializes")
    }

    /// H7111's speeds live in one mode's own value range, so the fan advertises
    /// an 8-step percentage plus the remaining modes as presets.
    #[tokio::test]
    async fn range_shape_advertises_percentage_and_presets() {
        let json = config_for(&device_from_fixture("H7111")).await;

        assert_eq!(json["speed_range_min"], 1);
        assert_eq!(json["speed_range_max"], 8);
        assert_eq!(json["payload_reset_percentage"], "None");
        assert_eq!(json["payload_reset_preset_mode"], "None");
        assert_eq!(
            json["preset_modes"],
            serde_json::json!(["Auto", "Custom", "Nature", "Sleep", "Storm"])
        );
        assert!(json["percentage_command_topic"]
            .as_str()
            .unwrap()
            .ends_with("/set-percentage"));
        assert!(
            json["command_topic"].as_str().unwrap().contains("/switch/"),
            "power reuses the switch route: {json}"
        );
    }

    /// H7111 has an `oscillationToggle`, so the fan advertises oscillation --
    /// pointed at the generic switch route with ON/OFF payloads, because
    /// `mqtt_switch_command` does not parse HA's `oscillate_on` default.
    #[tokio::test]
    async fn oscillation_is_advertised_only_when_the_capability_exists() {
        let with = config_for(&device_from_fixture("H7111")).await;
        assert!(with["oscillation_command_topic"]
            .as_str()
            .unwrap()
            .contains("oscillationToggle"));
        assert_eq!(with["payload_oscillation_on"], "ON");
        assert_eq!(with["payload_oscillation_off"], "OFF");

        let without = config_for(&device_from_fixture("H7121")).await;
        assert!(
            without.get("oscillation_command_topic").is_none(),
            "H7121 has no oscillationToggle, so HA must not render the control: {without}"
        );
        assert!(without.get("payload_oscillation_on").is_none());
    }

    /// The purifier case. Its speeds are top-level work modes, so it still gets
    /// a real 3-step slider -- the outcome the whole Step 3 rule exists for.
    #[tokio::test]
    async fn purifier_top_level_modes_still_get_a_speed_slider() {
        let json = config_for(&device_from_fixture("H7121")).await;

        assert_eq!(json["speed_range_min"], 1);
        assert_eq!(json["speed_range_max"], 3, "Low, Medium, High");
        assert_eq!(
            json["preset_modes"],
            serde_json::json!(["Sleep"]),
            "Sleep=16 breaks the contiguous run, so it is a preset"
        );
    }

    /// Upstream wez/govee2mqtt#713: a power toggle and nothing else. The device
    /// must gain a working on/off fan and must NOT advertise controls it cannot
    /// honour.
    #[tokio::test]
    async fn a_device_without_work_mode_gets_an_on_off_only_fan() {
        let device = device_with_capabilities(vec![on_off("powerSwitch")]);
        let json = config_for(&device).await;

        assert!(json["command_topic"].is_string());
        assert!(json["state_topic"].is_string());
        for absent in [
            "percentage_command_topic",
            "percentage_state_topic",
            "speed_range_min",
            "speed_range_max",
            "preset_mode_command_topic",
            "preset_modes",
            "oscillation_command_topic",
        ] {
            assert!(
                json.get(absent).is_none(),
                "{absent} must be absent on an on/off-only fan: {json}"
            );
        }
    }

    /// No power switch means no `command_topic`, which HA requires. Returning
    /// `Ok(None)` keeps the device's other entities: an `Err` here would abort
    /// the whole device's enumeration and cost it the sensors it already had.
    #[tokio::test]
    async fn a_device_without_a_power_switch_yields_no_fan() {
        let device = device_with_capabilities(vec![on_off("someOtherToggle")]);
        let fan = Fan::new(&device, &empty_state())
            .await
            .expect("must not be an error");
        assert!(fan.is_none());
    }

    /// No Platform API metadata at all -- also `Ok(None)`, never `Err`.
    #[tokio::test]
    async fn a_device_without_platform_metadata_yields_no_fan() {
        let device = ServiceDevice::new("H7121", DEVICE_ID);
        assert!(Fan::new(&device, &empty_state()).await.unwrap().is_none());
    }

    async fn fan_for(sku: &str) -> Fan {
        Fan::new(&device_from_fixture(sku), &empty_state())
            .await
            .unwrap()
            .unwrap()
    }

    fn parsed_for(sku: &str) -> ParsedWorkMode {
        let info = device_info_for(sku);
        let cap = info
            .capabilities
            .iter()
            .find(|cap| cap.instance == "workMode")
            .unwrap();
        ParsedWorkMode::with_capability(cap).unwrap()
    }

    /// Row 1 of the reset table: off. Both axes reset, nothing else.
    #[tokio::test]
    async fn an_off_device_resets_both_axes() {
        let fan = fan_for("H7111").await;
        let parsed = parsed_for("H7111");

        assert_eq!(
            fan.state_publishes(false, Some((1, Some(3))), Some(&parsed)),
            vec![
                FanPublish::Power("OFF"),
                FanPublish::Percentage("None".to_string()),
                FanPublish::PresetMode("None".to_string()),
            ],
            "an off fan reports no speed and no preset, whatever workMode says"
        );
    }

    /// Row 2: on the speed axis. The preset is reset, and -- the part that
    /// matters -- the reset is emitted BEFORE the live percentage.
    #[tokio::test]
    async fn a_device_on_the_speed_axis_resets_the_preset_first() {
        let fan = fan_for("H7111").await;
        let parsed = parsed_for("H7111");

        assert_eq!(
            fan.state_publishes(true, Some((1, Some(3))), Some(&parsed)),
            vec![
                FanPublish::Power("ON"),
                FanPublish::PresetMode("None".to_string()),
                FanPublish::Percentage("3".to_string()),
            ],
            "reset must precede the live value so HA never sees both set"
        );
    }

    /// Row 3: in a preset. Mirror image -- the percentage resets first.
    #[tokio::test]
    async fn a_device_in_a_preset_resets_the_percentage_first() {
        let fan = fan_for("H7111").await;
        let parsed = parsed_for("H7111");

        // workMode 3 is "Auto" on the H7111, which is a preset not a speed.
        assert_eq!(
            fan.state_publishes(true, Some((3, None)), Some(&parsed)),
            vec![
                FanPublish::Power("ON"),
                FanPublish::Percentage("None".to_string()),
                FanPublish::PresetMode("Auto".to_string()),
            ]
        );
    }

    /// Row 4: on, but the device has told us nothing usable. Reset both rather
    /// than leaving a stale speed on screen.
    #[tokio::test]
    async fn an_unknown_work_mode_resets_both_axes() {
        let fan = fan_for("H7111").await;
        let parsed = parsed_for("H7111");

        assert_eq!(
            fan.state_publishes(true, None, Some(&parsed)),
            vec![
                FanPublish::Power("ON"),
                FanPublish::Percentage("None".to_string()),
                FanPublish::PresetMode("None".to_string()),
            ]
        );
    }

    /// A mode that resolves but is not in the advertised `preset_modes` must not
    /// be published: HA logs an error on every update for an unknown preset.
    #[tokio::test]
    async fn a_mode_outside_the_advertised_presets_is_not_published() {
        let fan = fan_for("H7121").await;
        let parsed = parsed_for("H7121");

        // H7121's Low=1 is part of the speed axis, so it resolves to a mode
        // name but is deliberately absent from preset_modes.
        let publishes = fan.state_publishes(true, Some((1, None)), Some(&parsed));
        assert!(
            !publishes
                .iter()
                .any(|p| matches!(p, FanPublish::PresetMode(name) if name == "Low")),
            "Low is a speed, not a preset: {publishes:?}"
        );
        assert!(
            publishes.contains(&FanPublish::Percentage("1".to_string())),
            "it is reported as speed 1 instead: {publishes:?}"
        );
    }

    /// The device reports a `modeValue` outside the range we advertised -- a
    /// firmware change, or a mode Govee added after discovery ran. The speed
    /// lookup fails, so preset resolution runs and finds the AXIS mode's own
    /// name ("FanSpeed"), which is deliberately not an advertised preset.
    ///
    /// Publishing it would make Home Assistant log an error on every update.
    /// Both axes reset instead: "we do not know" is the honest report.
    #[tokio::test]
    async fn an_unadvertised_mode_name_is_filtered_out_not_published() {
        let fan = fan_for("H7111").await;
        let parsed = parsed_for("H7111");

        assert!(
            !fan.fan.preset_modes.contains(&"FanSpeed".to_string()),
            "FanSpeed is the speed axis, so it is not among the presets"
        );

        assert_eq!(
            fan.state_publishes(true, Some((1, Some(99))), Some(&parsed)),
            vec![
                FanPublish::Power("ON"),
                FanPublish::Percentage("None".to_string()),
                FanPublish::PresetMode("None".to_string()),
            ],
            "modeValue 99 is outside the advertised 1..=8, and FanSpeed is not \
             a valid preset, so neither axis gets a value"
        );
    }

    /// The purifier's Sleep=16 IS an advertised preset, and selects it.
    #[tokio::test]
    async fn the_purifier_reports_sleep_as_a_preset() {
        let fan = fan_for("H7121").await;
        let parsed = parsed_for("H7121");

        assert_eq!(
            fan.state_publishes(true, Some((16, None)), Some(&parsed)),
            vec![
                FanPublish::Power("ON"),
                FanPublish::Percentage("None".to_string()),
                FanPublish::PresetMode("Sleep".to_string()),
            ]
        );
    }

    /// End-to-end for the real device: what Home Assistant is actually handed.
    ///
    /// Before this change the purifier had a switch, a 0-255 `number` for
    /// gearMode, three preset buttons and two JSON-string sensors -- verified
    /// live against the unit. It gains a `fan` entity with a three-step slider
    /// and the three real presets.
    #[tokio::test]
    async fn the_h7124_purifier_gets_a_three_step_fan_with_three_presets() {
        let json = config_for(&h7124()).await;

        assert_eq!(json["speed_range_min"], 1);
        assert_eq!(
            json["speed_range_max"], 3,
            "Low/Medium/High -- the old number entity advertised 0..255"
        );
        assert_eq!(
            json["preset_modes"],
            serde_json::json!(["Auto", "Sleep", "Turbo"])
        );
        assert_eq!(json["payload_reset_percentage"], "None");
        assert_eq!(json["payload_reset_preset_mode"], "None");
        assert!(
            json.get("oscillation_command_topic").is_none(),
            "the H7124 has no oscillationToggle: {json}"
        );
        assert!(json["percentage_command_topic"]
            .as_str()
            .unwrap()
            .ends_with("/set-percentage"));
    }

    /// The live device was in Sleep (`{workMode: 5, modeValue: 0}`) when this
    /// was written. That must read back as the Sleep preset with the speed
    /// axis explicitly cleared, not as a stale speed.
    #[tokio::test]
    async fn the_h7124_reports_its_live_sleep_state_as_a_preset() {
        let device = h7124();
        let fan = Fan::new(&device, &empty_state()).await.unwrap().unwrap();
        let parsed = ParsedWorkMode::with_device(&device).unwrap();

        assert_eq!(
            fan.state_publishes(true, Some((5, Some(0))), Some(&parsed)),
            vec![
                FanPublish::Power("ON"),
                FanPublish::Percentage("None".to_string()),
                FanPublish::PresetMode("Sleep".to_string()),
            ]
        );

        // And gearMode 2 reads back as speed 2.
        assert_eq!(
            fan.state_publishes(true, Some((1, Some(2))), Some(&parsed)),
            vec![
                FanPublish::Power("ON"),
                FanPublish::PresetMode("None".to_string()),
                FanPublish::Percentage("2".to_string()),
            ]
        );
    }

    /// Dragging the HA slider to zero publishes `speed_range_min - 1`. That is
    /// a power-off, not an invalid speed. Mutation-checked: deleting the branch
    /// used to leave every test green while breaking slide-to-off.
    #[test]
    fn ordinal_zero_is_a_power_off_not_a_speed() {
        let axis = owned_axis();
        assert_eq!(
            fan_speed_action(Some(&axis), 0).unwrap(),
            FanSpeedAction::PowerOff
        );
        // Still a power-off on a fan that has no speed axis at all, which is
        // the only control such a device has.
        assert_eq!(fan_speed_action(None, 0).unwrap(), FanSpeedAction::PowerOff);
    }

    #[test]
    fn a_real_ordinal_becomes_a_speed_command() {
        let axis = owned_axis();
        assert_eq!(
            fan_speed_action(Some(&axis), 2).unwrap(),
            FanSpeedAction::SetSpeed {
                work_mode: 1,
                mode_value: 2
            }
        );
        assert!(
            fan_speed_action(None, 2).is_err(),
            "a non-zero speed on an axis-less fan is an error, not a silent no-op"
        );
    }

    /// The wiring `state_publishes` tests cannot see: which topic each publish
    /// lands on. Swapping percentage and preset here would be invisible to
    /// every ordering test.
    #[tokio::test]
    async fn each_publish_maps_to_its_own_topic() {
        let fan = fan_for("H7111").await;

        let (topic, payload) = fan.topic_and_payload(&FanPublish::Power("ON")).unwrap();
        assert_eq!(topic, fan.fan.state_topic);
        assert_eq!(payload, "ON");

        // Bound so the borrow outlives the call.
        let percentage = FanPublish::Percentage("3".to_string());
        let (topic, payload) = fan.topic_and_payload(&percentage).unwrap();
        assert!(topic.ends_with("/notify-percentage"), "got {topic}");
        assert_eq!(payload, "3");

        let preset = FanPublish::PresetMode("Auto".to_string());
        let (topic, payload) = fan.topic_and_payload(&preset).unwrap();
        assert!(topic.ends_with("/notify-preset-mode"), "got {topic}");
        assert_eq!(payload, "Auto");
    }

    /// An on/off-only fan advertises neither axis topic, so those publishes are
    /// dropped rather than sent somewhere Home Assistant never subscribed.
    #[tokio::test]
    async fn publishes_for_unadvertised_axes_are_dropped() {
        let device = device_with_capabilities(vec![on_off("powerSwitch")]);
        let fan = Fan::new(&device, &empty_state()).await.unwrap().unwrap();

        assert!(fan.topic_and_payload(&FanPublish::Power("OFF")).is_some());
        let percentage = FanPublish::Percentage("None".to_string());
        assert!(
            fan.topic_and_payload(&percentage).is_none(),
            "no percentage topic was advertised, so nothing may be published to one"
        );
        let preset = FanPublish::PresetMode("None".to_string());
        assert!(fan.topic_and_payload(&preset).is_none());
    }

    /// Govee reports toggles as integers. A malformed reading publishes
    /// nothing rather than asserting "OFF" on no evidence.
    #[test]
    fn oscillation_readback_maps_integers_and_ignores_junk() {
        assert_eq!(oscillation_payload(&json!({"value": 1})), Some("ON"));
        assert_eq!(oscillation_payload(&json!({"value": 2})), Some("ON"));
        assert_eq!(oscillation_payload(&json!({"value": 0})), Some("OFF"));
        assert_eq!(oscillation_payload(&json!({"value": "on"})), None);
        assert_eq!(oscillation_payload(&json!({})), None);
    }

    /// A Govee mode literally named "None" would collide with
    /// `payload_reset_preset_mode`, and Home Assistant discards the ENTIRE fan
    /// payload when `preset_modes` contains it. These are raw vendor strings.
    #[tokio::test]
    async fn a_mode_named_none_is_kept_out_of_the_preset_list() {
        let mut device = device_from_fixture("H7111");
        let info = device.http_device_info.as_mut().unwrap();
        let cap = info
            .capabilities
            .iter_mut()
            .find(|cap| cap.instance == "workMode")
            .unwrap();
        if let Some(crate::platform_api::DeviceParameters::Struct { fields }) = &mut cap.parameters
        {
            for field in fields.iter_mut() {
                if let crate::platform_api::DeviceParameters::Enum { options } =
                    &mut field.field_type
                {
                    for option in options.iter_mut() {
                        if option.name == "Auto" {
                            option.name = "None".to_string();
                        }
                    }
                }
            }
        }

        let json = config_for(&device).await;
        let presets = json["preset_modes"].as_array().unwrap();
        assert!(
            !presets.iter().any(|p| p == "None"),
            "a preset named None would make HA drop the whole payload: {presets:?}"
        );
    }

    fn owned_axis() -> SpeedAxis {
        SpeedAxis {
            owner: Some(1),
            steps: vec![(1, 1), (1, 2), (1, 3)],
        }
    }

    #[test]
    fn percentage_ordinals_map_to_work_mode_commands() {
        let axis = owned_axis();
        assert_eq!(fan_percentage_command(&axis, 1).unwrap(), (1, 1));
        assert_eq!(fan_percentage_command(&axis, 3).unwrap(), (1, 3));
    }

    /// 0 is HA's "off" and is handled by the caller as a power-off, so it must
    /// never reach this mapping as a speed. Negative and over-range are hard
    /// errors rather than being clamped into a speed the user did not pick.
    #[test]
    fn out_of_range_ordinals_are_rejected_not_clamped() {
        let axis = owned_axis();
        for bad in [0, -1, 4, i64::MAX] {
            assert!(
                fan_percentage_command(&axis, bad).is_err(),
                "ordinal {bad} must be rejected"
            );
        }
    }

    /// `humidifier_set_parameter` casts both fields to `u8`. A value past 255
    /// would wrap and command an unrelated mode, so it is refused here instead.
    #[test]
    fn commands_that_would_truncate_to_u8_are_refused() {
        let axis = SpeedAxis {
            owner: Some(1),
            steps: vec![(1, 300)],
        };
        let err = fan_percentage_command(&axis, 1).unwrap_err().to_string();
        assert!(
            err.contains("u8"),
            "the error must name the encoding limit: {err}"
        );
    }
}
