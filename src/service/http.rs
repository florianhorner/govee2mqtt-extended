// Every handler here returns `Result<Response, Response>`, the idiomatic axum
// shape: the error arm is an already-rendered HTTP response, not an error value
// waiting to be formatted. `result_large_err` measures that arm against a
// 128-byte budget and flags all 13 handlers. Boxing an `axum::response::Response`
// to satisfy it would add an allocation and a deref to every request path while
// changing nothing a user can observe, so the lint is allowed for this module.
//
// The lint began firing when CI's `dtolnay/rust-toolchain@stable` rolled from
// 1.94 to 1.98; the code it flags is unchanged and predates that.
#![allow(clippy::result_large_err)]

use crate::opt_env_var;
use crate::service::coordinator::Coordinator;
use crate::service::device::{Device, DeviceState};
use crate::service::state::StateHandle;
use anyhow::Context;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr};
use tower_http::services::ServeDir;

fn response_with_code<T: ToString + std::fmt::Display>(code: StatusCode, err: T) -> Response {
    if !code.is_success() {
        log::error!("err: {err:#}");
    }

    let mut response = Json(serde_json::json!({
        "code": code.as_u16(),
        "msg": format!("{err:#}")
    }))
    .into_response();
    *response.status_mut() = code;
    response
}

fn generic<T: ToString + std::fmt::Display>(err: T) -> Response {
    response_with_code(StatusCode::INTERNAL_SERVER_ERROR, err)
}

fn not_found<T: ToString + std::fmt::Display>(err: T) -> Response {
    response_with_code(StatusCode::NOT_FOUND, err)
}

fn bad_request<T: ToString + std::fmt::Display>(err: T) -> Response {
    response_with_code(StatusCode::BAD_REQUEST, err)
}

async fn resolve_device_for_control(
    state: &StateHandle,
    id: &str,
) -> Result<Coordinator, Response> {
    state
        .resolve_device_for_control(id)
        .await
        .map_err(not_found)
}

async fn resolve_device_read_only(state: &StateHandle, id: &str) -> Result<Device, Response> {
    state.resolve_device_read_only(id).await.map_err(not_found)
}

/// Returns a json array of device information
async fn list_devices(State(state): State<StateHandle>) -> Result<Response, Response> {
    let mut devices = state.devices().await;
    devices.sort_by_key(|d| (d.room_name().map(|name| name.to_string()), d.name()));
    let mqtt_connected = state.get_hass_client().await.is_some();

    #[derive(Serialize)]
    struct DeviceItem {
        pub sku: String,
        pub id: String,
        pub name: String,
        pub room: Option<String>,
        pub ip: Option<IpAddr>,
        pub state: Option<DeviceState>,
        pub mqtt_connected: bool,
        pub api_available: bool,
        pub lan_active: bool,
        pub cloud_online: Option<bool>,
    }

    let devices: Vec<_> = devices
        .into_iter()
        .map(|d| {
            let device_state = d.device_state();
            let cloud_online = device_state.as_ref().and_then(|state| state.online);
            let api_available = d.http_device_info.is_some();
            let ip = d.ip_addr();
            DeviceItem {
                name: d.name(),
                room: d.room_name().map(|r| r.to_string()),
                ip,
                state: device_state,
                mqtt_connected,
                api_available,
                lan_active: ip.is_some(),
                cloud_online,
                sku: d.sku,
                id: d.id,
            }
        })
        .collect();

    Ok(Json(devices).into_response())
}

/// Turns on a given device
async fn device_power_on(
    State(state): State<StateHandle>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_power_on(&device, true)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Turns off a given device
async fn device_power_off(
    State(state): State<StateHandle>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_power_on(&device, false)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Sets the brightness level of a given device
async fn device_set_brightness(
    State(state): State<StateHandle>,
    Path((id, level)): Path<(String, u8)>,
) -> Result<Response, Response> {
    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_set_brightness(&device, level)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Sets the color temperature of a given device
async fn device_set_color_temperature(
    State(state): State<StateHandle>,
    Path((id, kelvin)): Path<(String, u32)>,
) -> Result<Response, Response> {
    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_set_color_temperature(&device, kelvin)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Sets the RGB color of a given device
async fn device_set_color(
    State(state): State<StateHandle>,
    Path((id, color)): Path<(String, String)>,
) -> Result<Response, Response> {
    let color = csscolorparser::parse(&color)
        .map_err(|err| bad_request(format!("error parsing color '{color}': {err}")))?;
    let [r, g, b, _a] = color.to_rgba8();

    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_set_color_rgb(&device, r, g, b)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Activates the named scene for a given device
async fn device_set_scene(
    State(state): State<StateHandle>,
    Path((id, scene)): Path<(String, String)>,
) -> Result<Response, Response> {
    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_set_scene(&device, &scene)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Returns a JSON array of the available scenes with category structure preserved
async fn device_list_scenes_categorized(
    State(state): State<StateHandle>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let device = resolve_device_read_only(&state, &id).await?;

    let catalog = state
        .device_list_scenes_categorized(&device)
        .await
        .map_err(generic)?;

    Ok(Json(catalog).into_response())
}

/// Returns a JSON array of the available scene names for a given device
async fn device_list_scenes(
    State(state): State<StateHandle>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let device = resolve_device_read_only(&state, &id).await?;

    let scenes = state.device_list_scenes(&device).await.map_err(generic)?;

    Ok(Json(scenes).into_response())
}

async fn list_one_clicks(State(state): State<StateHandle>) -> Result<Response, Response> {
    let undoc = state
        .get_undoc_client()
        .await
        .ok_or_else(|| {
            anyhow::anyhow!("Govee cloud API unavailable: is your email/password configured?")
        })
        .map_err(generic)?;
    let items = undoc.parse_one_clicks().await.map_err(generic)?;

    Ok(Json(items).into_response())
}

async fn activate_one_click(
    State(state): State<StateHandle>,
    Path(name): Path<String>,
) -> Result<Response, Response> {
    let undoc = state
        .get_undoc_client()
        .await
        .ok_or_else(|| {
            anyhow::anyhow!("Govee cloud API unavailable: is your email/password configured?")
        })
        .map_err(generic)?;
    let items = undoc.parse_one_clicks().await.map_err(generic)?;
    let item = items
        .iter()
        .find(|item| item.name == name)
        .ok_or_else(|| anyhow::anyhow!("didn't find item {name}"))
        .map_err(not_found)?;

    let iot = state
        .get_iot_client()
        .await
        .ok_or_else(|| anyhow::anyhow!("AWS IoT client is not available"))
        .map_err(generic)?;

    iot.activate_one_click(item).await.map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

async fn redirect_to_index() -> Response {
    // Relative: a rooted target escapes the ingress prefix.
    axum::response::Redirect::to("assets/index.html").into_response()
}

fn build_router(state: StateHandle) -> Router {
    Router::new()
        .route("/api/devices", get(list_devices))
        .route("/api/device/{id}/power/on", get(device_power_on))
        .route("/api/device/{id}/power/off", get(device_power_off))
        .route(
            "/api/device/{id}/brightness/{level}",
            get(device_set_brightness),
        )
        .route(
            "/api/device/{id}/colortemp/{kelvin}",
            get(device_set_color_temperature),
        )
        .route("/api/device/{id}/color/{color}", get(device_set_color))
        .route("/api/device/{id}/scene/{scene}", get(device_set_scene))
        .route("/api/device/{id}/scenes", get(device_list_scenes))
        .route(
            "/api/device/{id}/scene-catalog",
            get(device_list_scenes_categorized),
        )
        .route("/api/oneclicks", get(list_one_clicks))
        .route("/api/oneclick/activate/{scene}", get(activate_one_click))
        .route("/", get(redirect_to_index))
        .nest_service("/assets", ServeDir::new("assets"))
        .with_state(state)
}

#[cfg(test)]
#[test]
fn test_build_router() {
    // axum has a history of chaning the URL syntax across
    // semver bumps; while that is OK, the syntax changes
    // are not caught at compile time, so we need a runtime
    // check to verify that the syntax is still good.
    // This next line will panic if axum decides that
    // the syntax is bad.
    let _ = build_router(StateHandle::default());
}

/// The address the HTTP server binds to, from $GOVEE_HTTP_ADDR; 0.0.0.0 when
/// unset. Empty is an error, not the default: it means the setting was filled
/// in and mistyped, and listening on every interface is what it exists to avoid.
pub fn http_addr_from_env() -> anyhow::Result<IpAddr> {
    match opt_env_var::<String>("GOVEE_HTTP_ADDR")? {
        None => Ok(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                anyhow::bail!(
                    "$GOVEE_HTTP_ADDR is set but empty; unset it for the \
                     default 0.0.0.0, or give an IP address"
                );
            }
            trimmed.parse().map_err(|err| {
                anyhow::anyhow!("$GOVEE_HTTP_ADDR={trimmed:?} is not an IP address: {err}")
            })
        }
    }
}

async fn bind_http_listener(addr: IpAddr, port: u16) -> anyhow::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind((addr, port))
        .await
        .with_context(|| format!("run_http_server: binding to {addr} port {port}"))
}

pub async fn run_http_server(state: StateHandle, addr: IpAddr, port: u16) -> anyhow::Result<()> {
    let app = build_router(state);
    let listener = bind_http_listener(addr, port).await?;
    let addr = listener.local_addr()?;
    log::info!("http server addr is {addr:?}");
    if let Err(err) = axum::serve(listener, app).await {
        log::error!("http server stopped: {err:#}");
    }

    Ok(())
}

/// A rooted URL 404s only under ingress, where nobody is looking. These two
/// fail here instead.
#[cfg(test)]
#[tokio::test]
async fn test_redirect_to_index_is_relative() {
    let response = redirect_to_index().await;
    let location = response
        .headers()
        .get("location")
        .expect("redirect sets location")
        .to_str()
        .expect("ascii");
    assert_eq!(location, "assets/index.html");
    assert!(!location.starts_with('/'), "{location} escapes the prefix");
}

#[cfg(test)]
#[test]
fn test_ui_sources_have_no_rooted_urls() {
    for (name, source) in [
        ("assets/index.html", include_str!("../../assets/index.html")),
        (
            "assets/components/devices.js",
            include_str!("../../assets/components/devices.js"),
        ),
    ] {
        for needle in [
            "\"/assets",
            "'/assets",
            "\"/api",
            "'/api",
            "`/api",
            "`/assets",
        ] {
            assert!(
                !source.contains(needle),
                "{name} contains {needle}: a rooted URL breaks under ingress"
            );
        }
    }
}

#[cfg(test)]
#[tokio::test]
async fn test_http_addr_from_env() {
    // One test: $GOVEE_HTTP_ADDR is process-global
    std::env::remove_var("GOVEE_HTTP_ADDR");
    let default = http_addr_from_env().unwrap();

    std::env::set_var("GOVEE_HTTP_ADDR", "127.0.0.1");
    let loopback = http_addr_from_env();

    std::env::set_var("GOVEE_HTTP_ADDR", " 127.0.0.1 ");
    let padded = http_addr_from_env();

    std::env::set_var("GOVEE_HTTP_ADDR", "not-an-ip");
    let invalid = http_addr_from_env();

    // Empty and whitespace-only must FAIL, not fall back to 0.0.0.0
    std::env::set_var("GOVEE_HTTP_ADDR", "");
    let empty = http_addr_from_env();

    std::env::set_var("GOVEE_HTTP_ADDR", "   ");
    let blank = http_addr_from_env();
    std::env::remove_var("GOVEE_HTTP_ADDR");

    assert_eq!(default, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    assert_eq!(padded.unwrap(), IpAddr::V4(Ipv4Addr::LOCALHOST));

    for (label, result) in [("empty", empty), ("whitespace", blank)] {
        let err = format!("{:#}", result.expect_err(label));
        assert!(err.contains("set but empty"), "{label}: {err}");
    }

    let loopback = loopback.unwrap();
    let listener = bind_http_listener(loopback, 0).await.unwrap();
    assert_eq!(
        listener.local_addr().unwrap().ip(),
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    );

    let err = format!("{:#}", invalid.unwrap_err());
    assert!(err.contains("\"not-an-ip\""), "{err}");
}
