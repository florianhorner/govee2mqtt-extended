use crate::cache::{cache_get, CacheComputeResult, CacheGetOptions};
use crate::lan_api::Client as LanClient;
use crate::platform_api::{GoveeApiClient, HttpDeviceInfo};
use crate::service::device::Device;
use crate::service::hass::spawn_hass_integration;
use crate::service::http::run_http_server;
use crate::service::iot::start_iot_client;
use crate::service::state::StateHandle;
use crate::undoc_api::GoveeUndocumentedApi;
use crate::version_info::govee_version;
use crate::UndocApiArguments;
use anyhow::Context;
use chrono::Utc;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::time::{sleep, Duration};

pub static POLL_INTERVAL: Lazy<chrono::Duration> = Lazy::new(|| chrono::Duration::seconds(900));

#[derive(clap::Parser, Debug)]
pub struct ServeCommand {
    /// The port on which the HTTP API will listen
    #[arg(long, default_value_t = 8056)]
    http_port: u16,
}

async fn poll_single_device(state: &StateHandle, device: &Device) -> anyhow::Result<()> {
    let now = Utc::now();

    if device.is_ble_only_device() == Some(true) {
        // We can't poll this device, we have no ble support
        return Ok(());
    }

    // Collect the device status via the LAN API, if possible.
    // This is partially redundant with the LAN discovery task,
    // but the timing of that is not as regular and predictable
    // because it employs exponential backoff.
    // Some Govee devices have bad firmware that will cause the
    // lights to flicker about a minute after polling, so it
    // is desirable to keep polling on a regular basis.
    // <https://github.com/wez/govee2mqtt/issues/250>
    if let Some(lan_device) = &device.lan_device {
        if let Some(client) = state.get_lan_client().await {
            if let Ok(status) = client.query_status(lan_device).await {
                state
                    .device_mut(&lan_device.sku, &lan_device.device)
                    .await
                    .set_lan_device_status(status);
                state.notify_of_state_change(&lan_device.device).await.ok();
            }
        }
    }

    let poll_interval = device.preferred_poll_interval();

    let can_update = match &device.last_polled {
        None => true,
        Some(last) => now - last > poll_interval,
    };

    if !can_update {
        return Ok(());
    }

    let device_state = device.device_state();
    let needs_update = match &device_state {
        None => true,
        Some(state) => now - state.updated > poll_interval,
    };

    if !needs_update {
        return Ok(());
    }

    let needs_platform = device.needs_platform_poll();

    // Don't interrogate via HTTP if we can use the LAN.
    // If we have LAN and the device is stale, it is likely
    // offline and there is little sense in burning up request
    // quota to the platform API for it
    if device.lan_device.is_some() && !needs_platform {
        log::trace!("LAN-available device {device} needs a status update; it's likely offline.");
        return Ok(());
    }

    if !needs_platform && state.poll_iot_api(device).await? {
        return Ok(());
    }

    state.poll_platform_api(device).await?;

    Ok(())
}

async fn periodic_state_poll(state: StateHandle) -> anyhow::Result<()> {
    sleep(Duration::from_secs(20)).await;
    loop {
        for d in state.devices().await {
            if let Err(err) = poll_single_device(&state, &d).await {
                log::error!("while polling {d}: {err:#}");
            }
        }

        sleep(Duration::from_secs(30)).await;
    }
}

/// Returns the configured capability cache refresh interval.
/// Reads from GOVEE_CAPABILITY_CACHE_DAYS env var (default: 7).
/// A value of 0 means "never refresh" (use a very long soft TTL).
fn capability_cache_soft_ttl() -> StdDuration {
    let days: u64 = std::env::var("GOVEE_CAPABILITY_CACHE_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7);
    if days == 0 {
        // "Never refresh" — use 10 years as a practical infinity
        StdDuration::from_secs(10 * 365 * 86400)
    } else {
        StdDuration::from_secs(days * 86400)
    }
}

const CAPABILITY_CACHE_TOPIC: &str = "capability-cache";
const CAPABILITY_CACHE_HARD_TTL: StdDuration = StdDuration::from_secs(365 * 86400);

/// Write an HttpDeviceInfo to the per-SKU capability cache.
/// Uses the cache_get write-through pattern: the closure trivially returns
/// the value we already have. This is intentional — cache_get is a
/// read-with-compute primitive, and we're using it as a write primitive
/// by making the "compute" a no-op that returns the pre-fetched value.
async fn write_capability_cache(info: &HttpDeviceInfo) {
    let sku = info.sku.clone();
    let info_clone = info.clone();
    if let Err(err) = cache_get(
        CacheGetOptions {
            topic: CAPABILITY_CACHE_TOPIC,
            key: &sku,
            soft_ttl: capability_cache_soft_ttl(),
            hard_ttl: CAPABILITY_CACHE_HARD_TTL,
            negative_ttl: StdDuration::from_secs(60),
            allow_stale: true,
        },
        async move { Ok(CacheComputeResult::Value(info_clone)) },
    )
    .await
    {
        log::warn!("Failed to write capability cache for SKU {sku}: {err:#}");
    }
}

/// Try to load cached capabilities for a device that has no http_device_info.
/// Overwrites the `device` and `device_name` fields to match the actual device,
/// since the cache is per-SKU and may contain a different device's ID/name.
async fn try_hydrate_from_capability_cache(
    sku: &str,
    device_id: &str,
    device_name: &str,
) -> Option<HttpDeviceInfo> {
    let result: anyhow::Result<HttpDeviceInfo> = cache_get(
        CacheGetOptions {
            topic: CAPABILITY_CACHE_TOPIC,
            key: sku,
            soft_ttl: capability_cache_soft_ttl(),
            hard_ttl: CAPABILITY_CACHE_HARD_TTL,
            negative_ttl: StdDuration::from_secs(60),
            allow_stale: true,
        },
        // On cache miss, there is nothing to compute — we don't have the data.
        // Return an error so cache_get treats this as a miss.
        async { anyhow::bail!("no cached capabilities for SKU") },
    )
    .await;

    match result {
        Ok(mut info) => {
            // Overwrite per-device fields: the cache stores per-SKU data,
            // but device and device_name are per-device. Using stale values
            // here would route control commands to the wrong physical device.
            info.device = device_id.to_string();
            info.device_name = device_name.to_string();
            log::info!(
                "Hydrated {sku} ({device_id}) from capability cache: \
                 type={:?}, rgb={}, brightness={}, color_temp={:?}",
                info.device_type,
                info.supports_rgb(),
                info.supports_brightness(),
                info.get_color_temperature_range(),
            );
            Some(info)
        }
        Err(_) => None,
    }
}

/// Log human-readable capability diffs when a refresh detects changes.
fn log_capability_diff(sku: &str, old: &HttpDeviceInfo, new: &HttpDeviceInfo) {
    if old.device_type != new.device_type {
        log::info!(
            "Capability change for {sku}: device_type {:?} -> {:?}",
            old.device_type,
            new.device_type
        );
    }
    if old.supports_rgb() != new.supports_rgb() {
        log::info!(
            "Capability change for {sku}: supports_rgb {} -> {}",
            old.supports_rgb(),
            new.supports_rgb()
        );
    }
    if old.supports_brightness() != new.supports_brightness() {
        log::info!(
            "Capability change for {sku}: supports_brightness {} -> {}",
            old.supports_brightness(),
            new.supports_brightness()
        );
    }
    if old.get_color_temperature_range() != new.get_color_temperature_range() {
        log::info!(
            "Capability change for {sku}: color_temp_range {:?} -> {:?}",
            old.get_color_temperature_range(),
            new.get_color_temperature_range()
        );
    }
}

async fn enumerate_devices_via_platform_api(
    state: StateHandle,
    client: Option<GoveeApiClient>,
) -> anyhow::Result<()> {
    let client = match client {
        Some(client) => client,
        None => match state.get_platform_client().await {
            Some(client) => client,
            None => return Ok(()),
        },
    };

    log::info!("Querying platform API for device list");
    for info in client.get_devices().await? {
        // Log capability diffs if this device already had cached/live capabilities
        {
            let existing = state.device_by_id(&info.device).await;
            if let Some(existing) = existing {
                if let Some(old_info) = &existing.http_device_info {
                    log_capability_diff(&info.sku, old_info, &info);
                }
            }
        }

        // Write per-SKU capability cache (for offline use on subsequent boots)
        write_capability_cache(&info).await;

        let mut device = state.device_mut(&info.sku, &info.device).await;
        device.set_http_device_info(info);
    }
    Ok(())
}

async fn enumerate_devices_via_undo_api(
    state: StateHandle,
    client: Option<GoveeUndocumentedApi>,
    args: &UndocApiArguments,
) -> anyhow::Result<()> {
    let (client, needs_start) = match client {
        Some(client) => (client, true),
        None => match state.get_undoc_client().await {
            Some(client) => (client, false),
            None => return Ok(()),
        },
    };

    log::info!("Querying undocumented API for device + room list");
    let acct = client.login_account_cached().await?;
    let info = client.get_device_list(&acct.token).await?;
    let mut group_by_id = HashMap::new();
    for group in info.groups {
        group_by_id.insert(group.group_id, group.group_name);
    }
    for entry in info.devices {
        let mut device = state.device_mut(&entry.sku, &entry.device).await;
        let room_name = group_by_id.get(&entry.group_id).map(|name| name.as_str());
        device.set_undoc_device_info(entry, room_name);
    }

    if needs_start {
        start_iot_client(args, state.clone(), Some(acct)).await?;
    }
    Ok(())
}

const ISSUE_76_EXPLANATION: &str = "Startup cannot automatically continue because entity names\n\
    could become inconsistent especially across frequent similar\n\
    intermittent issues if/as they occur on an ongoing basis.\n\
    Please see https://github.com/wez/govee2mqtt/issues/76\n\
    A workaround is to remove the Govee API credentials from your\n\
    configuration, which will cause this govee2mqtt to use only\n\
    the LAN API. Two consequences of that will be loss of control\n\
    over devices that do not support the LAN API, and also devices\n\
    changing entity ID to less descriptive names due to lack of\n\
    metadata availability via the LAN API.";

impl ServeCommand {
    pub async fn run(&self, args: &crate::Args) -> anyhow::Result<()> {
        log::info!("Starting service. version {}", govee_version());
        let state = Arc::new(crate::service::state::State::new());

        // First, use the HTTP APIs to determine the list of devices and
        // their names.

        if let Ok(client) = args.api_args.api_client() {
            if let Err(err) =
                enumerate_devices_via_platform_api(state.clone(), Some(client.clone())).await
            {
                anyhow::bail!(
                    "Error during initial platform API discovery: {err:#}\n{ISSUE_76_EXPLANATION}"
                );
            }

            // only record the client after we've completed the
            // initial platform disco attempt
            state.set_platform_client(client).await;

            // spawn periodic discovery task
            let state = state.clone();
            tokio::spawn(async move {
                loop {
                    sleep(Duration::from_secs(600)).await;
                    if let Err(err) = enumerate_devices_via_platform_api(state.clone(), None).await
                    {
                        log::error!("Error during periodic platform API discovery: {err:#}");
                    }
                }
            });
        }
        if let Ok(client) = args.undoc_args.api_client() {
            if let Err(err) = enumerate_devices_via_undo_api(
                state.clone(),
                Some(client.clone()),
                &args.undoc_args,
            )
            .await
            {
                anyhow::bail!(
                    "Error during initial undoc API discovery: {err:#}\n{ISSUE_76_EXPLANATION}"
                );
            }

            // only record the client after we've completed the
            // initial undoc disco attempt
            state.set_undoc_client(client).await;

            // spawn periodic discovery task
            let state = state.clone();
            let args = args.undoc_args.clone();
            tokio::spawn(async move {
                loop {
                    sleep(Duration::from_secs(600)).await;
                    if let Err(err) =
                        enumerate_devices_via_undo_api(state.clone(), None, &args).await
                    {
                        log::error!("Error during periodic undoc API discovery: {err:#}");
                    }
                }
            });
        }

        // Now start LAN discovery

        let options = args.lan_disco_args.to_disco_options()?;
        if !options.is_empty() {
            log::info!("Starting LAN discovery");
            let state = state.clone();
            let (client, mut scan) = LanClient::new(options).await?;

            state.set_lan_client(client.clone()).await;

            tokio::spawn(async move {
                while let Some(lan_device) = scan.recv().await {
                    log::trace!("LAN disco: {lan_device:?}");

                    // Check if this device needs capability cache hydration
                    // (late-discovered device that wasn't in the initial Platform API batch)
                    let needs_hydration = {
                        let existing = state.device_by_id(&lan_device.device).await;
                        existing
                            .map(|d| d.http_device_info.is_none() && !d.avoid_platform_api())
                            .unwrap_or(true)
                    };

                    state
                        .device_mut(&lan_device.sku, &lan_device.device)
                        .await
                        .set_lan_device(lan_device.clone());

                    // Hydrate from capability cache if needed
                    if needs_hydration {
                        let device_name = state
                            .device_by_id(&lan_device.device)
                            .await
                            .map(|d| d.name())
                            .unwrap_or_default();
                        if let Some(info) = try_hydrate_from_capability_cache(
                            &lan_device.sku,
                            &lan_device.device,
                            &device_name,
                        )
                        .await
                        {
                            state
                                .device_mut(&lan_device.sku, &lan_device.device)
                                .await
                                .set_http_device_info(info);
                        }
                    }

                    let state = state.clone();
                    let client = client.clone();
                    tokio::spawn(async move {
                        if let Ok(status) = client.query_status(&lan_device).await {
                            state
                                .device_mut(&lan_device.sku, &lan_device.device)
                                .await
                                .set_lan_device_status(status);

                            log::trace!("LAN disco: update and notify {}", lan_device.device);
                            state.notify_of_state_change(&lan_device.device).await.ok();
                        }
                    });
                }
            });

            // I don't love that this is 10 seconds but since our timeout
            // for query_status is 10 seconds, and we show a warning for
            // devices that didn't respond in the section below, in the
            // interest of reducing false positives we need to wait long
            // enough to provide high-signal warnings.
            log::info!("Waiting 10 seconds for LAN API discovery");
            sleep(Duration::from_secs(10)).await;
        }

        // Hydrate devices from capability cache: for any device discovered
        // via LAN or undoc API that doesn't yet have http_device_info
        // (e.g., Platform API was unreachable), try loading from the per-SKU
        // capability cache. This is the core of the "Living Quirks" feature.
        {
            let devices = state.devices().await;
            for device in &devices {
                if device.http_device_info.is_some() {
                    continue;
                }
                if device.avoid_platform_api() {
                    continue;
                }
                let name = device.name();
                if let Some(info) =
                    try_hydrate_from_capability_cache(&device.sku, &device.id, &name).await
                {
                    state
                        .device_mut(&device.sku, &device.id)
                        .await
                        .set_http_device_info(info);
                }
            }
        }

        log::info!("Devices returned from Govee's APIs");
        for device in state.devices().await {
            log::info!("{device}");
            if let Some(lan) = &device.lan_device {
                log::info!("  LAN API: ip={:?}", lan.ip);
            }
            if let Some(http_info) = &device.http_device_info {
                let kind = &http_info.device_type;
                let rgb = http_info.supports_rgb();
                let bright = http_info.supports_brightness();
                let color_temp = http_info.get_color_temperature_range();
                let segment_rgb = http_info.supports_segmented_rgb();
                log::info!(
                    "  Platform API: {kind}. supports_rgb={rgb} supports_brightness={bright}"
                );
                log::info!("                color_temp={color_temp:?} segment_rgb={segment_rgb:?}");
                log::trace!("{http_info:#?}");
            }
            if let Some(undoc) = &device.undoc_device_info {
                let room = &undoc.room_name;
                let supports_iot = undoc.entry.device_ext.device_settings.topic.is_some();
                let ble_only = undoc.entry.device_ext.device_settings.wifi_name.is_none();
                log::info!(
                    "  Undoc: room={room:?} supports_iot={supports_iot} ble_only={ble_only}"
                );
                log::trace!("{undoc:#?}");
            }
            if let Some(quirk) = device.resolve_quirk() {
                log::info!("  {quirk:?}");

                // Sanity check for LAN devices: if we don't see an API for it,
                // it may indicate a networking issue
                if quirk.lan_api_capable && device.lan_device.is_none() {
                    log::warn!(
                        "  This device should be available via the LAN API, \
                        but didn't respond to probing yet. Possible causes:"
                    );
                    log::warn!("  1) LAN API needs to be enabled in the Govee Home App.");
                    log::warn!("  2) The device is offline.");
                    log::warn!("  3) A network configuration issue is preventing communication.");
                    log::warn!(
                        "  4) The device needs a firmware update before it can enable LAN API."
                    );
                    log::warn!(
                        "  5) The hardware version of the device is too old to enable the LAN API."
                    );
                }
            } else if device.http_device_info.is_none() {
                log::warn!("  Unknown device type. Cannot map to Home Assistant.");
                if state.get_platform_client().await.is_none() {
                    log::warn!(
                        "  Recommendation: configure your Govee API Key so that \
                                  metadata can be fetched from Govee"
                    );
                }
            }

            log::info!("");
        }

        // Start periodic status polling
        {
            let state = state.clone();
            tokio::spawn(async move {
                if let Err(err) = periodic_state_poll(state).await {
                    log::error!("periodic_state_poll: {err:#}");
                }
            });
        }

        // start advertising on local mqtt
        spawn_hass_integration(state.clone(), &args.hass_args).await?;

        run_http_server(state.clone(), self.http_port)
            .await
            .with_context(|| format!("Starting HTTP service on port {}", self.http_port))
    }
}
