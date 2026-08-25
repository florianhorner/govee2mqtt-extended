use crate::hass_mqtt::climate::mqtt_set_temperature;
use crate::hass_mqtt::enumerator::{enumerate_all_entites, enumerate_entities_for_device};
use crate::hass_mqtt::humidifier::{mqtt_device_set_work_mode, mqtt_humidifier_set_target};
use crate::hass_mqtt::instance::EntityList;
use crate::hass_mqtt::number::{mqtt_number_command, MUSIC_SENSITIVITY_COMMAND_ROUTE};
use crate::hass_mqtt::select::mqtt_set_mode_scene;
use crate::lan_api::DeviceColor;
use crate::opt_env_var;
use crate::platform_api::{from_json, DeviceType};
use crate::service::device::Device as ServiceDevice;
use crate::service::state::StateHandle;
use crate::temperature::TemperatureScale;
use anyhow::Context;
use async_channel::Receiver;
use mosquitto_rs::router::{MqttRouter, Params, Payload, State};
use mosquitto_rs::{Client, Event, QoS};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

const HASS_REGISTER_DELAY: tokio::time::Duration = tokio::time::Duration::from_secs(15);
const MUSIC_PALETTE_ENV_VAR: &str = "GOVEE_MUSIC_PALETTE";

#[derive(clap::Parser, Debug)]
pub struct HassArguments {
    /// The mqtt broker hostname or address.
    /// You may also set this via the GOVEE_MQTT_HOST environment variable.
    #[arg(long, global = true)]
    mqtt_host: Option<String>,

    /// The mqtt broker port
    /// You may also set this via the GOVEE_MQTT_PORT environment variable.
    /// If unspecified, uses 1883
    #[arg(long, global = true)]
    mqtt_port: Option<u16>,

    /// The username to authenticate against the broker
    /// You may also set this via the GOVEE_MQTT_USER environment variable.
    #[arg(long, global = true)]
    mqtt_username: Option<String>,

    /// The password to authenticate against the broker
    /// You may also set this via the GOVEE_MQTT_PASSWORD environment variable.
    #[arg(long, global = true)]
    mqtt_password: Option<String>,

    #[arg(long, global = true)]
    mqtt_bind_address: Option<String>,

    #[arg(long, global = true, default_value = "homeassistant")]
    hass_discovery_prefix: String,

    /// The temperature scale to use when showing temperature values as
    /// entities in home assistant. Can be either "C" or "F" for Celsius
    /// or Fahrenheit respectively.
    /// You may also set this via the GOVEE_TEMPERATURE_SCALE environment
    /// variable.
    #[arg(long, global = true)]
    temperature_scale: Option<String>,
}

impl HassArguments {
    pub fn opt_mqtt_host(&self) -> anyhow::Result<Option<String>> {
        match &self.mqtt_host {
            Some(h) => Ok(Some(h.to_string())),
            None => opt_env_var("GOVEE_MQTT_HOST"),
        }
    }

    pub fn mqtt_host(&self) -> anyhow::Result<String> {
        self.opt_mqtt_host()?.ok_or_else(|| {
            anyhow::anyhow!(
                "Please specify the mqtt broker either via the \
                --mqtt-host parameter or by setting $GOVEE_MQTT_HOST"
            )
        })
    }

    pub fn mqtt_port(&self) -> anyhow::Result<u16> {
        match self.mqtt_port {
            Some(p) => Ok(p),
            None => Ok(opt_env_var("GOVEE_MQTT_PORT")?.unwrap_or(1883)),
        }
    }

    pub fn mqtt_username(&self) -> anyhow::Result<Option<String>> {
        match self.mqtt_username.clone() {
            Some(u) => Ok(Some(u)),
            None => opt_env_var("GOVEE_MQTT_USER"),
        }
    }

    pub fn mqtt_password(&self) -> anyhow::Result<Option<String>> {
        match self.mqtt_password.clone() {
            Some(u) => Ok(Some(u)),
            None => opt_env_var("GOVEE_MQTT_PASSWORD"),
        }
    }

    pub fn temperature_scale(&self) -> anyhow::Result<TemperatureScale> {
        match &self.temperature_scale {
            Some(s) => Ok(s.parse()?),
            None => {
                Ok(opt_env_var("GOVEE_TEMPERATURE_SCALE")?.unwrap_or(TemperatureScale::Celsius))
            }
        }
    }
}

#[derive(Clone)]
pub struct HassClient {
    client: Client,
}

impl HassClient {
    async fn register_with_hass(&self, state: &StateHandle) -> anyhow::Result<()> {
        let entities = enumerate_all_entites(state).await?;

        // Register the configs
        log::trace!("register_with_hass: register entities");
        entities.publish_config(state, self).await?;

        // Allow hass extra time to register the entities before
        // we mark them as available
        let delay = tokio::time::Duration::from_millis((10 * entities.len()) as u64);
        log::info!(
            "Wait {delay:?} for hass to settle on {} entity configs",
            entities.len()
        );
        tokio::time::sleep(delay).await;

        // Mark as available
        log::trace!("register_with_hass: mark as online");
        self.publish(availability_topic(), "online")
            .await
            .context("online -> availability_topic")?;

        // report initial state
        log::trace!("register_with_hass: reporting state");
        entities.notify_state(self).await.context("notify_state")?;

        log::trace!("register_with_hass: done");

        Ok(())
    }

    pub async fn publish<T: AsRef<str> + std::fmt::Display, P: AsRef<[u8]> + std::fmt::Display>(
        &self,
        topic: T,
        payload: P,
    ) -> anyhow::Result<()> {
        log::trace!("{topic} -> {payload}");
        self.client
            .publish(topic, payload, QoS::AtMostOnce, false)
            .await?;
        Ok(())
    }

    pub async fn publish_obj<T: AsRef<str> + std::fmt::Display, P: Serialize>(
        &self,
        topic: T,
        payload: P,
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_string(&payload)?;
        log::trace!("{topic} -> {payload}");
        self.client
            .publish(topic, payload, QoS::AtMostOnce, false)
            .await?;
        Ok(())
    }

    pub async fn publish_obj_retained<T: AsRef<str> + std::fmt::Display, P: Serialize>(
        &self,
        topic: T,
        payload: P,
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_string(&payload)?;
        log::trace!("{topic} -> {payload} (retained)");
        self.client
            .publish(topic, payload, QoS::AtMostOnce, true)
            .await?;
        Ok(())
    }

    pub async fn advise_hass_of_light_state(
        &self,
        device: &ServiceDevice,
        state: &StateHandle,
    ) -> anyhow::Result<()> {
        let mut entities = EntityList::new();
        enumerate_entities_for_device(device, state, &mut entities).await?;
        entities.notify_state(self).await?;

        Ok(())
    }
}

pub fn topic_safe_string(s: &str) -> String {
    let mut result = String::new();
    for c in s.chars() {
        if c == ':' || c == ' ' || c == '\\' || c == '/' || c == '\'' || c == '"' {
            result.push('_');
        } else {
            result.push(c.to_ascii_lowercase());
        }
    }
    result
}

pub fn topic_safe_id(device: &ServiceDevice) -> String {
    let mut id = device.id.to_string();
    id.retain(|c| c != ':');
    id.retain(|c| c != ' ');
    id
}

pub fn switch_instance_state_topic(device: &ServiceDevice, instance: &str) -> String {
    format!(
        "gv2mqtt/switch/{id}/{instance}/state",
        id = topic_safe_id(device)
    )
}

pub fn light_state_topic(device: &ServiceDevice) -> String {
    format!("gv2mqtt/light/{id}/state", id = topic_safe_id(device))
}

pub fn light_segment_state_topic(device: &ServiceDevice, segment: u32) -> String {
    format!(
        "gv2mqtt/light/{id}/state/{segment}",
        id = topic_safe_id(device)
    )
}

/// All entities use the same topic so that we can mark unavailable
/// via last-will
pub fn availability_topic() -> String {
    "gv2mqtt/availability".to_string()
}

pub fn oneclick_topic() -> String {
    "gv2mqtt/oneclick".to_string()
}

pub fn purge_cache_topic() -> String {
    "gv2mqtt/purge-caches".to_string()
}

#[derive(Deserialize)]
pub struct IdParameter {
    pub id: String,
}

#[derive(Deserialize)]
struct MusicPaletteCommand {
    style: String,
    colors: Vec<String>,
    #[serde(default = "default_music_sensitivity")]
    sensitivity: u8,
}

fn default_music_sensitivity() -> u8 {
    crate::platform_api::DEFAULT_MUSIC_SENSITIVITY
}

fn music_palette_enabled(value: Option<&str>) -> bool {
    value == Some("true")
}

/// `gv2mqtt/<id>/set-music-palette` with a JSON payload like
/// `{"style": "Rhythm", "colors": ["#0000ff", "#ff0000"], "sensitivity": 99}`.
///
/// Programs music mode with a caller-chosen palette over LAN, for the SKUs
/// mapped in `src/music.rs`. Opt-in: the topic only acts when
/// `GOVEE_MUSIC_PALETTE=true` is set. Documented in docs/MUSIC_MODE.md.
async fn mqtt_set_music_palette(
    Payload(payload): Payload<String>,
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let configured = std::env::var(MUSIC_PALETTE_ENV_VAR).ok();
    let enabled = music_palette_enabled(configured.as_deref());
    anyhow::ensure!(
        enabled,
        "set-music-palette is opt-in: set GOVEE_MUSIC_PALETTE=true to enable it"
    );

    let command: MusicPaletteCommand = serde_json::from_str(&payload)
        .with_context(|| format!("parsing set-music-palette payload {payload:?}"))?;

    let device = state.resolve_device_for_control(&id).await?;

    let profile = crate::music::music_profile(&device.sku, &command.style).ok_or_else(|| {
        match crate::music::music_styles(&device.sku) {
            Some(styles) => anyhow::anyhow!(
                "style {:?} is not mapped for {}; mapped styles: {}",
                command.style,
                device.sku,
                styles.join(", ")
            ),
            None => anyhow::anyhow!(
                "{} has no music profile table entry yet; \
                 docs/MUSIC_MODE.md describes how to map a new SKU",
                device.sku
            ),
        }
    })?;

    let colors = command
        .colors
        .iter()
        .map(|color| crate::music::parse_hex_color(color))
        .collect::<anyhow::Result<Vec<_>>>()?;

    state
        .device_set_music_palette(
            &device,
            &crate::ble::SetMusicPalette {
                profile,
                colors,
                sensitivity: command.sensitivity,
            },
        )
        .await
}

/// Someone pressed the "Scene Next" button
async fn mqtt_scene_next(
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    scene_cycle(&state, &id, 1).await
}

/// Someone pressed the "Scene Previous" button
async fn mqtt_scene_prev(
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    scene_cycle(&state, &id, -1).await
}

/// Computes the target scene index for cycling.
/// Returns the index into `scenes` to activate.
fn compute_scene_cycle_index(
    scenes: &[String],
    current_name: Option<&str>,
    direction: i32,
) -> usize {
    let total = scenes.len() as i32;
    match current_name.and_then(|name| scenes.iter().position(|n| n.eq_ignore_ascii_case(name))) {
        Some(idx) => ((idx as i32 + direction).rem_euclid(total)) as usize,
        None => {
            if direction > 0 {
                0
            } else {
                (total - 1) as usize
            }
        }
    }
}

/// Shared logic for scene next/prev cycling
async fn scene_cycle(state: &StateHandle, id: &str, direction: i32) -> anyhow::Result<()> {
    // Acquire Coordinator first to prevent races with concurrent scene changes
    let coord = state.resolve_device_for_control(id).await?;

    let catalog = state.device_list_scenes_categorized(&coord).await?;
    let flat: Vec<String> = catalog
        .into_iter()
        .flat_map(|cat| cat.scenes.into_iter().map(|s| s.name))
        .collect();

    if flat.is_empty() {
        anyhow::bail!("No scenes available for device {id}");
    }

    let current_name = coord.active_scene_name().map(|s| s.to_string());
    let new_idx = compute_scene_cycle_index(&flat, current_name.as_deref(), direction);

    let target_scene = &flat[new_idx];

    log::info!(
        "Scene cycle {}: {} -> {} (index {} of {})",
        if direction > 0 { "next" } else { "prev" },
        current_name.as_deref().unwrap_or("None"),
        target_scene,
        new_idx,
        flat.len()
    );

    state.device_set_scene(&coord, target_scene).await?;

    Ok(())
}

/// Someone clicked the "Request Platform API State" button
async fn mqtt_request_platform_data(
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_read_only(&id).await?;
    log::info!("Request Platform API State for {device}");
    if !state.poll_platform_api(&device).await? {
        log::warn!("Unable to poll platform API for {device}");
    }
    Ok(())
}

#[derive(Deserialize, Debug, Clone)]
struct HassLightCommand {
    state: String,
    color_temp: Option<u32>,
    color: Option<DeviceColor>,
    effect: Option<String>,
    brightness: Option<u8>,
}

/// HASS is sending a command to a light
async fn mqtt_light_command(
    Payload(payload): Payload<String>,
    Params(IdParameter { id }): Params<IdParameter>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;

    let command: HassLightCommand = serde_json::from_str(&payload)?;
    log::info!("Command for {device}: {payload}");

    let is_light = device.device_type() == DeviceType::Light;

    if command.state == "OFF" {
        if is_light {
            state
                .device_light_power_on(&device, false)
                .await
                .context("mqtt_light_command: state.device_power_on")?;
        } else {
            state
                .device_set_brightness(&device, 0)
                .await
                .context("mqtt_light_command: state.device_set_brightness")?;
        }
    } else {
        let mut power_on = true;

        if let Some(brightness) = command.brightness {
            state
                .device_set_brightness(&device, brightness)
                .await
                .context("mqtt_light_command: state.device_set_brightness")?;
            power_on = false;
        }

        if let Some(effect) = &command.effect {
            state
                .device_set_scene_with_music_color(&device, effect, command.color)
                .await
                .context("mqtt_light_command: state.device_set_scene")?;
            // A Music effect can carry one RGB colour in the same Platform API
            // struct; its presence disables autoColor. Ordinary scenes still
            // ignore colour and temperature, as before. Brightness is okay.
            return Ok(());
        }

        if let Some(color) = &command.color {
            state
                .device_set_color_rgb(&device, color.r, color.g, color.b)
                .await
                .context("mqtt_light_command: state.device_set_color_rgb")?;
            power_on = false;
        }
        if let Some(color_temp) = command.color_temp {
            state
                .device_set_color_temperature(&device, mired_to_kelvin(color_temp))
                .await
                .context("mqtt_light_command: state.device_set_color_temperature")?;
            power_on = false;
        }

        if power_on {
            if is_light {
                state
                    .device_light_power_on(&device, true)
                    .await
                    .context("mqtt_light_command: state.device_power_on")?;
            } else if command.brightness.is_none() {
                // The device is not primarily a light and we don't have
                // a guaranteed way to power it on without setting the
                // brightness to something, and we know we didn't set
                // the brightness just now, so let's turn it on 100%
                state
                    .device_set_brightness(&device, 100)
                    .await
                    .context("mqtt_light_command: state.device_set_brightness")?;
            }
        }
    }

    Ok(())
}

#[derive(Deserialize)]
struct IdAndSeg {
    id: String,
    segment: String,
}

async fn mqtt_light_segment_command(
    Payload(payload): Payload<String>,
    Params(IdAndSeg { id, segment }): Params<IdAndSeg>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let device = state.resolve_device_for_control(&id).await?;
    let segment: u32 = segment.parse()?;

    let command: HassLightCommand = from_json(&payload)?;
    log::info!("Command for {device} segment {segment}: {payload}");

    if let Some(client) = state.get_platform_client().await {
        let info = device
            .http_device_info
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("HTTP device info is missing"))?;

        log::info!("Using Platform API to control {device} segment");

        if let Some(brightness) = command.brightness {
            client
                .set_segment_brightness(info, segment, brightness)
                .await?;
        } else if command.state == "OFF" {
            // Do nothing here. We used to set brightness to zero,
            // but it is problematic:
            // * Some devices don't have a 0
            // * Setting it to 0 will power up the rest of the device,
            //   so if HASS is turning off all lights in an area, the
            //   effect is that they will turn off and then immediate
            //   on again when there are segments involved
            // client.set_segment_brightness(&info, segment, 0).await?;
        }
        if let Some(color) = &command.color {
            client
                .set_segment_rgb(info, segment, color.r, color.g, color.b)
                .await?;
            // A solid per-segment color ends any scene the bridge had applied.
            state
                .device_mut(&device.sku, &device.id)
                .await
                .set_active_scene(None);
        }
    } else {
        anyhow::bail!("set segments for {device}: Platform API is not available");
    }

    Ok(())
}

async fn mqtt_purge_caches(State(state): State<StateHandle>) -> anyhow::Result<()> {
    log::info!("mqtt_purge_caches");
    crate::cache::purge_cache()?;
    state.clear_scene_catalogs().await;
    state
        .get_hass_client()
        .await
        .expect("have hass client")
        .register_with_hass(&state)
        .await
        .context("register_with_hass")
}

async fn mqtt_oneclick(
    Payload(name): Payload<String>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    log::info!("mqtt_oneclick: {name}");

    let undoc = state.get_undoc_client().await.ok_or_else(|| {
        anyhow::anyhow!("Govee cloud API unavailable: is your email/password configured?")
    })?;
    let items = undoc.parse_one_clicks().await?;
    let item = items
        .iter()
        .find(|item| item.name == name)
        .ok_or_else(|| anyhow::anyhow!("didn't find item {name}"))?;

    let iot = state
        .get_iot_client()
        .await
        .ok_or_else(|| anyhow::anyhow!("AWS IoT client is not available"))?;

    iot.activate_one_click(item).await
}

#[derive(Deserialize)]
struct IdAndInst {
    id: String,
    instance: String,
}

async fn mqtt_switch_command(
    Payload(command): Payload<String>,
    Params(IdAndInst { id, instance }): Params<IdAndInst>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    log::info!("{instance} for {id}: {command}");
    let device = state.resolve_device_for_control(&id).await?;

    let on = match command.as_str() {
        "ON" | "on" => true,
        "OFF" | "off" => false,
        _ => anyhow::bail!("invalid {command} for {id}"),
    };

    if instance == "powerSwitch" {
        state.device_power_on(&device, on).await?;
    } else if let Some(client) = state.get_platform_client().await {
        if let Some(http_dev) = &device.http_device_info {
            client.set_toggle_state(http_dev, &instance, on).await?;
        } else {
            anyhow::bail!("No platform state available to set {id} {instance} to {on}");
        }
    } else {
        anyhow::bail!("Unsupported command '{command}' for {id} {instance}");
    }

    Ok(())
}

pub fn mired_to_kelvin(mired: u32) -> u32 {
    1000000u32.checked_div(mired).unwrap_or(0)
}

pub fn kelvin_to_mired(kelvin: u32) -> u32 {
    1000000u32.checked_div(kelvin).unwrap_or(0)
}

/// HASS is advising us that its status has changed
async fn mqtt_homeassitant_status(
    Payload(status): Payload<String>,
    State(state): State<StateHandle>,
) -> anyhow::Result<()> {
    let client = state
        .get_hass_client()
        .await
        .expect("hass client to be present");

    log::info!("Home Assistant status changed: {status}, waiting {HASS_REGISTER_DELAY:?} before re-registering entities");
    tokio::time::sleep(HASS_REGISTER_DELAY).await;

    client.register_with_hass(&state).await?;

    Ok(())
}

async fn run_mqtt_loop(
    state: StateHandle,
    subscriber: Receiver<Event>,
    client: Client,
) -> anyhow::Result<()> {
    // Give LAN disco a chance to get current state before
    // we register with hass
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    async fn rebuild_router(
        client: &Client,
        state: &StateHandle,
    ) -> anyhow::Result<Arc<MqttRouter<StateHandle>>> {
        let disco_prefix = state.get_hass_disco_prefix().await;
        let mut router: MqttRouter<StateHandle> = MqttRouter::new(client.clone());

        router
            .route(format!("{disco_prefix}/status"), mqtt_homeassitant_status)
            .await?;

        router
            .route("gv2mqtt/light/:id/command", mqtt_light_command)
            .await?;
        router
            .route(
                "gv2mqtt/light/:id/command/:segment",
                mqtt_light_segment_command,
            )
            .await?;
        router
            .route("gv2mqtt/switch/:id/command/:instance", mqtt_switch_command)
            .await?;

        router.route(oneclick_topic(), mqtt_oneclick).await?;
        router.route(purge_cache_topic(), mqtt_purge_caches).await?;
        router
            .route(
                "gv2mqtt/:id/request-platform-data",
                mqtt_request_platform_data,
            )
            .await?;
        router
            .route("gv2mqtt/:id/scene-next", mqtt_scene_next)
            .await?;
        router
            .route("gv2mqtt/:id/scene-prev", mqtt_scene_prev)
            .await?;
        router
            .route(
                "gv2mqtt/number/:id/command/:mode_name/:work_mode",
                mqtt_number_command,
            )
            .await?;
        router
            .route("gv2mqtt/humidifier/:id/set-mode", mqtt_device_set_work_mode)
            .await?;
        router
            .route("gv2mqtt/:id/set-work-mode", mqtt_device_set_work_mode)
            .await?;
        router
            .route(
                MUSIC_SENSITIVITY_COMMAND_ROUTE,
                crate::hass_mqtt::number::mqtt_music_sensitivity_command,
            )
            .await?;
        router
            .route(
                "gv2mqtt/humidifier/:id/set-target",
                mqtt_humidifier_set_target,
            )
            .await?;
        router
            .route(
                "gv2mqtt/:id/set-temperature/:instance/:units",
                mqtt_set_temperature,
            )
            .await?;
        router
            .route("gv2mqtt/:id/set-mode-scene", mqtt_set_mode_scene)
            .await?;
        router
            .route("gv2mqtt/:id/set-music-palette", mqtt_set_music_palette)
            .await?;

        tokio::time::sleep(HASS_REGISTER_DELAY).await;
        state
            .get_hass_client()
            .await
            .expect("have hass client")
            .register_with_hass(state)
            .await
            .context("register_with_hass")?;

        Ok(Arc::new(router))
    }

    let mut router = rebuild_router(&client, &state).await?;
    let mut need_rebuild = false;

    while let Ok(event) = subscriber.recv().await {
        match event {
            Event::Message(msg) => {
                let router = router.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = router.dispatch(msg.clone(), state.clone()).await {
                        log::error!("While dispatching {msg:?}: {err:#}");
                    }
                });
            }
            Event::Disconnected(reason) => {
                log::warn!("MQTT disconnected with reason={reason}");
                need_rebuild = true;
            }
            Event::Connected(status) => {
                log::info!("MQTT connected with status={status}");
                if need_rebuild {
                    router = rebuild_router(&client, &state).await?;
                }
            }
        }
    }

    log::info!("subscriber.recv loop terminated");

    Ok(())
}

pub async fn spawn_hass_integration(
    state: StateHandle,
    args: &HassArguments,
) -> anyhow::Result<()> {
    let client = Client::with_id(
        &format!("govee2mqtt/{}", uuid::Uuid::new_v4().simple()),
        true,
    )?;

    state.set_temperature_scale(args.temperature_scale()?).await;

    let mqtt_host = args.mqtt_host()?;
    let mqtt_username = args.mqtt_username()?;
    let mqtt_password = args.mqtt_password()?;
    let mqtt_port = args.mqtt_port()?;

    client.set_last_will(availability_topic(), "offline", QoS::AtMostOnce, false)?;

    if mqtt_username.is_some() != mqtt_password.is_some() {
        log::error!(
            "MQTT username and password either both need to be set, or both need to be unset"
        );
    }
    client.set_username_and_password(mqtt_username.as_deref(), mqtt_password.as_deref())?;

    let mut connected = false;
    for _ in 0..30 {
        log::info!("Attempting connection to mqtt broker {mqtt_host}:{mqtt_port}...");
        match client
            .connect(
                &mqtt_host,
                mqtt_port.into(),
                Duration::from_secs(120),
                args.mqtt_bind_address.as_deref(),
            )
            .await
        {
            Ok(status) => {
                log::info!("Connected to mqtt broker {mqtt_host}:{mqtt_port}, status={status}");
                connected = true;
                break;
            }
            Err(err) => {
                log::error!("Failed to connect to mqtt broker {mqtt_host}:{mqtt_port}: {err:#}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }

    anyhow::ensure!(
        connected,
        "Failed to connect to mqtt broker after several attempts"
    );

    let subscriber = client.subscriber().expect("to own the subscriber");

    state
        .set_hass_client(HassClient {
            client: client.clone(),
        })
        .await;

    let disco_prefix = args.hass_discovery_prefix.clone();
    state.set_hass_disco_prefix(disco_prefix).await;

    tokio::spawn(async move {
        let res = run_mqtt_loop(state, subscriber, client).await;
        if let Err(err) = res {
            log::error!("run_mqtt_loop: {err:#}");
            log::error!("FATAL: hass integration will not function.");
            log::error!("Pausing for 30 seconds before terminating.");
            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
            std::process::exit(1);
        } else {
            log::error!(
                "run_mqtt_loop exited unexpectedly. Terminating so HA can restart the addon."
            );
            std::process::exit(1);
        }
    });

    Ok(())
}

pub fn camel_case_to_space_separated(camel: &str) -> String {
    let mut chars = camel.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut result = first.to_ascii_uppercase().to_string();
    for c in chars {
        if c.is_uppercase() {
            result.push(' ');
        }
        result.push(c);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hass_mqtt::instance::EntityInstance;
    use crate::platform_api::{
        DeviceCapability, DeviceCapabilityKind, DeviceParameters, EnumOption,
    };
    use std::collections::HashMap;

    #[test]
    fn test_camel_case_ascii() {
        assert_eq!(camel_case_to_space_separated("powerSwitch"), "Power Switch");
        assert_eq!(
            camel_case_to_space_separated("oscillationToggle"),
            "Oscillation Toggle"
        );
    }

    #[test]
    fn test_camel_case_chinese_no_panic() {
        assert_eq!(
            camel_case_to_space_separated("用于三灯头中的第二个"),
            "用于三灯头中的第二个"
        );
    }

    #[test]
    fn test_camel_case_empty() {
        assert_eq!(camel_case_to_space_separated(""), "");
    }

    #[test]
    fn test_camel_case_emoji() {
        assert_eq!(camel_case_to_space_separated("🔥lightMode"), "🔥light Mode");
    }

    #[test]
    fn test_scene_cycle_next_from_middle() {
        let scenes: Vec<String> = vec!["A".into(), "B".into(), "C".into()];
        assert_eq!(compute_scene_cycle_index(&scenes, Some("B"), 1), 2);
    }

    #[test]
    fn test_scene_cycle_prev_from_middle() {
        let scenes: Vec<String> = vec!["A".into(), "B".into(), "C".into()];
        assert_eq!(compute_scene_cycle_index(&scenes, Some("B"), -1), 0);
    }

    #[test]
    fn test_scene_cycle_next_wraps_last_to_first() {
        let scenes: Vec<String> = vec!["A".into(), "B".into(), "C".into()];
        assert_eq!(compute_scene_cycle_index(&scenes, Some("C"), 1), 0);
    }

    #[test]
    fn test_scene_cycle_prev_wraps_first_to_last() {
        let scenes: Vec<String> = vec!["A".into(), "B".into(), "C".into()];
        assert_eq!(compute_scene_cycle_index(&scenes, Some("A"), -1), 2);
    }

    #[test]
    fn test_scene_cycle_no_active_scene_next() {
        let scenes: Vec<String> = vec!["A".into(), "B".into(), "C".into()];
        assert_eq!(compute_scene_cycle_index(&scenes, None, 1), 0);
    }

    #[test]
    fn test_scene_cycle_no_active_scene_prev() {
        let scenes: Vec<String> = vec!["A".into(), "B".into(), "C".into()];
        assert_eq!(compute_scene_cycle_index(&scenes, None, -1), 2);
    }

    #[test]
    fn test_scene_cycle_case_insensitive() {
        let scenes: Vec<String> = vec!["Sunset".into(), "Rainbow".into()];
        assert_eq!(compute_scene_cycle_index(&scenes, Some("sunset"), 1), 1);
    }

    #[test]
    fn test_scene_cycle_single_scene() {
        let scenes: Vec<String> = vec!["Only".into()];
        assert_eq!(compute_scene_cycle_index(&scenes, Some("Only"), 1), 0);
        assert_eq!(compute_scene_cycle_index(&scenes, Some("Only"), -1), 0);
    }

    #[test]
    fn test_scene_cycle_unknown_scene_treated_as_no_active() {
        let scenes: Vec<String> = vec!["A".into(), "B".into()];
        assert_eq!(
            compute_scene_cycle_index(&scenes, Some("nonexistent"), 1),
            0
        );
    }

    /// The add-on option must reach the bridge under the name the bridge reads.
    /// `music_palette` is declared in three files that nothing else ties
    /// together: a rename in any one of them leaves a toggle in the Home
    /// Assistant UI that silently does nothing.
    #[test]
    fn addon_music_palette_option_is_wired_to_the_env_var_the_bridge_reads() {
        const OPTION: &str = "music_palette";
        const CONFIG: &str = include_str!("../../addon/config.yaml");
        const RUN_SH: &str = include_str!("../../addon/run.sh");
        const TRANSLATIONS: &str = include_str!("../../addon/translations/en.yaml");

        assert!(
            CONFIG.contains(&format!("{OPTION}: \"bool?\"")),
            "addon/config.yaml must declare {OPTION} as an optional bool"
        );
        assert!(
            RUN_SH.contains(&format!("bashio::config.has_value {OPTION}")),
            "addon/run.sh must read the {OPTION} option"
        );
        assert!(
            RUN_SH.contains(&format!(
                "{MUSIC_PALETTE_ENV_VAR}=\"$(bashio::config {OPTION})\""
            )),
            "addon/run.sh must read {OPTION} into {MUSIC_PALETTE_ENV_VAR}"
        );
        assert!(
            RUN_SH.contains(&format!("export {MUSIC_PALETTE_ENV_VAR}")),
            "addon/run.sh must export {MUSIC_PALETTE_ENV_VAR}"
        );
        assert!(
            TRANSLATIONS.contains(&format!("{OPTION}:")),
            "addon/translations/en.yaml must label {OPTION}, or the add-on \
             config page shows a bare key"
        );
    }

    /// `bashio::config.has_value` is true for a boolean `false`, so switching
    /// the add-on toggle OFF exports `GOVEE_MUSIC_PALETTE=false` rather than
    /// leaving the variable unset. Anything but the exact string "true" has to
    /// keep the reverse-engineered LAN palette path disabled.
    #[test]
    fn music_palette_topic_requires_the_env_var_to_be_exactly_true() {
        for off in ["false", "False", "TRUE", "1", "yes", ""] {
            assert!(
                !music_palette_enabled(Some(off)),
                "{off:?} must not enable the topic"
            );
        }
        assert!(!music_palette_enabled(None));
        assert!(music_palette_enabled(Some("true")));
    }

    #[tokio::test]
    async fn music_effect_with_rgb_disables_auto_color_on_the_wire() {
        use crate::platform_api::test::{capture_server, live_path_guard, music_device};

        let _serialized = live_path_guard();
        let (base_url, captured) = capture_server();
        let info = music_device();
        let state: StateHandle = Arc::new(crate::service::state::State::new());
        state
            .set_platform_client(crate::platform_api::GoveeApiClient::new_for_test(
                "test-key", base_url,
            ))
            .await;
        {
            let mut device = state.device_mut(&info.sku, &info.device).await;
            device.set_http_device_info(info.clone());
            device.set_music_sensitivity(42);
        }

        let before = captured.lock().unwrap_or_else(|e| e.into_inner()).len();
        mqtt_light_command(
            Payload(
                serde_json::json!({
                    "state": "ON",
                    "effect": "Music: Rhythm",
                    "color": {"r": 0x12, "g": 0x34, "b": 0x56},
                })
                .to_string(),
            ),
            Params(IdParameter {
                id: info.device.clone(),
            }),
            State(state),
        )
        .await
        .expect("the HA light command reaches the capture server");

        let sent = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(sent.len(), before + 1, "exactly one request");
        let value = &sent[before]["payload"]["capability"]["value"];
        assert_eq!(value["sensitivity"], 42);
        assert_eq!(value["autoColor"], 0);
        assert_eq!(value["rgb"], 0x123456);
    }

    #[tokio::test]
    async fn ordinary_effect_still_ignores_rgb_in_the_same_command() {
        use crate::platform_api::test::{capture_server, live_path_guard};

        let _serialized = live_path_guard();
        let (base_url, captured) = capture_server();
        let info = crate::platform_api::HttpDeviceInfo {
            sku: "H9999".to_string(),
            device: "AA:BB:CC:DD:EE:FF:11:22".to_string(),
            device_name: "Ordinary Scene Test".to_string(),
            device_type: DeviceType::Light,
            capabilities: vec![DeviceCapability {
                kind: DeviceCapabilityKind::Mode,
                instance: "lightScene".to_string(),
                parameters: Some(DeviceParameters::Enum {
                    options: vec![EnumOption {
                        name: "Aurora".to_string(),
                        value: serde_json::json!(7),
                        extras: HashMap::new(),
                    }],
                }),
                alarm_type: None,
                event_state: None,
            }],
        };

        let state: StateHandle = Arc::new(crate::service::state::State::new());
        state
            .set_platform_client(crate::platform_api::GoveeApiClient::new_for_test(
                "test-key", base_url,
            ))
            .await;
        {
            state
                .device_mut(&info.sku, &info.device)
                .await
                .set_http_device_info(info.clone());
        }

        let before = captured.lock().unwrap_or_else(|e| e.into_inner()).len();
        mqtt_light_command(
            Payload(
                serde_json::json!({
                    "state": "ON",
                    "effect": "Aurora",
                    "color": {"r": 0x12, "g": 0x34, "b": 0x56},
                })
                .to_string(),
            ),
            Params(IdParameter {
                id: info.device.clone(),
            }),
            State(state),
        )
        .await
        .expect("the ordinary effect reaches the capture server");

        let sent = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(sent.len(), before + 1, "exactly one request");
        let capability = &sent[before]["payload"]["capability"];
        assert_eq!(capability["instance"], "lightScene");
        assert_eq!(capability["value"], 7);
        assert!(capability["value"].get("rgb").is_none());
        assert!(capability["value"].get("autoColor").is_none());
        assert!(capability["value"].get("sensitivity").is_none());
    }

    /// A device that vanished from the state map between discovery and the
    /// state sweep must be skipped, not turned into an error that aborts the
    /// whole `EntityList::notify_state` loop for every other entity.
    #[tokio::test]
    async fn music_sensitivity_notify_skips_a_device_that_left_the_state_map() {
        let state: StateHandle = Arc::new(crate::service::state::State::new());
        let device = ServiceDevice::new("H607C", "AA:BB:CC:DD:EE:FF:11:22");
        let entity = crate::hass_mqtt::number::MusicSensitivityNumber::new(&device, &state);
        let client = HassClient {
            client: Client::with_auto_id().expect("mosquitto client"),
        };

        assert!(state.devices().await.is_empty());
        tokio::time::timeout(Duration::from_secs(5), entity.notify_state(&client))
            .await
            .expect("notify must not block")
            .expect("an absent device is skipped, not an error");
    }
}
