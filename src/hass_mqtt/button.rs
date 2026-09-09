use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::command_routes::{
    instantiate_route, CommandTopic, MUSIC_SENSITIVITY_CLEAR_ROUTE, NUMBER_COMMAND_ROUTE,
    REQUEST_PLATFORM_DATA_ROUTE, SCENE_NEXT_ROUTE, SCENE_PREV_ROUTE, SWITCH_COMMAND_ROUTE,
};
use crate::hass_mqtt::instance::{publish_entity_config, EntityInstance};
use crate::platform_api::DeviceCapability;
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{
    availability_topic, camel_case_to_space_separated, topic_safe_id, topic_safe_string, HassClient,
};
use crate::service::state::StateHandle;
use async_trait::async_trait;
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct ButtonConfig {
    #[serde(flatten)]
    pub base: EntityConfig,

    pub command_topic: CommandTopic,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_press: Option<String>,
}

impl ButtonConfig {
    #[allow(dead_code)]
    pub async fn for_device(
        device: &ServiceDevice,
        instance: &DeviceCapability,
    ) -> anyhow::Result<Self> {
        let id = topic_safe_id(device);
        let command_topic = instantiate_route(
            SWITCH_COMMAND_ROUTE,
            &[("id", &id), ("instance", &instance.instance)],
        )?;
        let availability_topic = availability_topic();
        let unique_id = format!("gv2mqtt-{id}-{inst}", inst = instance.instance);

        Ok(Self {
            base: EntityConfig {
                availability_topic,
                name: Some(camel_case_to_space_separated(&instance.instance)),
                device_class: None,
                origin: Origin::default(),
                device: Device::for_device(device),
                unique_id,
                entity_category: None,
                icon: None,
            },
            command_topic,
            payload_press: None,
        })
    }

    pub fn new<NAME: Into<String>>(name: NAME, topic: CommandTopic) -> Self {
        let name = name.into();
        let unique_id = format!("global-{}", topic_safe_string(&name));
        Self {
            base: EntityConfig {
                availability_topic: availability_topic(),
                name: Some(name.to_string()),
                entity_category: None,
                origin: Origin::default(),
                device: Device::this_service(),
                unique_id: unique_id.clone(),
                device_class: None,
                icon: None,
            },
            command_topic: topic,
            payload_press: None,
        }
    }

    pub fn activate_work_mode_preset(
        device: &ServiceDevice,
        name: &str,
        mode_name: &str,
        mode_num: i64,
        value: i64,
    ) -> anyhow::Result<Self> {
        let id = topic_safe_id(device);
        let mode = topic_safe_string(mode_name);
        let mode_num = mode_num.to_string();
        let unique_id = format!("gv2mqtt-{id}-preset-{mode}-{mode_num}-{value}",);
        let command_topic = instantiate_route(
            NUMBER_COMMAND_ROUTE,
            &[("id", &id), ("mode_name", &mode), ("work_mode", &mode_num)],
        )?;
        Ok(Self {
            base: EntityConfig {
                availability_topic: availability_topic(),
                name: Some(name.to_string()),
                entity_category: None,
                origin: Origin::default(),
                device: Device::for_device(device),
                unique_id: unique_id.clone(),
                device_class: None,
                icon: None,
            },
            command_topic,
            payload_press: Some(value.to_string()),
        })
    }

    pub fn scene_next_for_device(device: &ServiceDevice) -> anyhow::Result<Self> {
        let id = topic_safe_id(device);
        let unique_id = format!("gv2mqtt-{id}-scene-next");
        let command_topic = instantiate_route(SCENE_NEXT_ROUTE, &[("id", &id)])?;
        Ok(Self {
            base: EntityConfig {
                availability_topic: availability_topic(),
                name: Some("Scene Next".to_string()),
                entity_category: None,
                origin: Origin::default(),
                device: Device::for_device(device),
                unique_id,
                device_class: None,
                icon: Some("mdi:skip-next".to_string()),
            },
            command_topic,
            payload_press: None,
        })
    }

    /// Clears the stored music sensitivity, returning the entity to unknown and
    /// the next `Music:` effect to the Platform API default.
    ///
    /// A button, not a payload on the number's command topic. Home Assistant
    /// reads `payload_reset` on the *state* topic and never publishes it to the
    /// command topic, so a number entity offers no way to reach "unset" from the
    /// UI. A button is the affordance HA can actually drive.
    pub fn clear_music_sensitivity_for_device(device: &ServiceDevice) -> anyhow::Result<Self> {
        let id = topic_safe_id(device);
        let unique_id = format!("gv2mqtt-{id}-clear-music-sensitivity");
        let command_topic = instantiate_route(MUSIC_SENSITIVITY_CLEAR_ROUTE, &[("id", &id)])?;
        Ok(Self {
            base: EntityConfig {
                availability_topic: availability_topic(),
                name: Some("Clear Music Sensitivity".to_string()),
                entity_category: Some("config".to_string()),
                origin: Origin::default(),
                device: Device::for_device(device),
                unique_id,
                device_class: None,
                icon: Some("mdi:music-note-off".to_string()),
            },
            command_topic,
            payload_press: None,
        })
    }

    pub fn scene_prev_for_device(device: &ServiceDevice) -> anyhow::Result<Self> {
        let id = topic_safe_id(device);
        let unique_id = format!("gv2mqtt-{id}-scene-prev");
        let command_topic = instantiate_route(SCENE_PREV_ROUTE, &[("id", &id)])?;
        Ok(Self {
            base: EntityConfig {
                availability_topic: availability_topic(),
                name: Some("Scene Previous".to_string()),
                entity_category: None,
                origin: Origin::default(),
                device: Device::for_device(device),
                unique_id,
                device_class: None,
                icon: Some("mdi:skip-previous".to_string()),
            },
            command_topic,
            payload_press: None,
        })
    }

    pub fn request_platform_data_for_device(device: &ServiceDevice) -> anyhow::Result<Self> {
        let id = topic_safe_id(device);
        let unique_id = format!("gv2mqtt-{id}-request-platform-data");
        let command_topic = instantiate_route(REQUEST_PLATFORM_DATA_ROUTE, &[("id", &id)])?;
        Ok(Self {
            base: EntityConfig {
                availability_topic: availability_topic(),
                name: Some("Request Platform API State".to_string()),
                entity_category: Some("diagnostic".to_string()),
                origin: Origin::default(),
                device: Device::for_device(device),
                unique_id: unique_id.clone(),
                device_class: None,
                icon: None,
            },
            command_topic,
            payload_press: None,
        })
    }
}

#[async_trait]
impl EntityInstance for ButtonConfig {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("button", state, client, &self.base, self).await
    }

    async fn notify_state(&self, _client: &HassClient) -> anyhow::Result<()> {
        // Buttons have no state
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    // H9999 is deliberately absent from the quirk table (see
    // hass_mqtt::enumerator's tests), so this fixture exercises only the
    // route-substitution logic, not quirk-derived behavior.
    const DEVICE_ID: &str = "AA:BB:CC:DD:EE:FF:11:22";
    const SKU: &str = "H9999";

    fn test_device() -> ServiceDevice {
        ServiceDevice::new(SKU, DEVICE_ID)
    }

    /// `hass.rs` only subscribes to `NUMBER_COMMAND_ROUTE`. A preset button
    /// that drifts from that exact segment order or param set would render
    /// in Home Assistant but never reach a handler. Also locks in that
    /// `mode_num.to_string()` formats a negative work-mode sentinel the same
    /// way the old `{mode_num}` interpolation did.
    #[test]
    fn activate_work_mode_preset_command_topic_matches_the_number_route() {
        let device = test_device();
        let id = topic_safe_id(&device);

        let button = ButtonConfig::activate_work_mode_preset(
            &device,
            "Activate Mode: Gentle",
            "gentleMode",
            -1,
            5,
        )
        .expect("a well-formed preset must build a button");

        let expected = format!("gv2mqtt/number/{id}/command/gentlemode/-1");
        assert_eq!(button.command_topic.as_str(), expected);
    }

    /// A malformed work-mode name (empty, e.g. from a Platform API
    /// capability that failed to parse) must fail loud instead of
    /// advertising a topic with a missing segment (`.../command//1`).
    #[test]
    fn activate_work_mode_preset_rejects_an_empty_mode_name() {
        let device = test_device();
        let error = ButtonConfig::activate_work_mode_preset(&device, "Activate", "", 1, 5)
            .expect_err("an empty mode name must not be advertised");
        assert!(
            error.to_string().contains("empty parameter 'mode_name'"),
            "unexpected error: {error:#}"
        );
    }

    /// These three buttons only ever substitute `:id`. Bundled because they
    /// share one invariant: the literal suffix must match the registered
    /// route exactly, or hass.rs's router never sees the command.
    #[test]
    fn id_only_button_routes_match_their_registered_routes() {
        let device = test_device();
        let id = topic_safe_id(&device);

        let next = ButtonConfig::scene_next_for_device(&device).expect("scene next must build");
        assert_eq!(
            next.command_topic.as_str(),
            format!("gv2mqtt/{id}/scene-next")
        );

        let prev = ButtonConfig::scene_prev_for_device(&device).expect("scene prev must build");
        assert_eq!(
            prev.command_topic.as_str(),
            format!("gv2mqtt/{id}/scene-prev")
        );

        let platform_data = ButtonConfig::request_platform_data_for_device(&device)
            .expect("request platform data must build");
        assert_eq!(
            platform_data.command_topic.as_str(),
            format!("gv2mqtt/{id}/request-platform-data")
        );
    }
}
