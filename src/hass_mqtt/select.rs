use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::command_routes::{
    instantiate_route, SET_MODE_SCENE_ROUTE, SET_WORK_MODE_ROUTE,
};
use crate::hass_mqtt::instance::{publish_entity_config, EntityInstance};
use crate::hass_mqtt::work_mode::ParsedWorkMode;
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{availability_topic, topic_safe_id, HassClient, IdParameter};
use crate::service::state::StateHandle;
use anyhow::Context;
use mosquitto_rs::router::{Params, Payload, State};
use serde::Serialize;
use serde_json::json;

#[derive(Serialize, Clone, Debug)]
pub struct SelectConfig {
    #[serde(flatten)]
    pub base: EntityConfig,

    pub command_topic: crate::hass_mqtt::command_routes::CommandTopic,
    pub options: Vec<String>,
    pub state_topic: String,
}

impl SelectConfig {
    pub async fn publish(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        publish_entity_config("select", state, client, &self.base, self).await
    }
}

pub struct WorkModeSelect {
    select: SelectConfig,
    device_id: String,
    state: StateHandle,
}

impl WorkModeSelect {
    pub fn new(
        device: &ServiceDevice,
        work_modes: &ParsedWorkMode,
        state: &StateHandle,
    ) -> anyhow::Result<Self> {
        let id = topic_safe_id(device);
        let command_topic = instantiate_route(SET_WORK_MODE_ROUTE, &[("id", &id)])?;
        let state_topic = format!("gv2mqtt/{id}/notify-work-mode");
        let availability_topic = availability_topic();
        let unique_id = format!("gv2mqtt-{id}-workMode");

        Ok(Self {
            select: SelectConfig {
                base: EntityConfig {
                    availability_topic,
                    name: Some("Mode".to_string()),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id,
                    entity_category: None,
                    icon: None,
                },
                command_topic,
                state_topic,
                options: work_modes.get_mode_names(),
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        })
    }
}

#[async_trait::async_trait]
impl EntityInstance for WorkModeSelect {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.select.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) = self.state.device_by_id(&self.device_id).await else {
            log::warn!(
                "Device {} not found in state, skipping notify",
                self.device_id
            );
            return Ok(());
        };

        if let Some(mode_value) = device.humidifier_work_mode {
            if let Ok(work_mode) = ParsedWorkMode::with_device(&device) {
                let mode_value_json = json!(mode_value);
                if let Some(mode) = work_mode.mode_for_value(&mode_value_json) {
                    client
                        .publish(&self.select.state_topic, mode.name.to_string())
                        .await?;
                }
            }
        } else {
            let work_modes = ParsedWorkMode::with_device(&device)?;

            if let Some(cap) = device.get_state_capability_by_instance("workMode") {
                if let Some(mode_num) = cap.state.pointer("/value/workMode") {
                    if let Some(mode) = work_modes.mode_for_value(mode_num) {
                        return client
                            .publish(&self.select.state_topic, mode.name.to_string())
                            .await;
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct SceneModeSelect {
    select: SelectConfig,
    device_id: String,
    state: StateHandle,
}

impl SceneModeSelect {
    pub async fn new(device: &ServiceDevice, state: &StateHandle) -> anyhow::Result<Option<Self>> {
        let scenes = state.device_list_scenes(device).await?;
        if scenes.is_empty() {
            return Ok(None);
        }

        let id = topic_safe_id(device);
        let command_topic = instantiate_route(SET_MODE_SCENE_ROUTE, &[("id", &id)])?;
        let state_topic = format!("gv2mqtt/{id}/notify-mode-scene");
        let availability_topic = availability_topic();
        let unique_id = format!("gv2mqtt-{id}-mode-scene");

        Ok(Some(Self {
            select: SelectConfig {
                base: EntityConfig {
                    availability_topic,
                    name: Some("Mode/Scene".to_string()),
                    device_class: None,
                    origin: Origin::default(),
                    device: Device::for_device(device),
                    unique_id,
                    entity_category: None,
                    icon: None,
                },
                command_topic,
                state_topic,
                options: scenes,
            },
            device_id: device.id.to_string(),
            state: state.clone(),
        }))
    }
}

#[async_trait::async_trait]
impl EntityInstance for SceneModeSelect {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()> {
        self.select.publish(state, client).await
    }

    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        let Some(device) = self.state.device_by_id(&self.device_id).await else {
            log::warn!(
                "Device {} not found in state, skipping notify",
                self.device_id
            );
            return Ok(());
        };

        if let Some(device_state) = device.device_state() {
            client
                .publish(
                    &self.select.state_topic,
                    device_state.scene.as_deref().unwrap_or(""),
                )
                .await?;
        }

        Ok(())
    }
}

pub async fn mqtt_set_mode_scene(
    Payload(scene): Payload<String>,
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;

    state
        .device_set_scene(&device, &scene)
        .await
        .context("mqtt_set_mode_scene: state.device_set_scene")?;

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::service::state::{SceneCatalogCache, SceneCatalogCategory, SceneCatalogEntry};

    // H9999 is deliberately absent from the quirk table (see
    // hass_mqtt::enumerator's tests), so this fixture exercises only the
    // route-substitution logic, not quirk-derived behavior.
    const DEVICE_ID: &str = "AA:BB:CC:DD:EE:FF:11:22";
    const SKU: &str = "H9999";

    fn test_device() -> ServiceDevice {
        ServiceDevice::new(SKU, DEVICE_ID)
    }

    fn empty_state() -> StateHandle {
        std::sync::Arc::new(crate::service::state::State::new())
    }

    /// `hass.rs` only subscribes to `SET_WORK_MODE_ROUTE`.
    #[test]
    fn work_mode_select_command_topic_matches_the_registered_route() {
        let device = test_device();
        let work_modes = ParsedWorkMode::default();
        let select = WorkModeSelect::new(&device, &work_modes, &empty_state())
            .expect("a device with no work modes still builds a select");

        let id = topic_safe_id(&device);
        assert_eq!(
            select.select.command_topic.as_str(),
            format!("gv2mqtt/{id}/set-work-mode")
        );
    }

    /// `hass.rs` only subscribes to `SET_MODE_SCENE_ROUTE`. Uses the same
    /// cached-scene-catalog fixture as `hass_mqtt::enumerator`'s tests so no
    /// test ever reaches out to the Govee API.
    #[tokio::test]
    async fn scene_mode_select_command_topic_matches_the_registered_route() {
        let device = test_device();
        let state = empty_state();
        {
            let mut canonical = state.device_mut(&device.sku, &device.id).await;
            canonical.set_scene_catalog(SceneCatalogCache {
                platform_signature: None,
                categories: vec![SceneCatalogCategory {
                    name: "Favorites".to_string(),
                    scenes: vec![SceneCatalogEntry {
                        name: "Sunset".to_string(),
                        icon_urls: vec![],
                        hint: None,
                    }],
                }],
            });
        }

        let select = SceneModeSelect::new(&device, &state)
            .await
            .expect("scene lookup must not fail")
            .expect("a non-empty scene catalog must build a select");

        let id = topic_safe_id(&device);
        assert_eq!(
            select.select.command_topic.as_str(),
            format!("gv2mqtt/{id}/set-mode-scene")
        );
    }
}
