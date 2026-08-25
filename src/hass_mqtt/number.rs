use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::instance::{publish_entity_config, EntityInstance};
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{
    availability_topic, topic_safe_id, topic_safe_string, HassClient, IdParameter,
};
use crate::service::state::StateHandle;
use anyhow::anyhow;
use async_trait::async_trait;
use mosquitto_rs::router::{Params, Payload, State};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::ops::Range;

pub const MUSIC_SENSITIVITY_COMMAND_ROUTE: &str = "gv2mqtt/:id/set-music-sensitivity";
const MUSIC_SENSITIVITY_RESET_PAYLOAD: &str = "None";

#[derive(Serialize, Clone, Debug)]
pub struct NumberConfig {
    #[serde(flatten)]
    pub base: EntityConfig,

    pub command_topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_reset: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f32>,
    pub step: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_of_measurement: Option<&'static str>,
}

impl NumberConfig {
    pub async fn publish(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("number", state, client, &self.base, self).await
    }

    pub async fn notify_state(&self, client: &HassClient, value: &str) -> anyhow::Result<()> {
        client
            .publish(
                self.state_topic
                    .as_deref()
                    .ok_or_else(|| anyhow!("number has no state_topic"))?,
                value,
            )
            .await
    }
}

pub struct WorkModeNumber {
    number: NumberConfig,
    device_id: String,
    state: StateHandle,
    mode_name: String,
    work_mode: JsonValue,
}

impl WorkModeNumber {
    pub fn new(
        device: &ServiceDevice,
        state: &StateHandle,
        label: String,
        mode_name: &str,
        work_mode: JsonValue,
        range: Option<Range<i64>>,
    ) -> Self {
        let command_topic = format!(
            "gv2mqtt/number/{id}/command/{mode}/{mode_num}",
            id = topic_safe_id(device),
            mode = topic_safe_string(mode_name),
            mode_num = work_mode
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "work-mode-was-not-int".to_string()),
        );
        let state_topic = format!(
            "gv2mqtt/number/{id}/state/{mode}",
            id = topic_safe_id(device),
            mode = topic_safe_string(mode_name)
        );

        let availability_topic = availability_topic();
        let unique_id = format!(
            "gv2mqtt-{id}-{mode}-number",
            id = topic_safe_id(device),
            mode = topic_safe_string(mode_name),
        );

        Self {
            number: NumberConfig {
                base: EntityConfig {
                    availability_topic,
                    name: Some(label),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id,
                    entity_category: None,
                    icon: None,
                },
                command_topic,
                state_topic: Some(state_topic),
                payload_reset: None,
                min: range.as_ref().map(|r| r.start as f32).or(Some(0.)),
                max: range
                    .as_ref()
                    .map(|r| r.end.saturating_sub(1) as f32)
                    .or(Some(255.)),
                step: 1f32,
                unit_of_measurement: None,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
            mode_name: mode_name.to_string(),
            work_mode,
        }
    }
}

#[async_trait]
impl EntityInstance for WorkModeNumber {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.number.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let state_topic = self
            .number
            .state_topic
            .as_ref()
            .ok_or_else(|| anyhow!("state_topic is None!?"))?;

        let Some(device) = self.state.device_by_id(&self.device_id).await else {
            log::warn!(
                "Device {} not found in state, skipping notify",
                self.device_id
            );
            return Ok(());
        };

        if let Some(cap) = device.get_state_capability_by_instance("workMode") {
            if let Some(work_mode) = cap.state.pointer("/value/workMode") {
                if *work_mode == self.work_mode {
                    // The current mode matches us, so it is valid to
                    // read the current parameter for that mode

                    if let Some(value) = cap.state.pointer("/value/modeValue") {
                        if let Some(n) = value.as_i64() {
                            client.publish(state_topic, n.to_string()).await?;
                            return Ok(());
                        }
                    }
                }
            }
        }

        if let Some(work_mode) = self.work_mode.as_i64() {
            // FIXME: assuming humidifier, rename that field?
            if let Some(n) = device.humidifier_param_by_mode.get(&(work_mode as u8)) {
                client.publish(state_topic, n.to_string()).await?;
                return Ok(());
            }
        }

        // We might get some data to report later, so this is just debug for now
        log::debug!(
            "Don't know how to report state for {} workMode {} value",
            self.device_id,
            self.mode_name
        );

        Ok(())
    }
}

#[derive(Deserialize)]
pub struct IdAndModeName {
    id: String,
    mode_name: String,
    work_mode: String,
}

pub async fn mqtt_number_command(
    Payload(value): Payload<i64>,
    Params(IdAndModeName {
        id,
        mode_name,
        work_mode,
    }): Params<IdAndModeName>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    log::info!("{mode_name} for {id}: {value}");
    let work_mode: i64 = work_mode.parse()?;
    let device = state.resolve_device_for_control(&id).await?;

    state
        .humidifier_set_parameter(&device, work_mode, value)
        .await?;

    Ok(())
}

/// Sensitivity used the next time a `Music:` effect is selected.
///
/// Reports a stored preference rather than device state, because Govee exposes
/// no readback: `GET /user/devices` answers `""` for `musicMode` on lights, and
/// the `aa 05 13` BLE triple only tracks BLE/LAN writes. Writing the slider
/// therefore sends nothing to the device; see `Device::set_music_sensitivity`.
pub struct MusicSensitivityNumber {
    number: NumberConfig,
    device_id: String,
    state: StateHandle,
}

fn music_sensitivity_state_topic(device: &ServiceDevice) -> String {
    format!("gv2mqtt/{}/notify-music-sensitivity", topic_safe_id(device))
}

#[async_trait]
trait MusicSensitivityPublisher: Sync {
    async fn publish_music_sensitivity(&self, topic: &str, value: &str) -> anyhow::Result<()>;
}

#[async_trait]
impl MusicSensitivityPublisher for HassClient {
    async fn publish_music_sensitivity(&self, topic: &str, value: &str) -> anyhow::Result<()> {
        self.publish(topic, value).await
    }
}

async fn publish_music_sensitivity_value<P: MusicSensitivityPublisher>(
    publisher: &P,
    topic: &str,
    value: u8,
) -> anyhow::Result<()> {
    publisher
        .publish_music_sensitivity(topic, &value.to_string())
        .await
}

impl MusicSensitivityNumber {
    pub fn new(device: &ServiceDevice, state: &StateHandle) -> Self {
        let id = topic_safe_id(device);
        // Built once: the discovery payload tells HA which topic to subscribe
        // to, and `notify_value` publishes through that same config field.
        let state_topic = music_sensitivity_state_topic(device);
        Self {
            number: NumberConfig {
                base: EntityConfig {
                    availability_topic: availability_topic(),
                    name: Some("Music Sensitivity".to_string()),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id: format!("gv2mqtt-{id}-music-sensitivity"),
                    entity_category: Some("config".to_string()),
                    icon: Some("mdi:music-note".to_string()),
                },
                command_topic: MUSIC_SENSITIVITY_COMMAND_ROUTE.replacen(":id", &id, 1),
                state_topic: Some(state_topic),
                payload_reset: Some(MUSIC_SENSITIVITY_RESET_PAYLOAD),
                min: Some(0.),
                max: Some(100.),
                step: 1f32,
                unit_of_measurement: Some("%"),
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }
    }

    async fn notify_value<P: MusicSensitivityPublisher>(
        &self,
        publisher: &P,
        value: u8,
    ) -> anyhow::Result<()> {
        publish_music_sensitivity_value(
            publisher,
            self.number
                .state_topic
                .as_deref()
                .ok_or_else(|| anyhow!("number has no state_topic"))?,
            value,
        )
        .await
    }

    async fn notify_state_with<P: MusicSensitivityPublisher>(
        &self,
        publisher: &P,
    ) -> anyhow::Result<()> {
        let Some(value) = self
            .state
            .device_music_sensitivity_value(&self.device_id)
            .await
        else {
            log::warn!(
                "Device {} not found in state, skipping notify",
                self.device_id
            );
            return Ok(());
        };

        match value {
            Some(value) => self.notify_value(publisher, value).await?,
            None => {
                publisher
                    .publish_music_sensitivity(
                        self.number
                            .state_topic
                            .as_deref()
                            .ok_or_else(|| anyhow!("number has no state_topic"))?,
                        MUSIC_SENSITIVITY_RESET_PAYLOAD,
                    )
                    .await?;
            }
        }

        Ok(())
    }
}

#[async_trait]
impl EntityInstance for MusicSensitivityNumber {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.number.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        self.notify_state_with(client).await
    }
}

fn parse_music_sensitivity(value: &str) -> anyhow::Result<u8> {
    let parsed = value
        .trim()
        .parse::<f64>()
        .map_err(|_| anyhow::anyhow!("music sensitivity must be a number, got {value:?}"))?;
    anyhow::ensure!(
        parsed.is_finite(),
        "music sensitivity must be a finite number, got {value:?}"
    );
    Ok(parsed.round().clamp(0.0, 100.0) as u8)
}

pub async fn mqtt_music_sensitivity_command(
    // `Payload<String>` + explicit parse, like `mqtt_set_temperature`: HA sends
    // integers, but a hand-published "55.0" would fail `Payload<i64>` before the
    // handler ever runs, losing the chance to say why.
    Payload(value): Payload<String>,
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    // Read-only resolver on purpose: this stores a preference and sends
    // nothing. Taking the control coordinator would hold the per-device permit
    // and schedule a `poll_after_control` Platform API request 5s after every
    // slider move.
    let device = state.resolve_device_read_only(&id).await?;
    let clamped = parse_music_sensitivity(&value)?;
    // Share the device's control semaphore without scheduling a device poll:
    // preferences send no device command, but still need ordering against a
    // concurrent scene selection and other slider writes.
    let _permit = state.acquire_device_update_permit(&device).await?;
    log::info!(
        "Storing music sensitivity {clamped} for {device}; \
         it applies on the next Music: effect"
    );
    {
        state
            .device_mut(&device.sku, &device.id)
            .await
            .set_music_sensitivity(clamped);
    }
    // A preference update only changes this entity. Publishing it directly
    // avoids rebuilding and notifying every entity for the device.
    if let Some(client) = state.get_hass_client().await {
        publish_music_sensitivity_value(&client, &music_sensitivity_state_topic(&device), clamped)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::service::state::State as ServiceState;
    use std::sync::Arc;

    #[derive(Default)]
    struct CapturingPublisher {
        messages: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl MusicSensitivityPublisher for CapturingPublisher {
        async fn publish_music_sensitivity(&self, topic: &str, value: &str) -> anyhow::Result<()> {
            self.messages
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push((topic.to_string(), value.to_string()));
            Ok(())
        }
    }

    struct FailingPublisher;

    #[async_trait]
    impl MusicSensitivityPublisher for FailingPublisher {
        async fn publish_music_sensitivity(
            &self,
            _topic: &str,
            _value: &str,
        ) -> anyhow::Result<()> {
            anyhow::bail!("synthetic publish failure")
        }
    }

    /// Deliberately colon-bearing: `topic_safe_id` must rewrite it, and the
    /// command topic the slider publishes to carries the rewritten form.
    const DEVICE_ID: &str = "AA:BB:CC:DD:EE:FF:11:22";
    const SKU: &str = "H607C";

    fn test_device() -> ServiceDevice {
        ServiceDevice::new(SKU, DEVICE_ID)
    }

    fn empty_state() -> StateHandle {
        Arc::new(ServiceState::new())
    }

    async fn state_with_device() -> StateHandle {
        let state = empty_state();
        {
            let _device = state.device_mut(SKU, DEVICE_ID).await;
        }
        state
    }

    async fn stored_sensitivity(state: &StateHandle) -> ServiceDevice {
        state
            .device_by_id(DEVICE_ID)
            .await
            .expect("device is in state")
    }

    /// The entity is a config-category percent slider over Govee's own 0-100
    /// range. `entity_category` keeps it out of the main device card, and the
    /// unique_id is the identity Home Assistant keys the entity registry on:
    /// changing it orphans every existing slider.
    #[test]
    fn music_sensitivity_entity_is_a_config_percent_slider() {
        let device = test_device();
        let entity = MusicSensitivityNumber::new(&device, &empty_state());
        let cfg = &entity.number;

        assert_eq!(
            cfg.base.unique_id,
            format!("gv2mqtt-{}-music-sensitivity", topic_safe_id(&device))
        );
        assert_eq!(cfg.base.name.as_deref(), Some("Music Sensitivity"));
        assert_eq!(cfg.base.entity_category.as_deref(), Some("config"));
        assert_eq!(cfg.base.icon.as_deref(), Some("mdi:music-note"));
        assert_eq!(cfg.base.device_class, None);

        assert_eq!(cfg.min, Some(0.));
        assert_eq!(cfg.max, Some(100.));
        assert_eq!(cfg.step, 1f32);
        assert_eq!(cfg.unit_of_measurement, Some("%"));

        // notify_state resolves the device from state by raw id, not topic id.
        assert_eq!(entity.device_id, DEVICE_ID);
    }

    /// The slider is inert unless `hass.rs` subscribes to exactly the topic the
    /// discovery payload advertises. Renaming one side only is silent: HA shows
    /// a working slider that writes to a topic nobody reads.
    #[test]
    fn command_and_state_topics_match_the_registered_mqtt_route() {
        let device = test_device();
        let entity = MusicSensitivityNumber::new(&device, &empty_state());
        let id = topic_safe_id(&device);

        assert_eq!(
            entity.number.command_topic,
            format!("gv2mqtt/{id}/set-music-sensitivity")
        );
        let expected_state_topic = format!("gv2mqtt/{id}/notify-music-sensitivity");
        assert_eq!(
            entity.number.state_topic.as_deref(),
            Some(expected_state_topic.as_str())
        );

        let route = entity.number.command_topic.replacen(&id, ":id", 1);
        assert_eq!(route, MUSIC_SENSITIVITY_COMMAND_ROUTE);
    }

    /// What actually reaches Home Assistant's discovery topic.
    #[test]
    fn music_sensitivity_discovery_payload_carries_the_slider_bounds() {
        let device = test_device();
        let entity = MusicSensitivityNumber::new(&device, &empty_state());
        let json = serde_json::to_value(&entity.number).unwrap();

        assert_eq!(json["min"], 0.0);
        assert_eq!(json["max"], 100.0);
        assert_eq!(json["step"], 1.0);
        assert_eq!(json["unit_of_measurement"], "%");
        assert_eq!(json["payload_reset"], MUSIC_SENSITIVITY_RESET_PAYLOAD);
        assert_eq!(json["entity_category"], "config");
        assert_eq!(json["icon"], "mdi:music-note");
        assert_eq!(json["name"], "Music Sensitivity");
        assert!(
            json["command_topic"].is_string() && json["state_topic"].is_string(),
            "both topics must survive serialization: {json}"
        );
        assert!(
            json.get("device_class").is_none(),
            "a percent preference has no HA device class: {json}"
        );
    }

    /// The HA slider write path, end to end minus the broker.
    #[tokio::test]
    async fn slider_write_stores_the_preference() {
        let state = state_with_device().await;

        // Bounded so a leaked device permit becomes a named failure instead
        // of a hung test process.
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            mqtt_music_sensitivity_command(
                Payload("55".to_string()),
                Params(IdParameter {
                    id: DEVICE_ID.to_string(),
                }),
                State(state.clone()),
            ),
        )
        .await
        .expect("the preference update must release its device locks")
        .expect("storing a preference must not fail without a hass client");

        let device = stored_sensitivity(&state).await;
        assert_eq!(device.music_sensitivity(), 55);
        assert_eq!(device.music_sensitivity_value(), Some(55));
    }

    #[tokio::test]
    async fn sensitivity_state_echo_has_the_exact_topic_and_payload() {
        let state = state_with_device().await;
        let entity = MusicSensitivityNumber::new(&test_device(), &state);
        let publisher = CapturingPublisher::default();

        entity
            .notify_state_with(&publisher)
            .await
            .expect("an unset preference must reset stale HA state");

        state
            .device_mut(SKU, DEVICE_ID)
            .await
            .set_music_sensitivity(60);
        entity
            .notify_state_with(&publisher)
            .await
            .expect("a stored preference must publish");

        assert_eq!(
            *publisher
                .messages
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            vec![
                (
                    format!(
                        "gv2mqtt/{}/notify-music-sensitivity",
                        topic_safe_id(&test_device())
                    ),
                    MUSIC_SENSITIVITY_RESET_PAYLOAD.to_string(),
                ),
                (
                    format!(
                        "gv2mqtt/{}/notify-music-sensitivity",
                        topic_safe_id(&test_device())
                    ),
                    "60".to_string(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn unset_sensitivity_reset_propagates_publish_failures() {
        let state = state_with_device().await;
        let entity = MusicSensitivityNumber::new(&test_device(), &state);

        let error = entity
            .notify_state_with(&FailingPublisher)
            .await
            .expect_err("MQTT reset failures must reach the caller");
        assert!(error.to_string().contains("synthetic publish failure"));
    }

    #[tokio::test]
    async fn sensitivity_state_echo_propagates_publish_failures() {
        let state = state_with_device().await;
        state
            .device_mut(SKU, DEVICE_ID)
            .await
            .set_music_sensitivity(60);
        let entity = MusicSensitivityNumber::new(&test_device(), &state);

        let error = entity
            .notify_state_with(&FailingPublisher)
            .await
            .expect_err("MQTT publication failures must reach the caller");
        assert!(error.to_string().contains("synthetic publish failure"));
    }

    #[tokio::test]
    async fn slider_handler_waits_for_the_device_update_permit() {
        let state = state_with_device().await;
        let device = stored_sensitivity(&state).await;
        let permit = state
            .acquire_device_update_permit(&device)
            .await
            .expect("test holds the device permit");

        let command_state = state.clone();
        let mut command = tokio::spawn(async move {
            mqtt_music_sensitivity_command(
                Payload("73".to_string()),
                Params(IdParameter {
                    id: DEVICE_ID.to_string(),
                }),
                State(command_state),
            )
            .await
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut command)
                .await
                .is_err(),
            "the real handler must wait behind an in-flight device control"
        );
        assert_eq!(
            stored_sensitivity(&state).await.music_sensitivity_value(),
            None,
            "the preference must not mutate before the permit is acquired"
        );

        drop(permit);
        tokio::time::timeout(std::time::Duration::from_secs(1), command)
            .await
            .expect("the handler proceeds after permit release")
            .expect("handler task does not panic")
            .expect("handler stores the preference");
        assert_eq!(
            stored_sensitivity(&state).await.music_sensitivity_value(),
            Some(73)
        );
    }

    /// Home Assistant publishes to `gv2mqtt/<topic_safe_id>/...`, so the `:id`
    /// the router hands the command is the topic-safe form — colons rewritten.
    /// `resolve_device_read_only` has to accept that form or every slider write
    /// fails with "device not found".
    #[tokio::test]
    async fn slider_write_resolves_the_topic_safe_id_from_the_command_topic() {
        let state = state_with_device().await;
        let topic_id = topic_safe_id(&test_device());
        assert_ne!(
            topic_id, DEVICE_ID,
            "the fixture id must differ from its topic-safe form, or this \
             test proves nothing"
        );

        mqtt_music_sensitivity_command(
            Payload("30".to_string()),
            Params(IdParameter { id: topic_id }),
            State(state.clone()),
        )
        .await
        .unwrap();

        assert_eq!(stored_sensitivity(&state).await.music_sensitivity(), 30);
    }

    /// The command topic is open to anything on the broker, so the handler
    /// clamps instead of trusting the HA min/max. The `as u8` cast after the
    /// clamp is only sound because the clamp ran first.
    #[tokio::test]
    async fn slider_write_clamps_payloads_outside_the_govee_range() {
        let state = state_with_device().await;

        for (payload, expected) in [
            (0i64, 0u8),
            (100, 100),
            (-5, 0),
            (1000, 100),
            (i64::MIN, 0),
            (i64::MAX, 100),
        ] {
            mqtt_music_sensitivity_command(
                Payload(payload.to_string()),
                Params(IdParameter {
                    id: DEVICE_ID.to_string(),
                }),
                State(state.clone()),
            )
            .await
            .unwrap();

            assert_eq!(
                stored_sensitivity(&state).await.music_sensitivity(),
                expected,
                "payload {payload} must clamp to {expected}"
            );
        }
    }

    #[test]
    fn slider_write_rounds_fractional_payloads() {
        for (payload, expected) in [
            ("55.0", 55),
            ("55.4", 55),
            ("55.5", 56),
            ("-0.6", 0),
            ("99.6", 100),
        ] {
            assert_eq!(
                parse_music_sensitivity(payload).expect("finite decimal must parse"),
                expected,
                "payload {payload}"
            );
        }
    }

    #[test]
    fn slider_write_rejects_non_finite_and_non_numeric_payloads() {
        for payload in [
            "NaN",
            "nan",
            "inf",
            "+inf",
            "-inf",
            "Infinity",
            "1e999",
            "not-a-number",
            "",
        ] {
            let err = parse_music_sensitivity(payload)
                .expect_err("non-finite and non-numeric values must be rejected");
            assert!(
                err.to_string().contains("must be"),
                "payload {payload:?}: {err:#}"
            );
        }
    }

    #[test]
    fn sensitivity_state_is_unknown_until_the_user_sets_it() {
        let mut device = test_device();

        assert_eq!(device.music_sensitivity_value(), None);
        device.set_music_sensitivity(60);
        assert_eq!(device.music_sensitivity_value(), Some(60));
    }

    /// `device_mut` creates a device for any id it is handed. The handler must
    /// resolve first, so a stray publish to an unknown id cannot conjure a
    /// phantom device into the state map (which would then be published to HA).
    #[tokio::test]
    async fn slider_write_for_an_unknown_device_errors_without_creating_one() {
        let state = empty_state();

        let err = mqtt_music_sensitivity_command(
            Payload("50".to_string()),
            Params(IdParameter {
                id: "no-such-device".to_string(),
            }),
            State(state.clone()),
        )
        .await
        .expect_err("an unknown device must not resolve");

        assert!(err.to_string().contains("not found"), "{err:#}");
        assert!(
            state.devices().await.is_empty(),
            "resolving must happen before device_mut, or an unknown id \
             conjures a device"
        );
    }
}
