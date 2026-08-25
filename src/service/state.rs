use crate::ble::{
    Base64HexBytes, SetHumidifierMode, SetHumidifierNightlightParams, SetMusicPalette,
};
use crate::lan_api::{Client as LanClient, DeviceStatus as LanDeviceStatus, LanDevice};
use crate::platform_api::{DeviceCapability, DeviceType, GoveeApiClient};
use crate::service::coordinator::Coordinator;
use crate::service::device::Device;
use crate::service::hass::{topic_safe_id, HassClient};
use crate::service::iot::IotClient;
use crate::temperature::{TemperatureScale, TemperatureValue};
use crate::undoc_api::GoveeUndocumentedApi;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{MappedMutexGuard, Mutex, MutexGuard, Semaphore};
use tokio::time::{sleep, Duration};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SceneCatalogCategory {
    pub name: String,
    pub scenes: Vec<SceneCatalogEntry>,
}

#[derive(Clone, Debug)]
pub struct SceneCatalogCache {
    /// Platform-capability fingerprint at fetch time. Used to detect when platform
    /// device metadata has arrived/changed so a cached catalog can be refreshed.
    pub platform_signature: Option<String>,
    pub categories: Vec<SceneCatalogCategory>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SceneCatalogEntry {
    pub name: String,
    pub icon_urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Default)]
pub struct State {
    devices_by_id: Mutex<HashMap<String, Device>>,
    semaphore_by_id: Mutex<HashMap<String, Arc<Semaphore>>>,
    lan_client: Mutex<Option<LanClient>>,
    platform_client: Mutex<Option<GoveeApiClient>>,
    undoc_client: Mutex<Option<GoveeUndocumentedApi>>,
    iot_client: Mutex<Option<IotClient>>,
    hass_client: Mutex<Option<HassClient>>,
    hass_discovery_prefix: Mutex<String>,
    temperature_scale: Mutex<TemperatureScale>,
}

pub type StateHandle = Arc<State>;

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn set_temperature_scale(&self, scale: TemperatureScale) {
        *self.temperature_scale.lock().await = scale;
    }

    pub async fn get_temperature_scale(&self) -> TemperatureScale {
        *self.temperature_scale.lock().await
    }

    pub async fn set_hass_disco_prefix(&self, prefix: String) {
        *self.hass_discovery_prefix.lock().await = prefix;
    }

    pub async fn get_hass_disco_prefix(&self) -> String {
        self.hass_discovery_prefix.lock().await.to_string()
    }

    /// Returns a mutable version of the specified device, creating
    /// an entry for it if necessary.
    pub async fn device_mut(&self, sku: &str, id: &str) -> MappedMutexGuard<'_, Device> {
        let devices = self.devices_by_id.lock().await;
        MutexGuard::map(devices, |devices| {
            devices
                .entry(id.to_string())
                .or_insert_with(|| Device::new(sku, id))
        })
    }

    pub async fn devices(&self) -> Vec<Device> {
        self.devices_by_id.lock().await.values().cloned().collect()
    }

    /// Returns an immutable copy of the specified Device
    pub async fn device_by_id(&self, id: &str) -> Option<Device> {
        let devices = self.devices_by_id.lock().await;
        devices.get(id).cloned()
    }

    async fn semaphore_for_device(&self, device: &Device) -> Arc<Semaphore> {
        self.semaphore_by_id
            .lock()
            .await
            .entry(device.id.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone()
    }

    pub async fn resolve_device_read_only(self: &Arc<Self>, label: &str) -> anyhow::Result<Device> {
        self.resolve_device(label)
            .await
            .ok_or_else(|| anyhow::anyhow!("device '{label}' not found"))
    }

    /// Resolve a device based on its label.
    /// Assuming the device is found, returns a Coordinator, which is a
    /// struct that ensures that only one task at a time can be processing
    /// control requests for a device.
    /// This method will not return until the calling task is permitted
    /// to proceed with its control attempt.
    pub async fn resolve_device_for_control(
        self: &Arc<Self>,
        label: &str,
    ) -> anyhow::Result<Coordinator> {
        let device = self
            .resolve_device(label)
            .await
            .ok_or_else(|| anyhow::anyhow!("device '{label}' not found"))?;
        let semaphore = self.semaphore_for_device(&device).await;
        let permit = semaphore.acquire_owned().await?;
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Schedule a task that will poll the device a short
        // time after the Coordinator is dropped, to reconcile
        // any changed state
        let state = self.clone();
        let device_id = device.id.to_string();
        tokio::spawn(async move {
            let _ = rx.await;
            state.poll_after_control(device_id).await
        });

        Ok(Coordinator::new(device, permit, tx))
    }

    /// Resolve a device using its name, computed name, id or label,
    /// ignoring case.
    pub async fn resolve_device(&self, label: &str) -> Option<Device> {
        let devices = self.devices_by_id.lock().await;

        // Try by id first
        if let Some(device) = devices.get(label) {
            return Some(device.clone());
        }

        for d in devices.values() {
            if d.name().eq_ignore_ascii_case(label)
                || d.id.eq_ignore_ascii_case(label)
                || topic_safe_id(d).eq_ignore_ascii_case(label)
                || d.ip_addr()
                    .map(|ip| ip.to_string().eq_ignore_ascii_case(label))
                    .unwrap_or(false)
                || d.computed_name().eq_ignore_ascii_case(label)
            {
                return Some(d.clone());
            }
        }

        None
    }

    pub async fn set_hass_client(&self, client: HassClient) {
        self.hass_client.lock().await.replace(client);
    }

    pub async fn get_hass_client(&self) -> Option<HassClient> {
        self.hass_client.lock().await.clone()
    }

    pub async fn set_iot_client(&self, client: IotClient) {
        self.iot_client.lock().await.replace(client);
    }

    pub async fn get_iot_client(&self) -> Option<IotClient> {
        self.iot_client.lock().await.clone()
    }

    pub async fn set_lan_client(&self, client: LanClient) {
        self.lan_client.lock().await.replace(client);
    }

    pub async fn get_lan_client(&self) -> Option<LanClient> {
        self.lan_client.lock().await.clone()
    }

    pub async fn set_platform_client(&self, client: GoveeApiClient) {
        self.platform_client.lock().await.replace(client);
    }

    pub async fn get_platform_client(&self) -> Option<GoveeApiClient> {
        self.platform_client.lock().await.clone()
    }

    pub async fn set_undoc_client(&self, client: GoveeUndocumentedApi) {
        self.undoc_client.lock().await.replace(client);
    }

    #[allow(dead_code)]
    pub async fn get_undoc_client(&self) -> Option<GoveeUndocumentedApi> {
        self.undoc_client.lock().await.clone()
    }

    pub async fn poll_iot_api(self: &Arc<Self>, device: &Device) -> anyhow::Result<bool> {
        if let Some(iot) = self.get_iot_client().await {
            if let Some(info) = device.undoc_device_info.clone() {
                if iot.is_device_compatible(&info.entry) {
                    let device_state = device.device_state();
                    log::info!("requesting update via IoT MQTT {device} {device_state:?}");
                    match iot
                        .request_status_update(&info.entry)
                        .await
                        .context("iot.request_status_update")
                    {
                        Err(err) => {
                            log::error!("Failed: {err:#}");
                        }
                        Ok(()) => {
                            // The response will come in async via the mqtt loop in iot.rs
                            // However, if the device is offline, nothing will change our state.
                            // Let's explicitly mark the device as having been polled so that
                            // we don't keep sending a request every minute.
                            self.device_mut(&device.sku, &device.id)
                                .await
                                .set_last_polled();

                            return Ok(true);
                        }
                    }
                }
            }
        }
        Ok(false)
    }

    pub async fn poll_platform_api(self: &Arc<Self>, device: &Device) -> anyhow::Result<bool> {
        if let Some(client) = self.get_platform_client().await {
            if let DeviceType::Other(other) = &device.device_type() {
                // Cannot poll an unknown device
                // <https://github.com/wez/govee2mqtt/issues/391>
                // <https://github.com/wez/govee2mqtt/issues/501>
                // <https://github.com/wez/govee2mqtt/issues/394>
                log::trace!("device {device} cannot be polled because it has type Other: {other}");
                return Ok(false);
            }

            let device_state = device.device_state();
            log::info!("requesting update via Platform API {device} {device_state:?}");
            if let Some(info) = &device.http_device_info {
                let http_state = client
                    .get_device_state(info)
                    .await
                    .context("get_device_state")?;
                log::trace!("updated state for {device}");

                {
                    let mut device = self.device_mut(&device.sku, &device.id).await;
                    device.set_http_device_state(http_state);
                    device.set_last_polled();
                }
                self.notify_of_state_change(&device.id)
                    .await
                    .context("state.notify_of_state_change")?;
                return Ok(true);
            }
        } else {
            log::trace!(
                "device {device} wanted a status update, but there is no platform client available"
            );
        }
        Ok(false)
    }

    async fn poll_lan_api<F: Fn(&LanDeviceStatus) -> bool>(
        self: &Arc<Self>,
        device: &LanDevice,
        acceptor: F,
    ) -> anyhow::Result<()> {
        match self.get_lan_client().await {
            Some(client) => {
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() <= deadline {
                    let status = client.query_status(device).await?;
                    let accepted = (acceptor)(&status);
                    self.device_mut(&device.sku, &device.device)
                        .await
                        .set_lan_device_status(status);
                    if accepted {
                        break;
                    }
                    sleep(Duration::from_millis(100)).await;
                }
                self.notify_of_state_change(&device.device).await?;
                Ok(())
            }
            None => anyhow::bail!("LAN control unavailable: no LAN client connected"),
        }
    }

    pub async fn device_control<V: Into<JsonValue>>(
        self: &Arc<Self>,
        device: &Device,
        capability: &DeviceCapability,
        value: V,
    ) -> anyhow::Result<()> {
        let value: JsonValue = value.into();
        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to send {value:?} control to {device}");
                client.control_device(info, capability, value).await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to use Platform API to control {device}");
    }

    pub async fn device_light_power_on(
        self: &Arc<Self>,
        device: &Device,
        on: bool,
    ) -> anyhow::Result<()> {
        if self
            .try_humidifier_set_nightlight(device, |p| p.on = on)
            .await?
        {
            return Ok(());
        }

        let instance_name = device
            .get_light_power_toggle_instance_name()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Unsupported light-only power command for {device}. \
                     Please share the device metadata and state if you report this issue"
                )
            })?;

        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} light power state");
            lan_dev.send_turn(on).await?;
            self.poll_lan_api(lan_dev, |status| status.on == on).await?;
            return Ok(());
        }

        if device.iot_api_supported() {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} light power state");
                    iot.set_power_state(&info.entry, on).await?;
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} light {instance_name} state");
                client.set_toggle_state(info, instance_name, on).await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to control light power state for {device}");
    }

    pub async fn device_power_on(
        self: &Arc<Self>,
        device: &Device,
        on: bool,
    ) -> anyhow::Result<()> {
        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} power state");
            lan_dev.send_turn(on).await?;
            self.poll_lan_api(lan_dev, |status| status.on == on).await?;
            return Ok(());
        }

        if device.iot_api_supported() {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} power state");
                    iot.set_power_state(&info.entry, on).await?;
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} power state");
                client.set_power_state(info, on).await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to control power state for {device}");
    }

    pub async fn device_set_brightness(
        self: &Arc<Self>,
        device: &Device,
        percent: u8,
    ) -> anyhow::Result<()> {
        if self
            .try_humidifier_set_nightlight(device, |p| {
                p.brightness = percent;
                p.on = true;
            })
            .await?
        {
            return Ok(());
        }

        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} brightness");
            lan_dev.send_brightness(percent).await?;
            self.poll_lan_api(lan_dev, |status| status.brightness == percent)
                .await?;
            return Ok(());
        }

        if device.iot_api_supported() {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} brightness");
                    iot.set_brightness(&info.entry, percent).await?;
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} brightness");
                client.set_brightness(info, percent).await?;
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control brightness for {device}");
    }

    pub async fn device_set_color_temperature(
        self: &Arc<Self>,
        device: &Device,
        kelvin: u32,
    ) -> anyhow::Result<()> {
        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} color temperature");
            lan_dev.send_color_temperature_kelvin(kelvin).await?;
            // Clear before polling: poll_lan_api emits the state notification, so the
            // scene must already be cleared or HA briefly sees the new color + old scene.
            self.device_mut(&device.sku, &device.id)
                .await
                .set_active_scene(None);
            self.poll_lan_api(lan_dev, |status| status.color_temperature_kelvin == kelvin)
                .await?;
            return Ok(());
        }

        if device.iot_api_supported() {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} color temperature");
                    iot.set_color_temperature(&info.entry, kelvin).await?;
                    self.device_mut(&device.sku, &device.id)
                        .await
                        .set_active_scene(None);
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} color temperature");
                client.set_color_temperature(info, kelvin).await?;
                self.device_mut(&device.sku, &device.id)
                    .await
                    .set_active_scene(None);
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control color temperature for {device}");
    }

    // FIXME: this function probably shouldn't exist here
    async fn try_humidifier_set_nightlight<F: Fn(&mut SetHumidifierNightlightParams)>(
        self: &Arc<Self>,
        device: &Device,
        apply: F,
    ) -> anyhow::Result<bool> {
        let mut params: SetHumidifierNightlightParams =
            device.nightlight_state.unwrap_or_default().into();
        (apply)(&mut params);

        if let Ok(command) = Base64HexBytes::encode_for_sku(&device.sku, &params) {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} color");
                    iot.send_real(&info.entry, command.base64()).await?;
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    pub async fn humidifier_set_parameter(
        self: &Arc<Self>,
        device: &Device,
        work_mode: i64,
        value: i64,
    ) -> anyhow::Result<()> {
        if let Ok(command) = Base64HexBytes::encode_for_sku(
            &device.sku,
            &SetHumidifierMode {
                mode: work_mode as u8,
                param: value as u8,
            },
        ) {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    iot.send_real(&info.entry, command.base64()).await?;
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                client.set_work_mode(info, work_mode, value).await?;
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control humidifier parameter work_mode={work_mode} for {device}");
    }

    pub async fn device_set_color_rgb(
        self: &Arc<Self>,
        device: &Device,
        r: u8,
        g: u8,
        b: u8,
    ) -> anyhow::Result<()> {
        if self
            .try_humidifier_set_nightlight(device, |p| {
                p.r = r;
                p.g = g;
                p.b = b;
                p.on = true;
            })
            .await?
        {
            return Ok(());
        }

        if let Some(lan_dev) = &device.lan_device {
            let color = crate::lan_api::DeviceColor { r, g, b };
            log::info!("Using LAN API to set {device} color");
            lan_dev.send_color_rgb(color).await?;
            // Clear before polling: poll_lan_api emits the state notification, so the
            // scene must already be cleared or HA briefly sees the new color + old scene.
            self.device_mut(&device.sku, &device.id)
                .await
                .set_active_scene(None);
            self.poll_lan_api(lan_dev, |status| status.color == color)
                .await?;
            return Ok(());
        }

        if device.iot_api_supported() {
            if let Some(iot) = self.get_iot_client().await {
                if let Some(info) = &device.undoc_device_info {
                    log::info!("Using IoT API to set {device} color");
                    iot.set_color_rgb(&info.entry, r, g, b).await?;
                    self.device_mut(&device.sku, &device.id)
                        .await
                        .set_active_scene(None);
                    return Ok(());
                }
            }
        }

        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} color");
                client.set_color_rgb(info, r, g, b).await?;
                self.device_mut(&device.sku, &device.id)
                    .await
                    .set_active_scene(None);
                return Ok(());
            }
        }
        anyhow::bail!("Unable to control color for {device}");
    }

    pub async fn poll_after_control(self: &Arc<Self>, id: String) {
        let Some(device) = self.device_by_id(&id).await else {
            return;
        };

        let iot_available = self.get_iot_client().await.is_some();

        if device.pollable_via_iot() && iot_available {
            return;
        }
        if device.pollable_via_lan() {
            return;
        }

        // Add a slight delay, as the status returned
        // by the platform API isn't guaranteed to be
        // coherent with the command we just issued
        // right away :-/
        sleep(Duration::from_secs(5)).await;

        log::info!("Polling {device} to get latest state after control");
        if let Err(err) = self.poll_platform_api(&device).await {
            log::error!("Polling {device} failed: {err:#}");
        }
    }

    pub async fn device_list_scenes(&self, device: &Device) -> anyhow::Result<Vec<String>> {
        // TODO: some plumbing to maintain offline scene controls for preferred-LAN control
        // Flatten the cached categorized catalog to a flat name list so this path shares
        // the same `scene_catalog_cache` as `device_list_scenes_categorized()` rather than
        // hitting the Govee API on every call.
        let catalog = self.device_list_scenes_categorized(device).await?;
        let names = catalog
            .into_iter()
            .flat_map(|cat| cat.scenes.into_iter().map(|scene| scene.name))
            .collect();
        Ok(sort_and_dedup_scenes(names))
    }

    /// Clears every device's in-memory scene-catalog cache. The MQTT cache-purge
    /// button only recreates the on-disk cache; without this, a stale scene catalog
    /// would survive a purge and only clear on a full restart.
    pub async fn clear_scene_catalogs(&self) {
        for device in self.devices_by_id.lock().await.values_mut() {
            device.clear_scene_catalog();
        }
    }

    /// Returns the scene catalog with category structure preserved.
    /// Unlike `device_list_scenes()` which returns a flat `Vec<String>`,
    /// this preserves category groupings and icon URLs from the Govee API.
    /// Results are cached in the Device struct after first fetch.
    pub async fn device_list_scenes_categorized(
        &self,
        device: &Device,
    ) -> anyhow::Result<Vec<SceneCatalogCategory>> {
        let device = self
            .device_by_id(&device.id)
            .await
            .unwrap_or_else(|| device.clone());
        let cached = device.scene_catalog_cache().cloned();

        if let Some(cached) = &cached {
            if !self.should_refresh_scene_catalog(&device, cached).await {
                return Ok(cached.categories.clone());
            }
        }

        let mut catalog = match self.fetch_scene_catalog(&device).await {
            Ok(catalog) => catalog,
            Err(err) => {
                if let Some(cached) = cached {
                    log::warn!(
                        "Scene catalog refresh failed for {device}: {err:#}; using cached catalog"
                    );
                    return Ok(cached.categories);
                }
                return Err(err);
            }
        };

        // On refresh, keep the cached scenes if the fresh fetch came back empty (e.g. a
        // transient upstream failure) rather than serving an empty list. The fresh
        // `platform_signature` is still adopted below so the refresh check settles
        // instead of refetching on every state notification. (The platform-vs-undoc
        // source tradeoff is handled inside `fetch_scene_catalog`, which uses the
        // platform names as the authoritative spine and enriches them with undoc
        // icons/hints — so a refresh never drops controllable scenes or their media.)
        if let Some(cached) = &cached {
            if catalog.categories.is_empty() && !cached.categories.is_empty() {
                catalog.categories = cached.categories.clone();
            }
        }

        // Cache the (possibly preserved) catalog so the platform-signature refresh
        // check stabilizes. A still-empty result is left uncached so a device whose
        // metadata hasn't loaded yet isn't pinned to "no scenes".
        if !catalog.categories.is_empty() {
            self.device_mut(&device.sku, &device.id)
                .await
                .set_scene_catalog(catalog.clone());
        }

        Ok(catalog.categories)
    }

    /// Fetches the scene catalog from the Govee API (no caching).
    async fn fetch_scene_catalog(&self, device: &Device) -> anyhow::Result<SceneCatalogCache> {
        let platform_client = self.get_platform_client().await;
        let platform_signature = platform_client
            .as_ref()
            .and_then(|_| scene_platform_signature(device));

        // Platform API is the authoritative set of controllable scene names but carries
        // no icons/hints. When it returns names, use them as the spine and enrich each
        // with icon/hint metadata from the undocumented API, matched by name. Scenes with
        // no undoc match still appear (without a thumbnail); undoc-only scenes the platform
        // can't drive are intentionally dropped. This is strictly >= using either source
        // alone: it never hides a controllable scene and never loses a thumbnail that exists.
        if let Some(client) = platform_client {
            if let Some(info) = &device.http_device_info {
                let names = sort_and_dedup_scenes(client.list_scene_names(info).await?);
                if !names.is_empty() {
                    let media = scene_media_by_name(&device.sku).await;
                    return Ok(SceneCatalogCache {
                        platform_signature,
                        categories: vec![SceneCatalogCategory {
                            name: "All".to_string(),
                            scenes: enrich_scene_names(names, &media),
                        }],
                    });
                }
            }
        }

        // No platform names available — fall back to the undocumented catalog wholesale
        // (preserves its real category groupings, icons, and hints).
        if let Ok(categories) = GoveeUndocumentedApi::get_scenes_for_device(&device.sku).await {
            let mut result = vec![];
            for cat in categories {
                let mut scenes = vec![];
                for scene in cat.scenes {
                    // Same validity filter as device_list_scenes():
                    // include only if at least one LightEffectEntry has scene_code != 0
                    let valid = scene
                        .light_effects
                        .iter()
                        .any(|effect| effect.scene_code != 0);
                    if valid {
                        scenes.push(SceneCatalogEntry {
                            name: scene.scene_name,
                            icon_urls: scene.icon_urls,
                            hint: if scene.scenes_hint.is_empty() {
                                None
                            } else {
                                Some(scene.scenes_hint)
                            },
                        });
                    }
                }
                if !scenes.is_empty() {
                    result.push(SceneCatalogCategory {
                        name: cat.category_name,
                        scenes,
                    });
                }
            }
            return Ok(SceneCatalogCache {
                platform_signature,
                categories: result,
            });
        }

        Ok(SceneCatalogCache {
            platform_signature,
            categories: vec![],
        })
    }

    async fn should_refresh_scene_catalog(
        &self,
        device: &Device,
        cache: &SceneCatalogCache,
    ) -> bool {
        if self.get_platform_client().await.is_none() {
            return false;
        }

        let current_signature = scene_platform_signature(device);
        current_signature.is_some() && cache.platform_signature != current_signature
    }

    pub async fn device_set_target_temperature(
        self: &Arc<Self>,
        device: &Device,
        instance_name: &str,
        target: TemperatureValue,
    ) -> anyhow::Result<()> {
        if let Some(client) = self.get_platform_client().await {
            if let Some(info) = &device.http_device_info {
                log::info!("Using Platform API to set {device} target temperature to {target}");
                client
                    .set_target_temperature(info, instance_name, target)
                    .await?;
                return Ok(());
            }
        }

        anyhow::bail!("Unable to set temperature for {device}");
    }

    pub async fn device_set_scene(
        self: &Arc<Self>,
        device: &Device,
        scene: &str,
    ) -> anyhow::Result<()> {
        // TODO: some plumbing to maintain offline scene controls for preferred-LAN control
        let avoid_platform_api = device.avoid_platform_api();

        if !avoid_platform_api {
            if let Some(client) = self.get_platform_client().await {
                if let Some(info) = &device.http_device_info {
                    log::info!("Using Platform API to set {device} to scene {scene}");
                    // Music styles carry the stored sensitivity preference: it
                    // cannot be sent on its own, so it rides the style change.
                    //
                    // Read it fresh rather than from `device`: callers hold a
                    // snapshot cloned by `resolve_device_for_control` *before*
                    // it acquires the per-device permit, so a slider write that
                    // lands while this command waits would otherwise be lost.
                    let sensitivity = match self.device_by_id(&device.id).await {
                        Some(current) => current.music_sensitivity(),
                        None => device.music_sensitivity(),
                    };
                    client
                        .set_scene_by_name_with_sensitivity(info, scene, sensitivity)
                        .await?;
                    self.device_mut(&device.sku, &device.id)
                        .await
                        .set_active_scene(Some(scene));
                    return Ok(());
                }
            }
        }

        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} to scene {scene}");
            lan_dev.set_scene_by_name(scene).await?;

            self.device_mut(&device.sku, &device.id)
                .await
                .set_active_scene(Some(scene));
            return Ok(());
        }

        anyhow::bail!("Unable to set scene for {device}");
    }

    /// Program music mode with a caller-chosen palette. LAN-only: the
    /// frames ride `ptReal`, and no other transport is verified for them
    /// (docs/MUSIC_MODE.md). The burst is UDP without acknowledgement, so
    /// it is sent twice, like the Govee app does — the sequence is
    /// idempotent, and a lost burst otherwise means a silently ignored
    /// command.
    pub async fn device_set_music_palette(
        self: &Arc<Self>,
        device: &Device,
        command: &SetMusicPalette,
    ) -> anyhow::Result<()> {
        let encoded = Base64HexBytes::encode_for_sku("Generic:Light", command)?.base64();

        if let Some(lan_dev) = &device.lan_device {
            log::info!("Using LAN API to set {device} music palette");
            lan_dev.send_real(encoded.clone()).await?;
            sleep(Duration::from_millis(300)).await;
            lan_dev.send_real(encoded).await?;
            // The device no longer shows whatever scene the bridge last
            // applied; music mode replaces it.
            self.device_mut(&device.sku, &device.id)
                .await
                .set_active_scene(None);
            return Ok(());
        }

        anyhow::bail!(
            "Unable to set music palette for {device}: it was not discovered \
             on the LAN. Music palettes are LAN-only; check that LAN Control \
             is enabled for this device in the Govee Home app"
        );
    }

    // Take care not to call this while you hold a mutable device
    // reference, as that will deadlock!
    pub async fn notify_of_state_change(self: &Arc<Self>, device_id: &str) -> anyhow::Result<()> {
        let Some(canonical_device) = self.device_by_id(device_id).await else {
            anyhow::bail!("Device not found: {device_id}");
        };

        if let Some(hass) = self.get_hass_client().await {
            hass.advise_hass_of_light_state(&canonical_device, self)
                .await?;
        }

        Ok(())
    }
}

pub fn sort_and_dedup_scenes(mut scenes: Vec<String>) -> Vec<String> {
    scenes.sort_by_key(|s| s.to_ascii_lowercase());
    scenes.dedup();
    scenes
}

fn scene_platform_signature(device: &Device) -> Option<String> {
    let info = device.http_device_info.as_ref()?;
    let mut capabilities: Vec<_> = info
        .capabilities
        .iter()
        .map(|cap| format!("{:?}:{}", cap.kind, cap.instance))
        .collect();
    capabilities.sort();
    Some(format!(
        "{}:{}:{}",
        info.sku,
        info.device,
        capabilities.join("|")
    ))
}

/// Best-effort map of scene name (lowercased) -> (icon_urls, hint) from the
/// undocumented API, used to enrich the authoritative platform scene list. Returns
/// an empty map when the undocumented API is unavailable, so enrichment degrades to
/// platform-names-only rather than failing. The first occurrence of a name wins.
async fn scene_media_by_name(sku: &str) -> HashMap<String, (Vec<String>, Option<String>)> {
    let mut media = HashMap::new();
    if let Ok(categories) = GoveeUndocumentedApi::get_scenes_for_device(sku).await {
        for cat in categories {
            for scene in cat.scenes {
                let hint = if scene.scenes_hint.is_empty() {
                    None
                } else {
                    Some(scene.scenes_hint)
                };
                media
                    .entry(scene.scene_name.to_ascii_lowercase())
                    .or_insert((scene.icon_urls, hint));
            }
        }
    }
    media
}

/// Builds scene entries from the authoritative platform name list, attaching undoc
/// icon/hint metadata where the name matches (case-insensitively). Every platform
/// name yields exactly one entry; a name with no media match still appears with empty
/// media. Undoc-only names (not in `names`) are not represented — the platform decides
/// the controllable set.
fn enrich_scene_names(
    names: Vec<String>,
    media: &HashMap<String, (Vec<String>, Option<String>)>,
) -> Vec<SceneCatalogEntry> {
    names
        .into_iter()
        .map(|name| {
            let (icon_urls, hint) = media
                .get(&name.to_ascii_lowercase())
                .cloned()
                .unwrap_or_default();
            SceneCatalogEntry {
                name,
                icon_urls,
                hint,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform_api::{DeviceCapabilityKind, GoveeApiClient, HttpDeviceInfo};

    fn test_scene_catalog_cache(
        platform_signature: Option<String>,
        scene_name: &str,
    ) -> SceneCatalogCache {
        SceneCatalogCache {
            platform_signature,
            categories: vec![SceneCatalogCategory {
                name: "All".to_string(),
                scenes: vec![SceneCatalogEntry {
                    name: scene_name.to_string(),
                    icon_urls: vec![],
                    hint: None,
                }],
            }],
        }
    }

    fn dynamic_scene_capability(instance: &str) -> DeviceCapability {
        DeviceCapability {
            kind: DeviceCapabilityKind::DynamicScene,
            instance: instance.to_string(),
            parameters: None,
            alarm_type: None,
            event_state: None,
        }
    }

    fn http_device_info(id: &str, instance: &str) -> HttpDeviceInfo {
        HttpDeviceInfo {
            sku: "H6001".to_string(),
            device: id.to_string(),
            device_name: "Test Light".to_string(),
            device_type: DeviceType::Light,
            capabilities: vec![dynamic_scene_capability(instance)],
        }
    }

    #[test]
    fn test_scene_catalog_entry_hint_serde_round_trip() {
        // hint: Some should serialize with "hint" field present
        let entry_with_hint = SceneCatalogEntry {
            name: "Karst Cave".to_string(),
            icon_urls: vec!["https://example.com/icon.png".to_string()],
            hint: Some("Calm green tones".to_string()),
        };
        let json = serde_json::to_string(&entry_with_hint).unwrap();
        assert!(json.contains("\"hint\":\"Calm green tones\""));
        let deserialized: SceneCatalogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.hint, Some("Calm green tones".to_string()));

        // hint: None should omit the "hint" field entirely (skip_serializing_if)
        let entry_no_hint = SceneCatalogEntry {
            name: "Sunset Glow".to_string(),
            icon_urls: vec![],
            hint: None,
        };
        let json = serde_json::to_string(&entry_no_hint).unwrap();
        assert!(!json.contains("hint"));
        // Deserializing JSON without "hint" field should produce None (serde default)
        let deserialized: SceneCatalogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.hint, None);
    }

    #[test]
    fn test_enrich_scene_names_uses_platform_spine_with_undoc_media() {
        // Platform is the authoritative controllable set; undoc supplies icons/hints.
        let mut media = HashMap::new();
        media.insert(
            "aurora".to_string(),
            (
                vec!["https://example.com/aurora.png".to_string()],
                Some("calm".to_string()),
            ),
        );
        // Undoc has a scene the platform does NOT list — it must be dropped.
        media.insert(
            "undoc only".to_string(),
            (vec!["https://example.com/x.png".to_string()], None),
        );

        // Platform lists "Aurora" (matches, case-insensitively via "AURORA") and
        // "Music: Energic" (platform-only, no undoc media).
        let names = vec!["AURORA".to_string(), "Music: Energic".to_string()];
        let scenes = enrich_scene_names(names, &media);

        // Exactly the platform set, in platform order. Undoc-only scene not present.
        assert_eq!(scenes.len(), 2);
        assert_eq!(scenes[0].name, "AURORA");
        assert_eq!(
            scenes[0].icon_urls,
            vec!["https://example.com/aurora.png".to_string()]
        );
        assert_eq!(scenes[0].hint.as_deref(), Some("calm"));

        // Platform-only scene survives (controllable) but carries no thumbnail.
        assert_eq!(scenes[1].name, "Music: Energic");
        assert!(scenes[1].icon_urls.is_empty());
        assert_eq!(scenes[1].hint, None);

        assert!(!scenes
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case("undoc only")));
    }

    #[test]
    fn test_scene_catalog_entry_hint_empty_string_convention() {
        // The convention: empty scenes_hint from API becomes None, non-empty becomes Some.
        // This tests the pattern used in fetch_scene_catalog.
        let empty_hint = "";
        let result: Option<String> = if empty_hint.is_empty() {
            None
        } else {
            Some(empty_hint.to_string())
        };
        assert_eq!(result, None);

        let non_empty_hint = "Gentle pulsing warmth";
        let result: Option<String> = if non_empty_hint.is_empty() {
            None
        } else {
            Some(non_empty_hint.to_string())
        };
        assert_eq!(result, Some("Gentle pulsing warmth".to_string()));
    }

    #[test]
    fn test_sort_and_dedup_scenes() {
        let scenes = vec![
            "Sunset".to_string(),
            "aurora".to_string(),
            "Blaze".to_string(),
            "aurora".to_string(),
            "Sunset".to_string(),
        ];
        let result = sort_and_dedup_scenes(scenes);
        assert_eq!(result, vec!["aurora", "Blaze", "Sunset"]);
    }

    #[test]
    fn test_sort_and_dedup_scenes_empty() {
        let result = sort_and_dedup_scenes(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_sort_and_dedup_scenes_case_insensitive_order() {
        // Verify case-insensitive sorting keeps correct order
        let scenes = vec!["Zelda".to_string(), "alpha".to_string(), "Beta".to_string()];
        let result = sort_and_dedup_scenes(scenes);
        assert_eq!(result, vec!["alpha", "Beta", "Zelda"]);
    }

    #[test]
    fn test_scene_catalog_category_serde_round_trip() {
        let category = SceneCatalogCategory {
            name: "Nature".to_string(),
            scenes: vec![
                SceneCatalogEntry {
                    name: "Forest".to_string(),
                    icon_urls: vec!["https://example.com/forest.png".to_string()],
                    hint: Some("Deep greens".to_string()),
                },
                SceneCatalogEntry {
                    name: "Ocean".to_string(),
                    icon_urls: vec![],
                    hint: None,
                },
            ],
        };
        let json = serde_json::to_string(&category).unwrap();
        let deserialized: SceneCatalogCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "Nature");
        assert_eq!(deserialized.scenes.len(), 2);
        assert_eq!(deserialized.scenes[0].hint, Some("Deep greens".to_string()));
        assert_eq!(deserialized.scenes[1].hint, None);
    }

    #[tokio::test]
    async fn test_state_temperature_scale_default() {
        let state = State::new();
        let scale = state.get_temperature_scale().await;
        // Default should be Celsius
        assert!(matches!(scale, TemperatureScale::Celsius));
    }

    #[tokio::test]
    async fn test_state_temperature_scale_round_trip() {
        let state = State::new();
        state
            .set_temperature_scale(TemperatureScale::Fahrenheit)
            .await;
        let scale = state.get_temperature_scale().await;
        assert!(matches!(scale, TemperatureScale::Fahrenheit));
    }

    #[tokio::test]
    async fn test_state_hass_disco_prefix_default_empty() {
        let state = State::new();
        let prefix = state.get_hass_disco_prefix().await;
        assert_eq!(prefix, "");
    }

    #[tokio::test]
    async fn test_state_hass_disco_prefix_round_trip() {
        let state = State::new();
        state
            .set_hass_disco_prefix("homeassistant".to_string())
            .await;
        let prefix = state.get_hass_disco_prefix().await;
        assert_eq!(prefix, "homeassistant");
    }

    #[tokio::test]
    async fn test_state_device_mut_creates_device() {
        let state = State::new();
        {
            let _device = state.device_mut("H6001", "AA:BB:CC:DD:EE:FF").await;
        }
        let devices = state.devices().await;
        assert_eq!(devices.len(), 1);
    }

    #[tokio::test]
    async fn test_state_devices_empty_by_default() {
        let state = State::new();
        let devices = state.devices().await;
        assert!(devices.is_empty());
    }

    #[tokio::test]
    async fn test_scene_catalog_reads_canonical_cache_for_stale_clone() {
        let state = State::new();
        let stale_clone = Device::new("H6001", "AA:BB:CC:DD:EE:FF");
        {
            let mut canonical = state.device_mut("H6001", "AA:BB:CC:DD:EE:FF").await;
            canonical.set_scene_catalog(test_scene_catalog_cache(None, "Aurora"));
        }

        let catalog = state
            .device_list_scenes_categorized(&stale_clone)
            .await
            .unwrap();

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].scenes[0].name, "Aurora");
    }

    #[tokio::test]
    async fn test_clear_scene_catalogs_drops_cached_catalog() {
        let state = State::new();
        {
            let mut canonical = state.device_mut("H6001", "AA:BB:CC:DD:EE:FF").await;
            canonical.set_scene_catalog(test_scene_catalog_cache(None, "Aurora"));
        }
        assert!(state
            .device_by_id("AA:BB:CC:DD:EE:FF")
            .await
            .unwrap()
            .scene_catalog_cache()
            .is_some());

        state.clear_scene_catalogs().await;

        assert!(state
            .device_by_id("AA:BB:CC:DD:EE:FF")
            .await
            .unwrap()
            .scene_catalog_cache()
            .is_none());
    }

    #[tokio::test]
    async fn test_scene_catalog_refreshes_when_platform_metadata_arrives() {
        let state = State::new();
        state
            .set_platform_client(GoveeApiClient::new("test-key"))
            .await;

        let mut device = Device::new("H6001", "AA:BB:CC:DD:EE:FF");
        device.http_device_info = Some(http_device_info(&device.id, "lightScene"));
        let stale_undoc_cache = test_scene_catalog_cache(None, "Fallback");

        assert!(
            state
                .should_refresh_scene_catalog(&device, &stale_undoc_cache)
                .await
        );

        let current_cache = SceneCatalogCache {
            platform_signature: scene_platform_signature(&device),
            ..stale_undoc_cache
        };

        assert!(
            !state
                .should_refresh_scene_catalog(&device, &current_cache)
                .await
        );
    }

    /// Regression guard for the sensitivity read in `device_set_scene`.
    ///
    /// The `Device` a control command holds is a clone taken by
    /// `resolve_device_for_control` *before* it acquires the per-device permit,
    /// so a slider write that lands while the command queues is absent from it.
    /// The scene call therefore re-reads the canonical device; reverting to
    /// `device.music_sensitivity()` would silently ship the pre-write value.
    #[tokio::test]
    async fn music_sensitivity_is_read_from_the_canonical_device_not_the_snapshot() {
        let state = State::new();
        let stale_snapshot = Device::new("H607C", "AA:BB:CC:DD:EE:FF");
        {
            let mut canonical = state.device_mut("H607C", "AA:BB:CC:DD:EE:FF").await;
            canonical.set_music_sensitivity(42);
        }

        assert_eq!(
            stale_snapshot.music_sensitivity(),
            crate::platform_api::DEFAULT_MUSIC_SENSITIVITY,
            "the snapshot predates the slider write"
        );

        // The exact lookup device_set_scene performs before calling
        // set_scene_by_name_with_sensitivity.
        let sensitivity = match state.device_by_id(&stale_snapshot.id).await {
            Some(current) => current.music_sensitivity(),
            None => stale_snapshot.music_sensitivity(),
        };
        assert_eq!(sensitivity, 42, "the fresh value must win");
    }

    /// The fallback arm of that same lookup: if the device is no longer in the
    /// map, the command must still send something rather than bail.
    #[tokio::test]
    async fn music_sensitivity_falls_back_to_the_snapshot_when_the_device_is_gone() {
        let state = State::new();
        let mut snapshot = Device::new("H607C", "AA:BB:CC:DD:EE:FF");
        snapshot.set_music_sensitivity(7);

        assert!(state.device_by_id(&snapshot.id).await.is_none());

        let sensitivity = match state.device_by_id(&snapshot.id).await {
            Some(current) => current.music_sensitivity(),
            None => snapshot.music_sensitivity(),
        };
        assert_eq!(sensitivity, 7);
    }
}
