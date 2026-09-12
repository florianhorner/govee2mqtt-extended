use crate::commands::serve::POLL_INTERVAL;
use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::humidifier::DEVICE_CLASS_HUMIDITY;
use crate::hass_mqtt::instance::{publish_entity_config, EntityInstance};
use crate::platform_api::DeviceCapability;
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{availability_topic, topic_safe_id, topic_safe_string, HassClient};
use crate::service::quirks::{HumidityUnits, Quirk};
use crate::service::state::StateHandle;
use crate::temperature::{TemperatureUnits, TemperatureValue, DEVICE_CLASS_TEMPERATURE};
use async_trait::async_trait;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value as JsonValue};

#[derive(Serialize, Clone, Debug)]
pub struct SensorConfig {
    #[serde(flatten)]
    pub base: EntityConfig,

    pub state_topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_class: Option<StateClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_of_measurement: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_attributes_topic: Option<String>,
}

#[allow(unused)]
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateClass {
    #[serde(rename = "measurement")]
    Measurement,
    #[serde(rename = "total")]
    Total,
    #[serde(rename = "total_increasing")]
    TotalIncreasing,
}

impl SensorConfig {
    pub async fn publish(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("sensor", state, client, &self.base, self).await
    }

    pub async fn notify_state(&self, client: &HassClient, value: &str) -> anyhow::Result<()> {
        client.publish(&self.state_topic, value).await
    }
}

#[derive(Clone)]
pub struct GlobalFixedDiagnostic {
    sensor: SensorConfig,
    value: String,
}

#[async_trait]
impl EntityInstance for GlobalFixedDiagnostic {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.sensor.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        self.sensor.notify_state(client, &self.value).await
    }
}

impl GlobalFixedDiagnostic {
    pub fn new<NAME: Into<String>, VALUE: Into<String>>(name: NAME, value: VALUE) -> Self {
        let name = name.into();
        let unique_id = format!("global-{}", topic_safe_string(&name));

        Self {
            sensor: SensorConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some(name),
                    entity_category: Some("diagnostic".to_string()),
                    origin: Origin::default(),
                    device: Device::this_service(),
                    unique_id: unique_id.clone(),
                    device_class: None,
                    icon: None,
                },
                state_topic: format!("gv2mqtt/sensor/{unique_id}/state"),
                state_class: None,
                unit_of_measurement: None,
                json_attributes_topic: None,
            },
            value: value.into(),
        }
    }
}

/// Resolve the exact string a capability publishes to Home Assistant.
///
/// Lifted out of `CapabilitySensor::notify_state` so the *dispatch* is
/// testable, not just its pieces. Testing `scalar_state_value` alone proves
/// the helper works while saying nothing about whether anything calls it --
/// reverting the default arm to `cap_state.to_string()` left a suite of
/// helper-only tests fully green, which is the fake-coverage shape this
/// project has been bitten by before.
///
/// `target_scale` is resolved by the caller because reading it is async and
/// this needs to stay a plain function.
fn capability_state_value(
    instance_name: &str,
    cap_state: &JsonValue,
    quirk: Option<&Quirk>,
    target_scale: TemperatureUnits,
) -> String {
    match instance_name {
        "sensorTemperature" => {
            let units = quirk
                .and_then(|q| q.platform_temperature_sensor_units)
                .unwrap_or(TemperatureUnits::Fahrenheit);

            match cap_state
                .pointer("/value")
                .and_then(|v| v.as_f64())
                .map(|v| TemperatureValue::new(v, units))
            {
                Some(v) => {
                    let value = v.as_unit(target_scale).value();
                    format!("{value:.2}")
                }
                None => "".to_string(),
            }
        }
        "sensorHumidity" => {
            let units = quirk
                .and_then(|q| q.platform_humidity_sensor_units)
                .unwrap_or(HumidityUnits::RelativePercent);
            match cap_state
                .pointer("/value")
                .and_then(|v| v.as_f64())
                .map(|v| units.from_reading_to_relative_percent(v))
            {
                Some(v) => format!("{v:.2}"),
                None => "".to_string(),
            }
        }
        _ => scalar_state_value(cap_state),
    }
}

/// Publish the scalar a capability carries, not the JSON wrapper around it.
///
/// Govee's Platform API reports property state as `{"value": 6}`. Publishing
/// that verbatim gives Home Assistant the literal string `{"value":6}` for an
/// entity it expects to be a number, which is why `airQuality` and
/// `filterLifeTime` have been unusable without a hand-written `value_template`
/// (upstream wez/govee2mqtt#369, #510, #667).
///
/// Anything that is not a scalar under `/value` -- a nested `workMode` struct,
/// a null, or a shape we do not recognise -- falls back to the whole object, so
/// an unfamiliar capability still shows *something* rather than going blank.
fn scalar_state_value(state: &JsonValue) -> String {
    match state.pointer("/value") {
        Some(JsonValue::String(s)) => s.clone(),
        Some(v @ JsonValue::Number(_)) | Some(v @ JsonValue::Bool(_)) => v.to_string(),
        _ => state.to_string(),
    }
}

#[derive(Clone)]
pub struct CapabilitySensor {
    sensor: SensorConfig,
    device_id: String,
    state: StateHandle,
    instance_name: String,
}

impl CapabilitySensor {
    pub async fn new(
        device: &ServiceDevice,
        state: &StateHandle,
        instance: &DeviceCapability,
    ) -> anyhow::Result<Self> {
        let unique_id = format!(
            "sensor-{id}-{inst}",
            id = topic_safe_id(device),
            inst = topic_safe_string(&instance.instance)
        );

        let unit_of_measurement = match instance.instance.as_str() {
            "sensorTemperature" => Some(state.get_temperature_scale().await.unit_of_measurement()),
            "sensorHumidity" => Some("%"),
            // Govee reports remaining filter life as a percentage.
            "filterLifeTime" => Some("%"),
            _ => None,
        };

        let device_class = match instance.instance.as_str() {
            "sensorTemperature" => Some(DEVICE_CLASS_TEMPERATURE),
            "sensorHumidity" => Some(DEVICE_CLASS_HUMIDITY),
            // Deliberately no `aqi` device class for airQuality: Govee reports a
            // vendor-specific index, and nobody has confirmed how it maps onto a
            // standard AQI scale (upstream wez/govee2mqtt#369). Claiming the class
            // would assert a mapping we have not verified.
            _ => None,
        };

        let state_class = match instance.instance.as_str() {
            "sensorTemperature" | "sensorHumidity" | "airQuality" | "filterLifeTime" => {
                Some(StateClass::Measurement)
            }
            _ => None,
        };

        let name = match instance.instance.as_str() {
            "sensorTemperature" => "Temperature".to_string(),
            "sensorHumidity" => "Humidity".to_string(),
            "online" => "Connected to Govee Cloud".to_string(),
            "airQuality" => "Air Quality".to_string(),
            "filterLifeTime" => "Filter Life".to_string(),
            _ => instance.instance.to_string(),
        };

        let icon = match instance.instance.as_str() {
            "airQuality" => Some("mdi:air-purifier".to_string()),
            "filterLifeTime" => Some("mdi:air-filter".to_string()),
            _ => None,
        };

        // Air quality is the reading a purifier owner actually looks at, so it
        // belongs on the device card rather than buried under Diagnostics.
        // Everything else here stays diagnostic -- including filter life, which
        // is consumable wear and is conventionally filed that way in HA.
        let entity_category = match instance.instance.as_str() {
            "airQuality" => None,
            _ => Some("diagnostic".to_string()),
        };

        Ok(Self {
            sensor: SensorConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some(name),
                    entity_category,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: unique_id.clone(),
                    device_class,
                    icon,
                },
                state_topic: format!("gv2mqtt/sensor/{unique_id}/state"),
                state_class,
                unit_of_measurement,
                json_attributes_topic: None,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
            instance_name: instance.instance.to_string(),
        })
    }
}

#[async_trait]
impl EntityInstance for CapabilitySensor {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.sensor.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) = self.state.device_by_id(&self.device_id).await else {
            log::warn!(
                "Device {} not found in state, skipping notify",
                self.device_id
            );
            return Ok(());
        };

        let quirk = device.resolve_quirk();

        if let Some(cap) = device.get_state_capability_by_instance(&self.instance_name) {
            let value = capability_state_value(
                &self.instance_name,
                &cap.state,
                quirk.as_ref(),
                self.state.get_temperature_scale().await.into(),
            );

            return self.sensor.notify_state(client, &value).await;
        }
        log::trace!(
            "CapabilitySensor::notify_state: didn't find state for {device} {instance}",
            instance = self.instance_name
        );
        Ok(())
    }
}

pub struct DeviceStatusDiagnostic {
    sensor: SensorConfig,
    device_id: String,
    state: StateHandle,
}

impl DeviceStatusDiagnostic {
    pub fn new(device: &ServiceDevice, state: &StateHandle) -> Self {
        let unique_id = format!("sensor-{id}-gv2mqtt-status", id = topic_safe_id(device),);

        Self {
            sensor: SensorConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some("Status".to_string()),
                    entity_category: Some("diagnostic".to_string()),
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: unique_id.clone(),
                    device_class: None,
                    icon: None,
                },
                state_topic: format!("gv2mqtt/sensor/{unique_id}/state"),
                state_class: None,
                json_attributes_topic: Some(format!("gv2mqtt/sensor/{unique_id}/attributes")),
                unit_of_measurement: None,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }
    }
}

#[async_trait]
impl EntityInstance for DeviceStatusDiagnostic {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.sensor.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) = self.state.device_by_id(&self.device_id).await else {
            log::warn!(
                "Device {} not found in state, skipping notify",
                self.device_id
            );
            return Ok(());
        };

        let iot_state = device.compute_iot_device_state();
        let lan_state = device.compute_lan_device_state();
        let http_state = device.compute_http_device_state();
        let platform_metadata = &device.http_device_info;
        let platform_state = &device.http_device_state;
        let device_state = device.device_state();

        let now = Utc::now();

        let threshold = *POLL_INTERVAL + chrono::Duration::seconds(30);

        let summary = match &device_state {
            Some(state) => {
                if now - state.updated > threshold {
                    "Missing".to_string()
                } else {
                    "Available".to_string()
                }
            }
            None => "Unknown".to_string(),
        };

        let attributes = json!({
            "iot": iot_state,
            "lan": lan_state,
            "http": http_state,
            "platform_metadata": platform_metadata,
            "platform_state": platform_state,
            "overall": device_state,
        });

        self.sensor.notify_state(client, &summary).await?;
        if let Some(topic) = &self.sensor.json_attributes_topic {
            client.publish_obj(topic, attributes).await?;
        }
        Ok(())
    }
}

pub struct SceneInfoSensor {
    sensor: SensorConfig,
    device_id: String,
    device_topic_id: String,
    state: StateHandle,
}

impl SceneInfoSensor {
    pub fn new(device: &ServiceDevice, state: &StateHandle) -> Self {
        let unique_id = format!("sensor-{id}-gv2mqtt-scene-info", id = topic_safe_id(device));

        Self {
            device_topic_id: topic_safe_id(device),
            sensor: SensorConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some("Scene Info".to_string()),
                    entity_category: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: unique_id.clone(),
                    device_class: None,
                    icon: Some("mdi:palette".to_string()),
                },
                state_topic: format!("gv2mqtt/sensor/{unique_id}/state"),
                state_class: None,
                json_attributes_topic: Some(format!("gv2mqtt/sensor/{unique_id}/attributes")),
                unit_of_measurement: None,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }
    }
}

#[async_trait]
impl EntityInstance for SceneInfoSensor {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.sensor.publish(state, client).await?;

        // Publish scene catalog as retained MQTT message during registration (once),
        // not on every state change. HA automations can subscribe to this topic.
        if let Some(device) = self.state.device_by_id(&self.device_id).await {
            let catalog = self
                .state
                .device_list_scenes_categorized(&device)
                .await
                .unwrap_or_default();
            if !catalog.is_empty() {
                let catalog_topic = format!("gv2mqtt/{}/scene-catalog", self.device_topic_id);
                if let Err(err) = client.publish_obj_retained(&catalog_topic, &catalog).await {
                    log::warn!("Failed to publish scene catalog: {err:#}");
                }
            }
        }

        Ok(())
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) = self.state.device_by_id(&self.device_id).await else {
            return Ok(());
        };

        let active = device.active_scene_name();
        let scene_name = active.unwrap_or("None").to_string();

        let catalog = self
            .state
            .device_list_scenes_categorized(&device)
            .await
            .unwrap_or_default();

        // Build flat ordered list (entry + category) for index + thumbnail/hint lookup
        let flat: Vec<_> = catalog
            .iter()
            .flat_map(|cat| cat.scenes.iter().map(move |s| (s, cat.name.as_str())))
            .collect();

        // Only resolve an index when a scene is actually active; never match the
        // "None" sentinel against the catalog (a scene literally named "None" must
        // not look active when nothing is playing).
        let current_idx = active.and_then(|name| {
            flat.iter()
                .position(|(s, _)| s.name.eq_ignore_ascii_case(name))
        });

        // Keep `scene_name`, `category`, `index`, `total`, `next_scene`, `prev_scene`
        // exactly as before — existing HA automations, templates, and the user's
        // dashboard consume them. The new fields further down are strictly additive.
        let (category, index, next_scene, prev_scene) = if let Some(idx) = current_idx {
            let total = flat.len();
            let next_idx = (idx + 1) % total;
            let prev_idx = if idx == 0 { total - 1 } else { idx - 1 };
            (
                flat[idx].1.to_string(),
                idx,
                flat[next_idx].0.name.clone(),
                flat[prev_idx].0.name.clone(),
            )
        } else {
            let next = flat
                .first()
                .map(|(s, _)| s.name.clone())
                .unwrap_or_default();
            let prev = flat.last().map(|(s, _)| s.name.clone()).unwrap_or_default();
            ("Unknown".to_string(), 0, next, prev)
        };

        // Additive, render-ready fields for the v2 Scene Deck card. We publish data,
        // not UI strings — the card composes its display copy from `has_active_scene`,
        // `scene_name`, and `is_on`.
        let has_active_scene = active.is_some();
        let thumbnail = current_idx
            .and_then(|idx| flat[idx].0.icon_urls.first().cloned())
            .unwrap_or_default();
        let hint = current_idx
            .and_then(|idx| flat[idx].0.hint.clone())
            .unwrap_or_default();
        // Mirror the light entity's on/off semantics (see light.rs) so the card and the
        // light entity never disagree: an absent `light_on` (or no device state yet)
        // means "light off". Always a bool so consumers never see JSON null.
        let is_on = device
            .device_state()
            .and_then(|s| s.light_on)
            .unwrap_or(false);

        let attributes = json!({
            // existing contract — unchanged
            "scene_name": scene_name,
            "category": category,
            "index": index,
            "total": flat.len(),
            "next_scene": next_scene,
            "prev_scene": prev_scene,
            // additive
            "has_active_scene": has_active_scene,
            "thumbnail": thumbnail,
            "hint": hint,
            "is_on": is_on,
        });

        self.sensor.notify_state(client, &scene_name).await?;
        if let Some(topic) = &self.sensor.json_attributes_topic {
            client.publish_obj(topic, attributes).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::platform_api::DeviceCapabilityKind;
    use crate::service::state::State as ServiceState;
    use std::sync::Arc;

    const DEVICE_ID: &str = "AA:BB:CC:DD:EE:FF:11:22";
    const SKU: &str = "H7121";

    fn test_device() -> ServiceDevice {
        ServiceDevice::new(SKU, DEVICE_ID)
    }

    fn empty_state() -> StateHandle {
        Arc::new(ServiceState::new())
    }

    fn property(instance: &str) -> DeviceCapability {
        DeviceCapability {
            kind: DeviceCapabilityKind::Property,
            instance: instance.to_string(),
            parameters: None,
            alarm_type: None,
            event_state: None,
        }
    }

    async fn discovery_payload_for(instance: &str) -> serde_json::Value {
        let entity = CapabilitySensor::new(&test_device(), &empty_state(), &property(instance))
            .await
            .expect("capability sensor is constructible");
        serde_json::to_value(&entity.sensor).expect("SensorConfig serializes")
    }

    /// Drives the real dispatch, not just the helper. Mutation-checked:
    /// reverting the default arm to `cap_state.to_string()` turns this red,
    /// which the helper-only tests below do NOT do.
    #[test]
    fn notify_dispatch_publishes_scalars_for_purifier_properties() {
        let scale = TemperatureUnits::Celsius;

        assert_eq!(
            capability_state_value("airQuality", &json!({"value": 6}), None, scale),
            "6"
        );
        assert_eq!(
            capability_state_value("filterLifeTime", &json!({"value": 78}), None, scale),
            "78"
        );
        assert_eq!(
            capability_state_value("online", &json!({"value": true}), None, scale),
            "true"
        );
        assert_eq!(
            capability_state_value("somethingNovel", &json!({"value": 3}), None, scale),
            "3"
        );
    }

    /// The two instances with bespoke arms keep their own formatting -- the
    /// extraction must not have folded them into the scalar path, which would
    /// publish a bare "21.5" and skip the unit conversion entirely.
    ///
    /// With no quirk the SOURCE units default to Fahrenheit (see the
    /// `unwrap_or` in `capability_state_value`), so 21.5 is 21.5 F. Asserting
    /// both directions pins that default: it is the reason a device without a
    /// `platform_temperature_sensor_units` quirk can report nonsense if Govee
    /// actually sent Celsius.
    #[test]
    fn notify_dispatch_keeps_temperature_and_humidity_formatting() {
        // 21.5 F -> F is the identity, and still gets 2-decimal formatting.
        assert_eq!(
            capability_state_value(
                "sensorTemperature",
                &json!({"value": 21.5}),
                None,
                TemperatureUnits::Fahrenheit
            ),
            "21.50"
        );
        // 21.5 F -> C actually converts: (21.5 - 32) * 5 / 9 = -5.833...
        assert_eq!(
            capability_state_value(
                "sensorTemperature",
                &json!({"value": 21.5}),
                None,
                TemperatureUnits::Celsius
            ),
            "-5.83"
        );
        // Humidity has no scale conversion, but keeps the same 2-decimal shape.
        assert_eq!(
            capability_state_value(
                "sensorHumidity",
                &json!({"value": 44.0}),
                None,
                TemperatureUnits::Celsius
            ),
            "44.00"
        );
    }

    /// A missing reading yields an empty string on the bespoke arms, but the
    /// scalar path falls back to the object. Pinning both so the extraction
    /// cannot quietly unify them.
    #[test]
    fn notify_dispatch_handles_absent_readings_per_arm() {
        let scale = TemperatureUnits::Celsius;
        assert_eq!(
            capability_state_value("sensorTemperature", &json!({}), None, scale),
            ""
        );
        assert_eq!(
            capability_state_value("sensorHumidity", &json!({}), None, scale),
            ""
        );
        assert_eq!(
            capability_state_value("airQuality", &json!({}), None, scale),
            "{}"
        );
    }

    /// The whole point of the change: Govee wraps property readings in
    /// `{"value": N}`, and Home Assistant wants the bare `N`.
    #[test]
    fn scalar_value_is_unwrapped_from_the_govee_envelope() {
        assert_eq!(scalar_state_value(&json!({"value": 6})), "6");
        assert_eq!(scalar_state_value(&json!({"value": 100})), "100");
        assert_eq!(scalar_state_value(&json!({"value": 78.5})), "78.5");
        assert_eq!(scalar_state_value(&json!({"value": false})), "false");
        assert_eq!(scalar_state_value(&json!({"value": "Auto"})), "Auto");
    }

    /// A string comes back unquoted. `Value::to_string()` on a JSON string
    /// keeps the quotes, which would surface in HA as `"Auto"` rather than
    /// `Auto`, so the String arm cannot be folded into the numeric one.
    #[test]
    fn string_values_are_published_without_json_quotes() {
        let published = scalar_state_value(&json!({"value": "Auto"}));
        assert!(
            !published.contains('"'),
            "string state must not carry JSON quotes: {published}"
        );
    }

    /// Anything we do not recognise still shows *something*. A nested struct
    /// (`workMode` is the real case) has no scalar at `/value`, so the whole
    /// object is published rather than a blank sensor.
    #[test]
    fn non_scalar_and_missing_values_fall_back_to_the_whole_object() {
        let nested = json!({"value": {"workMode": 3, "modeValue": 9}});
        assert_eq!(scalar_state_value(&nested), nested.to_string());

        let no_value_key = json!({"other": 1});
        assert_eq!(scalar_state_value(&no_value_key), no_value_key.to_string());

        let null_value = json!({ "value": null });
        assert_eq!(scalar_state_value(&null_value), null_value.to_string());

        let not_an_object = json!(5);
        assert_eq!(scalar_state_value(&not_an_object), "5");
    }

    /// Air quality is what a purifier owner opens the app to look at, so it
    /// gets the device card. `entity_category` must be ABSENT, not empty --
    /// HA files any diagnostic-categorised entity away from the main card.
    #[tokio::test]
    async fn air_quality_is_a_primary_measurement() {
        let json = discovery_payload_for("airQuality").await;

        assert!(
            json.get("entity_category").is_none(),
            "airQuality belongs on the device card, not under Diagnostics: {json}"
        );
        assert_eq!(json["name"], "Air Quality");
        assert_eq!(json["state_class"], "measurement");
        assert_eq!(json["icon"], "mdi:air-purifier");
        assert!(
            json.get("unit_of_measurement").is_none(),
            "Govee's air-quality index has no confirmed unit; claiming one would \
             assert a mapping we have not verified: {json}"
        );
        assert!(
            json.get("device_class").is_none(),
            "no `aqi` device class until the Govee index is mapped to a real AQI scale: {json}"
        );
    }

    /// Filter life is consumable wear. HA convention files that as a
    /// diagnostic, so unlike air quality it deliberately stays off the card.
    #[tokio::test]
    async fn filter_life_is_a_diagnostic_percentage() {
        let json = discovery_payload_for("filterLifeTime").await;

        assert_eq!(json["entity_category"], "diagnostic");
        assert_eq!(json["name"], "Filter Life");
        assert_eq!(json["unit_of_measurement"], "%");
        assert_eq!(json["state_class"], "measurement");
        assert_eq!(json["icon"], "mdi:air-filter");
    }

    /// Regression guard: the new arms must not reclassify every other
    /// `Property` capability the bridge already exposes.
    #[tokio::test]
    async fn unrecognised_properties_keep_their_diagnostic_defaults() {
        let json = discovery_payload_for("somethingNovel").await;

        assert_eq!(json["entity_category"], "diagnostic");
        assert_eq!(json["name"], "somethingNovel");
        assert!(json.get("state_class").is_none(), "{json}");
        assert!(json.get("unit_of_measurement").is_none(), "{json}");
        assert!(json.get("icon").is_none(), "{json}");
        assert!(json.get("device_class").is_none(), "{json}");
    }

    /// Regression guard for the two instances that already had bespoke
    /// handling before this change.
    #[tokio::test]
    async fn temperature_and_humidity_metadata_is_unchanged() {
        let humidity = discovery_payload_for("sensorHumidity").await;
        assert_eq!(humidity["entity_category"], "diagnostic");
        assert_eq!(humidity["name"], "Humidity");
        assert_eq!(humidity["unit_of_measurement"], "%");
        assert_eq!(humidity["state_class"], "measurement");
        assert_eq!(humidity["device_class"], DEVICE_CLASS_HUMIDITY);

        let temperature = discovery_payload_for("sensorTemperature").await;
        assert_eq!(temperature["entity_category"], "diagnostic");
        assert_eq!(temperature["name"], "Temperature");
        assert_eq!(temperature["state_class"], "measurement");
        assert_eq!(temperature["device_class"], DEVICE_CLASS_TEMPERATURE);
    }
}
