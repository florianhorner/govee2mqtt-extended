use crate::cache::{cache_get, CacheComputeResult, CacheGetOptions};
use crate::hass_mqtt::climate::parse_temperature_constraints;
use crate::opt_env_var;
use crate::sensitive::{
    describe_json_error_with_policy, describe_request_url, describe_sensitive_body_with_policy,
    should_log_sensitive_data,
};
use crate::service::state::sort_and_dedup_scenes;
use crate::temperature::{TemperatureUnits, TemperatureValue};
use crate::undoc_api::GoveeUndocumentedApi;
use anyhow::Context;
use reqwest::Method;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value as JsonValue};
use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;

// This file implements the Govee Platform API V1 as described at:
// <https://developer.govee.com/reference/get-you-devices>
//
// It is NOT the same thing as the older, but confusingly versioned
// with a higher number, Govee HTTP API v2 that is described at
// <https://govee.readme.io/reference/getlightdeviceinfo>

const SERVER: &str = "https://openapi.api.govee.com";
pub const ONE_WEEK: Duration = Duration::from_secs(86400 * 7);
pub const FIVE_MINUTES: Duration = Duration::from_secs(5 * 60);

fn endpoint(url: &str) -> String {
    format!("{SERVER}{url}")
}

fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(clap::Parser, Debug)]
pub struct GoveeApiArguments {
    /// The Govee API Key. If not passed here, it will be read from
    /// the GOVEE_API_KEY environment variable.
    #[arg(long, global = true)]
    pub api_key: Option<String>,
}

impl GoveeApiArguments {
    pub fn opt_api_key(&self) -> anyhow::Result<Option<String>> {
        match &self.api_key {
            Some(key) => Ok(Some(key.to_string())),
            None => opt_env_var("GOVEE_API_KEY"),
        }
    }

    pub fn api_key(&self) -> anyhow::Result<String> {
        self.opt_api_key()?.ok_or_else(|| {
            anyhow::anyhow!(
                "Please specify the api key either via the \
                --api-key parameter or by setting $GOVEE_API_KEY"
            )
        })
    }

    pub fn api_client(&self) -> anyhow::Result<GoveeApiClient> {
        let key = self.api_key()?;
        Ok(GoveeApiClient::new(key))
    }
}

#[derive(Clone)]
pub struct GoveeApiClient {
    key: String,
}

/// Build the `music_setting` payload for a named style.
///
/// Returns `None` when the capability does not offer that style, which is the
/// signal to fall through to the ordinary scene lookup. Split out from
/// `set_scene_by_name_with_sensitivity` so the wire shape is testable against
/// the recorded device fixtures without an HTTP mock.
fn build_music_mode_payload(
    cap: &DeviceCapability,
    music_mode: &str,
    sensitivity: u8,
) -> Option<JsonValue> {
    let value = cap
        .struct_field_by_name("musicMode")?
        .field_type
        .enum_parameter_by_name(music_mode)?;
    Some(serde_json::json!({
        "musicMode": value,
        // Govee's range is a percentage; clamp rather than trust the caller.
        "sensitivity": sensitivity.min(100),
        "autoColor": 1,
    }))
}

/// Sensitivity this bridge sent unconditionally before the preference
/// existed. Kept as the fallback so untouched installs are unchanged.
pub const DEFAULT_MUSIC_SENSITIVITY: u8 = 100;

impl GoveeApiClient {
    pub fn new<K: Into<String>>(key: K) -> Self {
        Self { key: key.into() }
    }

    pub async fn get_devices(&self) -> anyhow::Result<Vec<HttpDeviceInfo>> {
        cache_get(
            CacheGetOptions {
                topic: "http-api",
                key: "device-list",
                soft_ttl: Duration::from_secs(900),
                hard_ttl: ONE_WEEK,
                negative_ttl: Duration::from_secs(60),
                allow_stale: true,
            },
            async {
                let url = endpoint("/router/api/v1/user/devices");
                let resp: GetDevicesResponse = self.get_request_with_json_response(url).await?;
                Ok(CacheComputeResult::Value(resp.data))
            },
        )
        .await
    }

    pub async fn get_device_by_id<I: AsRef<str>>(&self, id: I) -> anyhow::Result<HttpDeviceInfo> {
        let id = id.as_ref();
        let devices = self.get_devices().await?;
        for d in devices {
            if d.device == id {
                return Ok(d);
            }
        }
        anyhow::bail!("device {id} not found");
    }

    pub async fn control_device<V: Into<JsonValue>>(
        &self,
        device: &HttpDeviceInfo,
        capability: &DeviceCapability,
        value: V,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        let url = endpoint("/router/api/v1/device/control");
        let request = ControlDeviceRequest {
            request_id: request_id(),
            payload: ControlDevicePayload {
                sku: device.sku.to_string(),
                device: device.device.to_string(),
                capability: ControlDeviceCapability {
                    kind: capability.kind.clone(),
                    instance: capability.instance.to_string(),
                    value: value.into(),
                },
            },
        };

        let resp: ControlDeviceResponse = self
            .request_with_json_response(Method::POST, url, &request)
            .await?;

        log::info!("control_device result: {resp:?}");

        Ok(resp.capability)
    }

    pub async fn get_device_state(
        &self,
        device: &HttpDeviceInfo,
    ) -> anyhow::Result<HttpDeviceState> {
        let url = endpoint("/router/api/v1/device/state");
        let request = GetDeviceStateRequest {
            request_id: request_id(),
            payload: GetDeviceStateRequestPayload {
                sku: device.sku.to_string(),
                device: device.device.to_string(),
            },
        };

        let resp: GetDeviceStateResponse = self
            .request_with_json_response(Method::POST, url, &request)
            .await?;

        Ok(resp.payload)
    }

    pub async fn get_device_diy_scenes(
        &self,
        device: &HttpDeviceInfo,
    ) -> anyhow::Result<Vec<DeviceCapability>> {
        if !device.supports_dynamic_scenes() {
            return Ok(vec![]);
        }

        let key = format!("scene-list-diy-{}-{}", device.sku, device.device);
        cache_get(
            CacheGetOptions {
                topic: "http-api",
                key: &key,
                soft_ttl: Duration::from_secs(300),
                hard_ttl: ONE_WEEK,
                negative_ttl: FIVE_MINUTES,
                allow_stale: true,
            },
            async {
                let url = endpoint("/router/api/v1/device/diy-scenes");
                let request = GetDeviceScenesRequest {
                    request_id: request_id(),
                    payload: GetDeviceScenesPayload {
                        sku: device.sku.to_string(),
                        device: device.device.to_string(),
                    },
                };

                let resp: GetDeviceScenesResponse = self
                    .request_with_json_response(Method::POST, url, &request)
                    .await?;

                Ok(CacheComputeResult::Value(resp.payload.capabilities))
            },
        )
        .await
    }

    pub async fn get_device_scenes(
        &self,
        device: &HttpDeviceInfo,
    ) -> anyhow::Result<Vec<DeviceCapability>> {
        if !device.supports_dynamic_scenes() {
            return Ok(vec![]);
        }

        let key = format!("scene-list-{}-{}", device.sku, device.device);
        cache_get(
            CacheGetOptions {
                topic: "http-api",
                key: &key,
                soft_ttl: Duration::from_secs(300),
                hard_ttl: ONE_WEEK,
                negative_ttl: FIVE_MINUTES,
                allow_stale: true,
            },
            async {
                let url = endpoint("/router/api/v1/device/scenes");
                let request = GetDeviceScenesRequest {
                    request_id: request_id(),
                    payload: GetDeviceScenesPayload {
                        sku: device.sku.to_string(),
                        device: device.device.to_string(),
                    },
                };

                let resp: GetDeviceScenesResponse = self
                    .request_with_json_response(Method::POST, url, &request)
                    .await?;

                Ok(CacheComputeResult::Value(resp.payload.capabilities))
            },
        )
        .await
    }

    pub async fn get_scene_caps(
        &self,
        device: &HttpDeviceInfo,
    ) -> anyhow::Result<Vec<DeviceCapability>> {
        let mut result = vec![];

        let scene_caps = self.get_device_scenes(device).await?;
        let diy_caps = self.get_device_diy_scenes(device).await?;
        let undoc_caps =
            match GoveeUndocumentedApi::synthesize_platform_api_scene_list(&device.sku).await {
                Ok(caps) => caps,
                Err(err) => {
                    log::warn!("synthesize_platform_api_scene_list: {err:#}");
                    vec![]
                }
            };

        for (origin, caps) in [
            ("device.capabilities", &device.capabilities),
            ("scene_caps", &scene_caps),
            ("diy_caps", &diy_caps),
            ("undoc_caps", &undoc_caps),
        ] {
            for cap in caps {
                let is_scene = matches!(
                    cap.kind,
                    DeviceCapabilityKind::DynamicScene
                        | DeviceCapabilityKind::DynamicSetting
                        | DeviceCapabilityKind::Mode
                );
                if !is_scene {
                    continue;
                }

                match &cap.parameters {
                    Some(DeviceParameters::Enum { .. }) => {
                        result.push(cap.clone());
                    }
                    None => {
                        // This device has no scenes, skip it.
                    }
                    _ => {
                        log::warn!(
                            "get_scene_caps(sku={sku} device={id}): \
                            Unexpected cap.parameters in {origin}: {cap:#?}. \
                            Ignoring this entry.",
                            sku = device.sku,
                            id = device.device
                        );
                    }
                }
            }
        }

        Ok(result)
    }

    pub async fn list_scene_names(&self, device: &HttpDeviceInfo) -> anyhow::Result<Vec<String>> {
        let mut result = vec![];

        let caps = self
            .get_scene_caps(device)
            .await
            .context("list_scene_names: get_scene_caps")?;
        for cap in caps {
            match &cap.parameters {
                Some(DeviceParameters::Enum { options }) => {
                    for opt in options {
                        result.push(opt.name.to_string());
                    }
                }
                _ => anyhow::bail!("list_scene_names: unexpected type {cap:#?}"),
            }
        }

        // Add in music modes
        if let Some(cap) = device.capability_by_instance("musicMode") {
            if let Some(DeviceParameters::Struct { fields }) = &cap.parameters {
                for f in fields {
                    if f.field_name == "musicMode" {
                        if let DeviceParameters::Enum { options } = &f.field_type {
                            for opt in options {
                                result.push(format!("Music: {}", opt.name));
                            }
                        }
                    }
                }
            }
        }

        if !result.is_empty() {
            result.insert(0, "".to_string());
        }

        Ok(sort_and_dedup_scenes(result))
    }

    pub async fn set_scene_by_name(
        &self,
        device: &HttpDeviceInfo,
        scene: &str,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        self.set_scene_by_name_with_sensitivity(device, scene, DEFAULT_MUSIC_SENSITIVITY)
            .await
    }

    /// As [`Self::set_scene_by_name`], but carries the sensitivity to use when
    /// `scene` names a music mode. Sensitivity cannot be sent on its own (Govee
    /// rejects a `musicMode`-less call), so the caller's stored preference rides
    /// along with the style change that does reach the device.
    pub async fn set_scene_by_name_with_sensitivity(
        &self,
        device: &HttpDeviceInfo,
        scene: &str,
        sensitivity: u8,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        if scene.is_empty() {
            // Can't set no scene
            anyhow::bail!("Cannot set scene to no-scene");
        }

        if let Some(music_mode) = scene.strip_prefix("Music: ") {
            if let Some(cap) = device.capability_by_instance("musicMode") {
                if let Some(value) = build_music_mode_payload(cap, music_mode, sensitivity) {
                    return self.control_device(device, cap, value).await;
                }
            }
        }

        let caps = self.get_scene_caps(device).await?;
        for cap in caps {
            match &cap.parameters {
                Some(DeviceParameters::Enum { options }) => {
                    for opt in options {
                        if scene.eq_ignore_ascii_case(&opt.name) {
                            return self.control_device(device, &cap, opt.value.clone()).await;
                        }
                    }
                }
                _ => anyhow::bail!("set_scene_by_name: unexpected type {cap:#?}"),
            }
        }
        anyhow::bail!("Scene '{scene}' is not available for this device");
    }

    pub async fn set_target_temperature(
        &self,
        device: &HttpDeviceInfo,
        instance_name: &str,
        target: TemperatureValue,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        let cap = device
            .capability_by_instance(instance_name)
            .ok_or_else(|| anyhow::anyhow!("device has no {instance_name}"))?;

        let constraints = parse_temperature_constraints(cap)?.as_unit(TemperatureUnits::Celsius);

        let min = constraints.min.as_celsius();
        let max = constraints.max.as_celsius();
        let celsius = target.as_celsius().max(min).min(max);
        let clamped = celsius.max(min).min(max);
        if clamped != celsius {
            log::info!(
                "set_target_temperature: constraining requested {celsius} to \
                       {clamped} because min={min} and max={max}"
            );
        }

        let value = json!({
            "temperature": celsius,
            "unit": "Celsius",
        });

        self.control_device(device, cap, value).await
    }

    pub async fn set_work_mode(
        &self,
        device: &HttpDeviceInfo,
        work_mode: i64,
        value: i64,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        let cap = device
            .capability_by_instance("workMode")
            .ok_or_else(|| anyhow::anyhow!("device has no workMode"))?;

        let value = json!({
            "workMode": work_mode,
            "modeValue": value
        });

        self.control_device(device, cap, value).await
    }

    pub async fn set_toggle_state(
        &self,
        device: &HttpDeviceInfo,
        instance: &str,
        on: bool,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        let cap = device
            .capability_by_instance(instance)
            .ok_or_else(|| anyhow::anyhow!("device has no {instance}"))?;

        let value = cap
            .enum_parameter_by_name(if on { "on" } else { "off" })
            .ok_or_else(|| anyhow::anyhow!("{instance} has no on/off!?"))?;

        self.control_device(device, cap, value).await
    }

    pub async fn set_power_state(
        &self,
        device: &HttpDeviceInfo,
        on: bool,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        self.set_toggle_state(device, "powerSwitch", on).await
    }

    pub async fn set_brightness(
        &self,
        device: &HttpDeviceInfo,
        percent: u8,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        let cap = device
            .capability_by_instance("brightness")
            .ok_or_else(|| anyhow::anyhow!("device has no brightness"))?;
        let value = match &cap.parameters {
            Some(DeviceParameters::Integer {
                range: IntegerRange { min, max, .. },
                ..
            }) => (percent as u32).max(*min).min(*max),
            _ => anyhow::bail!("unexpected parameter type for brightness"),
        };
        self.control_device(device, cap, value).await
    }

    pub async fn set_color_temperature(
        &self,
        device: &HttpDeviceInfo,
        kelvin: u32,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        let cap = device
            .capability_by_instance("colorTemperatureK")
            .ok_or_else(|| anyhow::anyhow!("device has no colorTemperatureK"))?;
        let value = match &cap.parameters {
            Some(DeviceParameters::Integer {
                range: IntegerRange { min, max, .. },
                ..
            }) => (kelvin).max(*min).min(*max),
            _ => anyhow::bail!("unexpected parameter type for colorTemperatureK"),
        };
        self.control_device(device, cap, value).await
    }

    pub async fn set_color_rgb(
        &self,
        device: &HttpDeviceInfo,
        r: u8,
        g: u8,
        b: u8,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        let cap = device
            .capability_by_instance("colorRgb")
            .ok_or_else(|| anyhow::anyhow!("device has no colorRgb"))?;
        let value = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
        self.control_device(device, cap, value).await
    }

    pub async fn set_segment_rgb(
        &self,
        device: &HttpDeviceInfo,
        segment: u32,
        r: u8,
        g: u8,
        b: u8,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        let cap = device
            .capability_by_instance("segmentedColorRgb")
            .ok_or_else(|| anyhow::anyhow!("device has no segmentedColorRgb"))?;
        let value = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
        self.control_device(
            device,
            cap,
            json!({
                "segment": vec![segment],
                "rgb": value,
            }),
        )
        .await
    }

    pub async fn set_segment_brightness(
        &self,
        device: &HttpDeviceInfo,
        segment: u32,
        percent: u8,
    ) -> anyhow::Result<ControlDeviceResponseCapability> {
        let cap = device
            .capability_by_instance("segmentedBrightness")
            .ok_or_else(|| anyhow::anyhow!("device has no segmentedBrightness"))?;

        let (min, max) = device
            .supports_segmented_brightness()
            .ok_or_else(|| anyhow::anyhow!("device doesnt support segmented brightness"))?;

        let value = (percent as u32).max(min).min(max);

        self.control_device(
            device,
            cap,
            json!({
                "segment": vec![segment],
                "brightness": value,
            }),
        )
        .await
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
struct GetDeviceScenesResponse {
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub code: u32,
    #[serde(rename = "msg")]
    pub message: String,
    pub payload: GetDeviceScenesResponsePayload,
}

#[derive(Deserialize, Serialize, Debug)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
struct GetDeviceScenesResponsePayload {
    pub sku: String,
    pub device: String,
    pub capabilities: Vec<DeviceCapability>,
}

#[derive(Serialize, Debug)]
struct GetDeviceScenesRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub payload: GetDeviceScenesPayload,
}

#[derive(Serialize, Debug)]
struct GetDeviceScenesPayload {
    pub sku: String,
    pub device: String,
}

#[derive(Serialize, Debug)]
struct ControlDeviceRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub payload: ControlDevicePayload,
}

#[derive(Serialize, Debug)]
struct ControlDevicePayload {
    pub sku: String,
    pub device: String,
    pub capability: ControlDeviceCapability,
}

#[derive(Serialize, Debug)]
struct ControlDeviceCapability {
    #[serde(rename = "type")]
    pub kind: DeviceCapabilityKind,
    pub instance: String,
    pub value: JsonValue,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct ControlDeviceResponse {
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub code: u32,
    #[serde(rename = "msg")]
    pub message: String,

    pub capability: ControlDeviceResponseCapability,
}

#[derive(Deserialize, Debug)]
#[allow(unused)]
pub struct ControlDeviceResponseCapability {
    #[serde(rename = "type")]
    pub kind: DeviceCapabilityKind,
    pub instance: String,
    pub value: JsonValue,
    pub state: JsonValue,
}

#[derive(Serialize, Debug)]
struct GetDeviceStateRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub payload: GetDeviceStateRequestPayload,
}

#[derive(Serialize, Debug)]
struct GetDeviceStateRequestPayload {
    pub sku: String,
    pub device: String,
}

#[derive(Deserialize, Serialize, Debug)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
struct GetDeviceStateResponse {
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub code: u32,
    #[serde(rename = "msg")]
    pub message: String,
    pub payload: HttpDeviceState,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct HttpDeviceState {
    pub sku: String,
    pub device: String,
    pub capabilities: Vec<DeviceCapabilityState>,
}

impl HttpDeviceState {
    pub fn capability_by_instance(&self, instance: &str) -> Option<&DeviceCapabilityState> {
        self.capabilities
            .iter()
            .find(|c| c.instance.eq_ignore_ascii_case(instance))
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "type")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct DeviceCapabilityState {
    #[serde(rename = "type")]
    pub kind: DeviceCapabilityKind,
    pub instance: String,
    pub state: JsonValue,
}

#[derive(Deserialize, Serialize, Debug)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
struct GetDevicesResponse {
    pub code: u32,
    pub message: String,
    pub data: Vec<HttpDeviceInfo>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct HttpDeviceInfo {
    pub sku: String,
    pub device: String,
    #[serde(default, rename = "deviceName")]
    pub device_name: String,
    #[serde(default, rename = "type")]
    pub device_type: DeviceType,
    pub capabilities: Vec<DeviceCapability>,
}

impl HttpDeviceInfo {
    pub fn capability_by_instance(&self, instance: &str) -> Option<&DeviceCapability> {
        self.capabilities
            .iter()
            .find(|c| c.instance.eq_ignore_ascii_case(instance))
    }

    pub fn supports_rgb(&self) -> bool {
        self.capability_by_instance("colorRgb").is_some()
    }

    pub fn supports_brightness(&self) -> bool {
        self.capability_by_instance("brightness").is_some()
    }

    pub fn supports_dynamic_scenes(&self) -> bool {
        self.capabilities
            .iter()
            .any(|cap| cap.kind == DeviceCapabilityKind::DynamicScene)
    }

    /// If supported, returns the number of segments
    pub fn supports_segmented_rgb(&self) -> Option<std::ops::Range<u32>> {
        let cap = self.capability_by_instance("segmentedColorRgb")?;
        let field = cap.struct_field_by_name("segment")?;
        match field.field_type {
            DeviceParameters::Array {
                size:
                    Some(ArraySize {
                        // These are the display indices. eg: 1-based
                        min: label_min,
                        max: label_max,
                    }),
                element_range:
                    Some(ElementRange {
                        // These are the actual indices. eg: 0-based
                        min: range_min,
                        // We ignore the max here, because the data
                        // reported by Govee can be bogus:
                        // <https://developer.govee.com/discuss/6599afb91cb48d002dbed2b8>
                        max: _,
                    }),
                ..
            } => {
                // This range is an inclusive range, so add 1
                let num_segments = (1 + label_max).saturating_sub(label_min);
                // Return our exclusive range
                Some(range_min..range_min + num_segments)
            }
            _ => None,
        }
    }

    pub fn supports_segmented_brightness(&self) -> Option<(u32, u32)> {
        let cap = self.capability_by_instance("segmentedBrightness")?;
        let field = cap.struct_field_by_name("brightness")?;
        match &field.field_type {
            DeviceParameters::Integer {
                range: IntegerRange { min, max, .. },
                ..
            } => Some((*min, *max)),
            _ => None,
        }
    }

    pub fn get_color_temperature_range(&self) -> Option<(u32, u32)> {
        let cap = self.capability_by_instance("colorTemperatureK")?;

        match cap.parameters {
            Some(DeviceParameters::Integer {
                range: IntegerRange { min, max, .. },
                ..
            }) => Some((min, max)),
            _ => None,
        }
    }
}

/// Helper to generate boilerplate around govee enum string types
macro_rules! enum_string {
    {pub enum $name:ident {
     $($var:ident = $label:literal),* $(,)?
     }
    } => {

#[derive(Debug, Clone, PartialEq, Eq, strum_macros::Display, strum_macros::EnumString)]
pub enum $name {
    $(
        #[strum(serialize = $label)]
        $var,
    )*
        Other(String),
}

impl Default for $name {
    fn default() -> Self {
        Self::Other("NONE".to_string())
    }
}

impl<'de> Deserialize<'de> for $name {
    fn deserialize<D>(d: D) -> Result<Self, <D as Deserializer<'de>>::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(d)?;

        if let Ok(t) = s.parse::<Self>() {
            Ok(t)
        } else {
            Ok(Self::Other(s))
        }
    }
}

impl Serialize for $name {
    fn serialize<S>(&self, serializer: S) -> Result<<S as Serializer>::Ok, <S as Serializer>::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Other(s) => s.serialize(serializer),
            _ => self.to_string().serialize(serializer),
        }
    }
}

    }
}

enum_string! {
pub enum DeviceType {
    Light = "devices.types.light",
    AirPurifier = "devices.types.air_purifier",
    Thermometer = "devices.types.thermometer",
    Socket = "devices.types.socket",
    Sensor = "devices.types.sensor",
    Heater = "devices.types.heater",
    Humidifier = "devices.types.humidifier",
    Dehumidifier = "devices.types.dehumidifier",
    IceMaker = "devices.types.ice_maker",
    AromaDiffuser = "devices.types.aroma_diffuser",
    Fan = "devices.types.fan",
    Kettle = "devices.types.kettle",
}
}

enum_string! {
pub enum DeviceCapabilityKind {
    OnOff = "devices.capabilities.on_off",
    Toggle = "devices.capabilities.toggle",
    Range = "devices.capabilities.range",
    Mode = "devices.capabilities.mode",
    ColorSetting = "devices.capabilities.color_setting",
    SegmentColorSetting = "devices.capabilities.segment_color_setting",
    MusicSetting = "devices.capabilities.music_setting",
    DynamicScene = "devices.capabilities.dynamic_scene",
    WorkMode = "devices.capabilities.work_mode",
    DynamicSetting = "devices.capabilities.dynamic_setting",
    TemperatureSetting = "devices.capabilities.temperature_setting",
    Online = "devices.capabilities.online",
    Property = "devices.capabilities.property",
    Event = "devices.capabilities.event",
}
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct DeviceCapability {
    #[serde(rename = "type")]
    pub kind: DeviceCapabilityKind,
    pub instance: String,
    pub parameters: Option<DeviceParameters>,
    #[serde(rename = "alarmType")]
    pub alarm_type: Option<u32>,
    #[serde(rename = "eventState")]
    pub event_state: Option<JsonValue>,
}

impl DeviceCapability {
    pub fn enum_parameter_by_name(&self, name: &str) -> Option<u32> {
        self.parameters
            .as_ref()
            .and_then(|p| p.enum_parameter_by_name(name))
    }

    pub fn struct_field_by_name(&self, name: &str) -> Option<&StructField> {
        match &self.parameters {
            Some(DeviceParameters::Struct { fields }) => {
                fields.iter().find(|f| f.field_name == name)
            }
            _ => None,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "dataType")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub enum DeviceParameters {
    #[serde(rename = "ENUM")]
    Enum { options: Vec<EnumOption> },
    #[serde(rename = "INTEGER")]
    Integer {
        unit: Option<String>,
        range: IntegerRange,
    },
    #[serde(rename = "STRUCT")]
    Struct { fields: Vec<StructField> },
    #[serde(rename = "Array")]
    Array {
        size: Option<ArraySize>,
        #[serde(rename = "elementRange")]
        element_range: Option<ElementRange>,
        #[serde(rename = "elementType")]
        element_type: Option<String>,
        #[serde(default)]
        options: Vec<ArrayOption>,
    },
}

impl DeviceParameters {
    pub fn enum_parameter_by_name(&self, name: &str) -> Option<u32> {
        match self {
            DeviceParameters::Enum { options } => options
                .iter()
                .find(|e| e.name == name && e.value.is_i64())
                .map(|e| e.value.as_i64().expect("i64") as u32),
            _ => None,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
// No deny_unknown_fields here, because we embed via flatten
pub struct StructField {
    #[serde(rename = "fieldName")]
    pub field_name: String,

    #[serde(flatten)]
    pub field_type: DeviceParameters,

    #[serde(rename = "defaultValue")]
    pub default_value: Option<JsonValue>,

    #[serde(default)]
    pub required: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct ElementRange {
    pub min: u32,
    pub max: u32,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct ArraySize {
    pub min: u32,
    pub max: u32,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct IntegerRange {
    pub min: u32,
    pub max: u32,
    pub precision: u32,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EnumOption {
    pub name: String,
    #[serde(default)]
    pub value: JsonValue,
    #[serde(flatten)]
    pub extras: HashMap<String, JsonValue>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct ArrayOption {
    pub value: u32,
}

pub fn from_json<T: serde::de::DeserializeOwned, S: AsRef<[u8]>>(text: S) -> anyhow::Result<T> {
    from_json_with_policy(text, should_log_sensitive_data())
}

pub(crate) fn from_json_with_policy<T: serde::de::DeserializeOwned, S: AsRef<[u8]>>(
    text: S,
    allow_sensitive: bool,
) -> anyhow::Result<T> {
    let text = text.as_ref();
    serde_json_path_to_error::from_slice(text).map_err(|error| {
        anyhow::anyhow!(
            "{}",
            describe_json_error_with_policy::<T>(&error, text, allow_sensitive)
        )
    })
}

#[derive(Deserialize)]
struct EmbeddedRequestStatus {
    #[serde(rename = "message", alias = "msg")]
    _message: String,
    #[serde(alias = "code")]
    status: u16,
}

fn embedded_http_failure_message_with_policy(
    url: &reqwest::Url,
    status: u16,
    body: &[u8],
    allow_sensitive: bool,
) -> String {
    let url = describe_request_url(url);
    let status = reqwest::StatusCode::from_u16(status)
        .map(|code| format!("code {code}"))
        .unwrap_or_else(|_| format!("status={status}"));
    format!(
        "Request to {url} failed with {status}. {}",
        describe_sensitive_body_with_policy(body, allow_sensitive)
    )
}

fn http_status_failure_message_with_policy(
    url: &reqwest::Url,
    status: reqwest::StatusCode,
    body: &[u8],
    allow_sensitive: bool,
) -> String {
    let url = describe_request_url(url);
    format!(
        "request {url} status {}: {}. {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or(""),
        describe_sensitive_body_with_policy(body, allow_sensitive)
    )
}

fn http_status_failure_with_policy(
    url: &reqwest::Url,
    status: reqwest::StatusCode,
    body: &[u8],
    allow_sensitive: bool,
) -> anyhow::Error {
    anyhow::anyhow!(http_status_failure_message_with_policy(
        url,
        status,
        body,
        allow_sensitive
    ))
}

#[derive(Error, Debug)]
#[error("Failed with status {status} {}: {content}", .status.canonical_reason().unwrap_or(""))]
pub struct HttpRequestFailed {
    status: reqwest::StatusCode,
    content: String,
}

impl HttpRequestFailed {
    #[allow(unused)]
    pub fn from_err(err: &anyhow::Error) -> Option<&Self> {
        err.root_cause().downcast_ref::<Self>()
    }
}

fn parse_json_body_with_policy<T: serde::de::DeserializeOwned>(
    url: &reqwest::Url,
    data: &[u8],
    allow_sensitive: bool,
) -> anyhow::Result<T> {
    let diagnostic_url = describe_request_url(url);

    if let Ok(status) = from_json_with_policy::<EmbeddedRequestStatus, _>(data, allow_sensitive) {
        if status.status != reqwest::StatusCode::OK.as_u16() {
            let content = embedded_http_failure_message_with_policy(
                url,
                status.status,
                data,
                allow_sensitive,
            );
            if let Ok(code) = reqwest::StatusCode::from_u16(status.status) {
                return Err(HttpRequestFailed {
                    status: code,
                    content,
                })
                .with_context(|| format!("parsing {diagnostic_url} response"));
            }

            anyhow::bail!(content);
        }
    }

    from_json_with_policy(data, allow_sensitive)
        .with_context(|| format!("parsing {diagnostic_url} response"))
}

pub async fn json_body<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> anyhow::Result<T> {
    let url = response.url().clone();
    let diagnostic_url = describe_request_url(&url);
    let data = response
        .bytes()
        .await
        .with_context(|| format!("read {diagnostic_url} response body"))?;

    parse_json_body_with_policy(&url, &data, should_log_sensitive_data())
}

pub async fn http_response_body<R: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> anyhow::Result<R> {
    let url = response.url().clone();
    let diagnostic_url = describe_request_url(&url);

    let status = response.status();
    if !status.is_success() {
        let body_bytes = response.bytes().await.with_context(|| {
            format!(
                "request {diagnostic_url} status {}: {}, and failed to read response body",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            )
        })?;

        return Err(http_status_failure_with_policy(
            &url,
            status,
            &body_bytes,
            should_log_sensitive_data(),
        ));
    }
    json_body(response).await.with_context(|| {
        format!(
            "request {diagnostic_url} status {}: {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        )
    })
}

impl GoveeApiClient {
    async fn get_request_with_json_response<T: reqwest::IntoUrl, R: serde::de::DeserializeOwned>(
        &self,
        url: T,
    ) -> anyhow::Result<R> {
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?
            .request(Method::GET, url)
            .header("Govee-API-Key", &self.key)
            .send()
            .await?;

        http_response_body(response).await
    }

    async fn request_with_json_response<
        T: reqwest::IntoUrl,
        B: serde::Serialize,
        R: serde::de::DeserializeOwned,
    >(
        &self,
        method: Method,
        url: T,
        body: &B,
    ) -> anyhow::Result<R> {
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?
            .request(method, url)
            .header("Govee-API-Key", &self.key)
            .json(body)
            .send()
            .await?;

        http_response_body(response).await
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct NumericResponse {
        value: u64,
    }

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct NestedNumericResponse {
        data: NumericResponse,
    }

    const SCENE_LIST: &str = include_str!("../test-data/scenes.json");

    #[test]
    fn get_device_scenes() {
        let resp: GetDeviceScenesResponse = from_json(&SCENE_LIST).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    const GET_DEVICE_STATE_EXAMPLE: &str = include_str!("../test-data/get_device_state.json");

    #[test]
    fn get_device_state() {
        let resp: GetDeviceStateResponse = from_json(&GET_DEVICE_STATE_EXAMPLE).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    const LIST_DEVICES_EXAMPLE: &str = include_str!("../test-data/list_devices.json");
    const LIST_DEVICES_EXAMPLE2: &str = include_str!("../test-data/list_devices_2.json");

    #[test]
    fn list_devices_issue4() {
        let resp: GetDevicesResponse =
            from_json(&include_str!("../test-data/list_devices_issue4.json")).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    #[test]
    fn list_devices_2() {
        let resp: GetDevicesResponse = from_json(&LIST_DEVICES_EXAMPLE2).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    #[test]
    fn list_devices() {
        let resp: GetDevicesResponse = from_json(&LIST_DEVICES_EXAMPLE).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    #[test]
    fn enum_repr() {
        k9::assert_equal!(
            serde_json::to_string(&DeviceType::Light).unwrap(),
            "\"devices.types.light\""
        );
        k9::assert_equal!(
            serde_json::to_string(&DeviceType::Other("something".to_string())).unwrap(),
            "\"something\""
        );
    }

    #[test]
    fn request_ids_are_unique_uuids() {
        let first = request_id();
        let second = request_id();

        assert_ne!(first, "uuid");
        assert_ne!(first, second);
        uuid::Uuid::parse_str(&first).unwrap();
        uuid::Uuid::parse_str(&second).unwrap();
    }

    #[test]
    fn from_json_error_withholds_top_level_rejected_value() {
        let body = br#"{"value":"PASSWORD_SENTINEL"}"#;
        let error = from_json_with_policy::<NumericResponse, _>(body, false)
            .expect_err("string must not deserialize as u64");
        let rendered = format!("{error:#}");

        assert!(!rendered.contains("PASSWORD_SENTINEL"), "{rendered}");
        assert!(
            rendered.contains("NumericResponse JSON Data error"),
            "{rendered}"
        );
        assert!(rendered.contains("line 1"), "{rendered}");
        assert!(
            rendered.contains(&format!("Response body withheld ({} bytes)", body.len())),
            "{rendered}"
        );
    }

    #[test]
    fn from_json_error_withholds_nested_rejected_value() {
        let body = br#"{"data":{"value":"TOKEN_SENTINEL"}}"#;
        let error = from_json_with_policy::<NestedNumericResponse, _>(body, false)
            .expect_err("nested string must not deserialize as u64");
        let rendered = format!("{error:#}");

        assert!(!rendered.contains("TOKEN_SENTINEL"), "{rendered}");
        assert!(
            rendered.contains("NestedNumericResponse JSON Data error"),
            "{rendered}"
        );
        assert!(rendered.contains("column "), "{rendered}");
    }

    #[test]
    fn authenticated_http_failure_chains_withhold_body_and_url_secrets() {
        let url = reqwest::Url::parse("https://example.com/path?token=QUERY_SENTINEL")
            .expect("test URL must parse");
        let body = br#"{"message":"BODY_SENTINEL","status":401,"token":"TOKEN_SENTINEL"}"#;

        let embedded = parse_json_body_with_policy::<NumericResponse>(&url, body, false)
            .expect_err("embedded status 401 must fail");
        let status =
            http_status_failure_with_policy(&url, reqwest::StatusCode::UNAUTHORIZED, body, false);

        for error in [&embedded, &status] {
            let rendered = format!("{error:#}");
            for secret in ["QUERY_SENTINEL", "BODY_SENTINEL", "TOKEN_SENTINEL"] {
                assert!(!rendered.contains(secret), "leaked {secret}: {rendered}");
            }
            assert!(rendered.contains("https://example.com/path"), "{rendered}");
            assert!(
                rendered.contains(&format!("Response body withheld ({} bytes)", body.len())),
                "{rendered}"
            );
        }
        let embedded = format!("{embedded:#}");
        let status = format!("{status:#}");
        assert!(
            embedded.contains("parsing https://example.com/path response"),
            "{embedded}"
        );
        assert!(embedded.contains("code 401 Unauthorized"), "{embedded}");
        assert!(status.contains("status 401: Unauthorized"), "{status}");
    }

    #[test]
    fn embedded_http_failure_retains_unknown_numeric_status() {
        let url = reqwest::Url::parse("https://example.com/path").expect("test URL must parse");
        let rendered = embedded_http_failure_message_with_policy(
            &url,
            99,
            br#"{"token":"TOKEN_SENTINEL"}"#,
            false,
        );

        assert!(rendered.contains("status=99"), "{rendered}");
        assert!(!rendered.contains("TOKEN_SENTINEL"), "{rendered}");
    }

    /// The recorded H6072 fixture carries a real `musicMode` capability, so
    /// these assert the wire shape against Govee's own schema rather than a
    /// hand-written stub.
    fn music_capability() -> DeviceCapability {
        let resp: GetDevicesResponse = from_json(&LIST_DEVICES_EXAMPLE2).unwrap();
        resp.data
            .iter()
            .find_map(|d| d.capability_by_instance("musicMode").cloned())
            .expect("fixture has a musicMode capability")
    }

    #[test]
    fn music_payload_carries_the_requested_sensitivity() {
        let cap = music_capability();
        let payload =
            build_music_mode_payload(&cap, "Rhythm", 55).expect("Rhythm is a style in the fixture");

        assert_eq!(payload["sensitivity"], 55);
        assert_eq!(payload["autoColor"], 1);
        assert!(
            !payload["musicMode"].is_null(),
            "the style must resolve to its enum value: {payload}"
        );
    }

    #[test]
    fn music_payload_defaults_match_the_previous_hardcode() {
        let cap = music_capability();
        let payload = build_music_mode_payload(&cap, "Rhythm", DEFAULT_MUSIC_SENSITIVITY)
            .expect("Rhythm is a style in the fixture");

        // Before the preference existed this call always sent 100/1. An
        // install that never touches the slider must be byte-identical.
        assert_eq!(payload["sensitivity"], 100);
        assert_eq!(payload["autoColor"], 1);
    }

    #[test]
    fn music_payload_clamps_out_of_range_sensitivity() {
        let cap = music_capability();
        let payload = build_music_mode_payload(&cap, "Rhythm", 255).unwrap();
        assert_eq!(payload["sensitivity"], 100);
    }

    #[test]
    fn music_payload_is_none_for_an_unknown_style() {
        let cap = music_capability();
        assert!(
            build_music_mode_payload(&cap, "NotAStyle", 50).is_none(),
            "an unmapped style must fall through to the scene lookup, not \
             send a malformed music_setting call"
        );
    }
}
