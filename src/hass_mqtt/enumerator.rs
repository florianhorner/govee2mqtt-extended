use crate::hass_mqtt::base::{Device, EntityConfig, Origin};
use crate::hass_mqtt::button::ButtonConfig;
use crate::hass_mqtt::climate::TargetTemperatureEntity;
use crate::hass_mqtt::humidifier::Humidifier;
use crate::hass_mqtt::instance::EntityList;
use crate::hass_mqtt::light::DeviceLight;
use crate::hass_mqtt::number::{MusicSensitivityNumber, WorkModeNumber};
use crate::hass_mqtt::scene::SceneConfig;
use crate::hass_mqtt::select::{SceneModeSelect, WorkModeSelect};
use crate::hass_mqtt::sensor::{
    CapabilitySensor, DeviceStatusDiagnostic, GlobalFixedDiagnostic, SceneInfoSensor,
};
use crate::hass_mqtt::switch::CapabilitySwitch;
use crate::hass_mqtt::work_mode::ParsedWorkMode;
use crate::platform_api::{DeviceCapability, DeviceCapabilityKind, DeviceType};
use crate::service::device::Device as ServiceDevice;
use crate::service::hass::{availability_topic, oneclick_topic, purge_cache_topic};
use crate::service::state::StateHandle;
use crate::version_info::govee_version;
use anyhow::Context;

use uuid::Uuid;

pub async fn enumerate_all_entites(state: &StateHandle) -> anyhow::Result<EntityList> {
    let mut entities = EntityList::new();

    enumerate_global_entities(state, &mut entities).await?;
    enumerate_scenes(state, &mut entities).await?;

    let devices = state.devices().await;

    for d in &devices {
        enumerate_entities_for_device(d, state, &mut entities)
            .await
            .with_context(|| format!("Config::for_device({d})"))?;
    }

    Ok(entities)
}

async fn enumerate_global_entities(
    _state: &StateHandle,
    entities: &mut EntityList,
) -> anyhow::Result<()> {
    entities.add(GlobalFixedDiagnostic::new("Version", govee_version()));
    entities.add(ButtonConfig::new("Purge Caches", purge_cache_topic()));
    Ok(())
}

async fn enumerate_scenes(state: &StateHandle, entities: &mut EntityList) -> anyhow::Result<()> {
    if let Some(undoc) = state.get_undoc_client().await {
        match undoc.parse_one_clicks().await {
            Ok(items) => {
                for oc in items {
                    let unique_id = format!(
                        "gv2mqtt-one-click-{}",
                        Uuid::new_v5(&Uuid::NAMESPACE_DNS, oc.name.as_bytes()).simple()
                    );
                    entities.add(SceneConfig {
                        base: EntityConfig {
                            availability_topic: availability_topic(),
                            name: Some(oc.name.to_string()),
                            entity_category: None,
                            origin: Origin::default(),
                            device: Device::this_service(),
                            unique_id: unique_id.clone(),
                            device_class: None,
                            icon: None,
                        },
                        command_topic: oneclick_topic(),
                        payload_on: oc.name,
                    });
                }
            }
            Err(err) => {
                log::warn!("Failed to parse one-clicks: {err:#}");
            }
        }
    }

    Ok(())
}

async fn entities_for_work_mode(
    d: &ServiceDevice,
    state: &StateHandle,
    cap: &DeviceCapability,
    entities: &mut EntityList,
) -> anyhow::Result<()> {
    let mut work_modes = ParsedWorkMode::with_capability(cap)?;
    work_modes.adjust_for_device(&d.sku);

    let quirk = d.resolve_quirk();

    for work_mode in work_modes.modes.values() {
        let Some(mode_num) = work_mode.value.as_i64() else {
            continue;
        };

        let range = work_mode.contiguous_value_range();

        let show_as_preset = work_mode.should_show_as_preset()
            || quirk
                .as_ref()
                .map(|q| q.should_show_mode_as_preset(&work_mode.name))
                .unwrap_or(false);

        if show_as_preset {
            if work_mode.values.is_empty() {
                entities.add(ButtonConfig::activate_work_mode_preset(
                    d,
                    &format!("Activate Mode: {}", work_mode.label()),
                    &work_mode.name,
                    mode_num,
                    work_mode.default_value(),
                ));
            } else {
                for value in &work_mode.values {
                    if let Some(mode_value) = value.value.as_i64() {
                        entities.add(ButtonConfig::activate_work_mode_preset(
                            d,
                            &value.computed_label,
                            &work_mode.name,
                            mode_num,
                            mode_value,
                        ));
                    }
                }
            }
        } else {
            let label = work_mode.label().to_string();

            entities.add(WorkModeNumber::new(
                d,
                state,
                label,
                &work_mode.name,
                work_mode.value.clone(),
                range,
            ));
        }
    }

    entities.add(WorkModeSelect::new(d, &work_modes, state));

    Ok(())
}

pub async fn enumerate_entities_for_device(
    d: &ServiceDevice,
    state: &StateHandle,
    entities: &mut EntityList,
) -> anyhow::Result<()> {
    if !d.is_controllable() {
        return Ok(());
    }

    entities.add(DeviceStatusDiagnostic::new(d, state));
    entities.add(ButtonConfig::request_platform_data_for_device(d));

    // Add scene cycling buttons for devices that support scenes
    if d.supports_rgb() || d.get_color_temperature_range().is_some() {
        entities.add(ButtonConfig::scene_next_for_device(d));
        entities.add(ButtonConfig::scene_prev_for_device(d));
        entities.add(SceneInfoSensor::new(d, state));
    }

    if d.supports_rgb() || d.get_color_temperature_range().is_some() || d.supports_brightness() {
        entities.add(DeviceLight::for_device(d, state, None).await?);
    }

    if matches!(
        d.device_type(),
        DeviceType::Humidifier | DeviceType::Dehumidifier
    ) {
        entities.add(Humidifier::new(d, state).await?);
    }

    if d.device_type() != DeviceType::Light {
        if let Some(scenes) = SceneModeSelect::new(d, state).await? {
            entities.add(scenes);
        }
    }

    if let Some(info) = &d.http_device_info {
        for cap in &info.capabilities {
            match &cap.kind {
                DeviceCapabilityKind::Toggle | DeviceCapabilityKind::OnOff => {
                    entities.add(CapabilitySwitch::new(d, state, cap).await?);
                }
                DeviceCapabilityKind::MusicSetting
                    if cap.has_music_mode_options() && !d.avoid_platform_api() =>
                {
                    // Styles already ship as `Music: <name>` light effects; the
                    // only thing HA cannot reach is the sensitivity parameter.
                    //
                    // Gated on the same condition `device_set_scene` uses to
                    // pick the Platform branch. Devices we deliberately keep off
                    // the Platform API (the `with_broken_platform` quirks) take
                    // the LAN path, which cannot carry sensitivity — publishing
                    // the entity there would echo a value that never applies.
                    entities.add(MusicSensitivityNumber::new(d, state));
                }

                DeviceCapabilityKind::ColorSetting
                | DeviceCapabilityKind::SegmentColorSetting
                | DeviceCapabilityKind::MusicSetting
                | DeviceCapabilityKind::Event
                | DeviceCapabilityKind::Mode
                | DeviceCapabilityKind::DynamicScene => {}

                DeviceCapabilityKind::Range if cap.instance == "brightness" => {}
                DeviceCapabilityKind::Range if cap.instance == "humidity" => {}
                DeviceCapabilityKind::WorkMode => {
                    entities_for_work_mode(d, state, cap, entities).await?;
                }

                DeviceCapabilityKind::Property => {
                    entities.add(CapabilitySensor::new(d, state, cap).await?);
                }

                DeviceCapabilityKind::TemperatureSetting => {
                    entities.add(TargetTemperatureEntity::new(d, state, cap).await?);
                }

                kind => {
                    log::warn!(
                        "Do something about {kind:?} {} for {d} {cap:?}",
                        cap.instance
                    );
                }
            }
        }

        if let Some(segments) = info.supports_segmented_rgb() {
            for n in segments {
                entities.add(DeviceLight::for_device(d, state, Some(n)).await?);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::platform_api::{DeviceParameters, HttpDeviceInfo};
    use crate::service::state::{SceneCatalogCache, State};
    use std::sync::Arc;

    const DEVICE_ID: &str = "AA:BB:CC:DD:EE:FF:11:22";

    fn music_mode_capability(instance: &str) -> DeviceCapability {
        let info = crate::platform_api::test::music_device();
        let mut capability = info
            .capability_by_instance("musicMode")
            .cloned()
            .expect("fixture has a usable musicMode capability");
        capability.instance = instance.to_string();
        capability
    }

    fn device_with_capabilities(sku: &str, capabilities: Vec<DeviceCapability>) -> ServiceDevice {
        let mut device = ServiceDevice::new(sku, DEVICE_ID);
        device.http_device_info = Some(HttpDeviceInfo {
            sku: sku.to_string(),
            device: DEVICE_ID.to_string(),
            device_name: "Test Light".to_string(),
            device_type: DeviceType::Light,
            capabilities,
        });
        device
    }

    /// Enumerates against a state that already holds an (empty) scene catalog
    /// for the device, so the light entity's effect list is served from cache
    /// and no test ever reaches out to the Govee API.
    async fn entity_count(device: &ServiceDevice) -> usize {
        let state: StateHandle = Arc::new(State::new());
        {
            let mut canonical = state.device_mut(&device.sku, &device.id).await;
            canonical.set_scene_catalog(SceneCatalogCache {
                platform_signature: None,
                categories: vec![],
            });
        }

        let mut entities = EntityList::new();
        enumerate_entities_for_device(device, &state, &mut entities)
            .await
            .expect("enumeration must not fail");
        entities.len()
    }

    /// A Platform-API device that advertises `musicMode` gains exactly one
    /// extra entity: the sensitivity slider. Measured as a delta against the
    /// same device without the capability, so unrelated entity additions to
    /// the enumerator do not make this test lie.
    #[tokio::test]
    async fn music_mode_capability_publishes_the_sensitivity_slider() {
        // H9999 is deliberately absent from the quirk table: no quirk means
        // no rgb/brightness inference, so only the capability list matters.
        let without = entity_count(&device_with_capabilities("H9999", vec![])).await;
        let with = entity_count(&device_with_capabilities(
            "H9999",
            // Platform responses are not guaranteed to preserve casing; the
            // production match is deliberately case-insensitive.
            vec![music_mode_capability("MusicMode")],
        ))
        .await;

        assert_eq!(
            with,
            without + 1,
            "musicMode must add exactly the Music Sensitivity number"
        );
    }

    /// The arm is guarded on the instance name, not just the capability kind.
    /// Other `MusicSetting` instances must keep falling through to the
    /// deliberate no-op arm rather than publishing a slider that maps to
    /// nothing (or hitting the `kind => warn` catch-all).
    #[tokio::test]
    async fn other_music_setting_instances_publish_nothing() {
        let without = entity_count(&device_with_capabilities("H9999", vec![])).await;
        let with = entity_count(&device_with_capabilities(
            "H9999",
            vec![music_mode_capability("musicScene")],
        ))
        .await;

        assert_eq!(with, without, "only the musicMode instance is actionable");
    }

    #[tokio::test]
    async fn unusable_music_mode_metadata_does_not_publish_the_slider() {
        let without = entity_count(&device_with_capabilities("H9999", vec![])).await;

        let mut missing_parameters = music_mode_capability("musicMode");
        missing_parameters.parameters = None;

        let mut empty_options = music_mode_capability("musicMode");
        let Some(DeviceParameters::Struct { fields }) = &mut empty_options.parameters else {
            panic!("fixture musicMode parameters must be a struct");
        };
        let field = fields
            .iter_mut()
            .find(|field| field.field_name == "musicMode")
            .expect("fixture has the musicMode field");
        let DeviceParameters::Enum { options } = &mut field.field_type else {
            panic!("fixture musicMode field must be an enum");
        };
        options.clear();

        let mut non_numeric_options = music_mode_capability("musicMode");
        let Some(DeviceParameters::Struct { fields }) = &mut non_numeric_options.parameters else {
            panic!("fixture musicMode parameters must be a struct");
        };
        let field = fields
            .iter_mut()
            .find(|field| field.field_name == "musicMode")
            .expect("fixture has the musicMode field");
        let DeviceParameters::Enum { options } = &mut field.field_type else {
            panic!("fixture musicMode field must be an enum");
        };
        for option in options {
            option.value = serde_json::json!("not-a-numeric-mode");
        }

        for capability in [missing_parameters, empty_options, non_numeric_options] {
            let with = entity_count(&device_with_capabilities("H9999", vec![capability])).await;
            assert_eq!(
                with, without,
                "a capability with no selectable styles must not add a dead slider"
            );
        }
    }

    /// Devices quirked off the Platform API take the LAN scene path, which
    /// cannot carry sensitivity. Publishing the slider there would echo a
    /// value that never reaches the device. H6141 is `with_broken_platform`.
    #[tokio::test]
    async fn broken_platform_quirks_do_not_publish_the_slider() {
        let quirked = device_with_capabilities("H6141", vec![music_mode_capability("musicMode")]);
        assert!(
            quirked.avoid_platform_api(),
            "H6141 must still be quirked off the Platform API, or this test \
             is guarding nothing"
        );

        let without = entity_count(&device_with_capabilities("H6141", vec![])).await;
        let with = entity_count(&quirked).await;

        assert_eq!(
            with, without,
            "a LAN-only device must not get a sensitivity slider"
        );
    }
}
