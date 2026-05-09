#![allow(unused)]
use crate::cache::{cache_get, CacheComputeResult, CacheGetOptions, NoCacheError};
use crate::lan_api::{boolean_int, truthy};
use crate::opt_env_var;
use crate::platform_api::{
    from_json, http_response_body, DeviceCapability, DeviceCapabilityKind, DeviceParameters,
    EnumOption,
};
use anyhow::Context;
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

// <https://github.com/constructorfleet/homebridge-ultimate-govee/blob/main/src/data/clients/RestClient.ts>

const APP_VERSION: &str = "7.4.10";
const HALF_DAY: Duration = Duration::from_secs(3600 * 12);
const ONE_DAY: Duration = Duration::from_secs(86400);
const ONE_WEEK: Duration = Duration::from_secs(86400 * 7);
const FIFTEEN_MINS: Duration = Duration::from_secs(60 * 15);

/// Some data is not meant for human eyes except in very unusual circumstances.
#[derive(Deserialize, Serialize, Clone)]
#[serde(transparent)]
pub struct Redacted<T: std::fmt::Debug>(T);

pub fn should_log_sensitive_data() -> bool {
    if let Ok(Some(v)) = opt_env_var::<String>("GOVEE_LOG_SENSITIVE_DATA") {
        truthy(&v).unwrap_or(false)
    } else {
        false
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Redacted<T> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        if should_log_sensitive_data() {
            self.0.fmt(fmt)
        } else {
            fmt.write_str("REDACTED")
        }
    }
}

impl<T: std::fmt::Debug> std::ops::Deref for Redacted<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Inspect a Govee login response body for the `status` field. Govee uses
/// HTTP 200 with `{"status": 454}` to signal "2FA required" and `{"status":
/// 455}` for "code invalid/expired", so we have to look inside the JSON
/// payload rather than relying on the HTTP status code. Returns `None` when
/// the body is not JSON or the field is missing/non-numeric — the caller
/// then falls through to normal response parsing.
fn classify_login_status(body_bytes: &[u8]) -> Option<u64> {
    serde_json::from_slice::<serde_json::Value>(body_bytes)
        .ok()
        .and_then(|v| v.get("status").and_then(|s| s.as_u64()))
}

/// Normalize a user-supplied 2FA code: strip surrounding whitespace and treat
/// an empty result as "no code provided." Govee codes are 6 digits with no
/// padding, but users routinely paste from email with trailing newlines or
/// surrounding spaces; sending those verbatim trips status 455 with a
/// misleading "code expired" message, which is a bad UX. Handling here keeps
/// the trim policy in one place (vs scattered across CLI/env/HA-config code
/// paths).
fn normalize_2fa_code(raw: Option<String>) -> Option<String> {
    raw.and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

/// Decide how to surface an HTTP-layer login failure. 5xx is transient by
/// definition (gateway flake, Govee maintenance) and must NOT be negative-
/// cached, otherwise a 10-second-cached failure slows recovery from a Govee
/// outage that resolved in 2 seconds. 4xx is deterministic (auth wrong, bad
/// request) and benefits from short negative caching to avoid hammering the
/// API. The 454/455 cases are handled separately via `build_2fa_error` so
/// they never hit this branch.
fn classify_login_http_error(status_code: u16, message: String) -> anyhow::Error {
    if (500..600).contains(&status_code) {
        NoCacheError(anyhow::anyhow!("{message}")).into()
    } else {
        anyhow::anyhow!("{message}")
    }
}

/// Build the right NoCacheError for a Govee login response status, or None if
/// the status is not a 2FA condition. Pulled out of `login_account_impl` so
/// the user-facing messaging can be unit-tested without an HTTP mock.
fn build_2fa_error(status: u64, code_was_set: bool) -> Option<NoCacheError> {
    match status {
        454 => {
            let msg = if code_was_set {
                "Govee 2FA verification failed (status 454 returned despite \
                 a code being supplied). The code may have expired (~15 min \
                 validity) or be incorrect. Generate a fresh code by signing \
                 in to the Govee mobile app, update govee_2fa_code, and \
                 restart the addon."
            } else {
                "Govee account requires 2FA verification. Sign in to the \
                 Govee mobile app to trigger a verification email, then set \
                 govee_2fa_code in the addon configuration (or the \
                 GOVEE_2FA_CODE environment variable) and restart. The code \
                 is valid for approximately 15 minutes."
            };
            Some(NoCacheError(anyhow::anyhow!("{msg}")))
        }
        455 => Some(NoCacheError(anyhow::anyhow!(
            "Govee 2FA verification code was rejected as invalid or expired \
             (status 455). Generate a fresh code via the Govee mobile app, \
             update govee_2fa_code, and restart."
        ))),
        _ => None,
    }
}

fn user_agent() -> String {
    format!(
        "GoveeHome/{APP_VERSION} (com.ihoment.GoVeeSensor; build:8; iOS 26.5.0) Alamofire/5.11.0"
    )
}

pub fn ms_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix epoch in the past")
        .as_millis()
        .to_string()
}

#[derive(Clone, clap::Parser, Debug)]
pub struct UndocApiArguments {
    /// The email address you registered with Govee.
    /// If not passed here, it will be read from
    /// the GOVEE_EMAIL environment variable.
    #[arg(long, global = true)]
    pub govee_email: Option<String>,

    /// The password for your Govee account.
    /// If not passed here, it will be read from
    /// the GOVEE_PASSWORD environment variable.
    #[arg(long, global = true)]
    pub govee_password: Option<String>,

    /// One-time verification code from Govee. Required when the account has 2FA
    /// enabled and login returns status 454. Trigger a fresh code by signing in
    /// to the Govee mobile app, paste it here, and restart the addon. The code
    /// is valid for ~15 minutes. If not passed, read from GOVEE_2FA_CODE.
    #[arg(long, global = true)]
    pub govee_2fa_code: Option<String>,

    /// Where to store the AWS IoT key file.
    #[arg(long, global = true, default_value = "/dev/shm/govee.iot.key")]
    pub govee_iot_key: PathBuf,

    /// Where to store the AWS IoT certificate file.
    #[arg(long, global = true, default_value = "/dev/shm/govee.iot.cert")]
    pub govee_iot_cert: PathBuf,

    /// Where to find the AWS root CA certificate
    #[arg(long, global = true, default_value = "AmazonRootCA1.pem")]
    pub amazon_root_ca: PathBuf,
}

/// Resolve a config value from a CLI/HA-config field first, falling back to
/// an environment variable. Three accessors on `UndocApiArguments` share this
/// shape; collapsing them into one helper keeps future credential additions
/// (community login, IoT cert) honest.
fn opt_arg_or_env(field: &Option<String>, env_var: &str) -> anyhow::Result<Option<String>> {
    match field {
        Some(v) => Ok(Some(v.clone())),
        None => opt_env_var(env_var),
    }
}

impl UndocApiArguments {
    pub fn opt_email(&self) -> anyhow::Result<Option<String>> {
        opt_arg_or_env(&self.govee_email, "GOVEE_EMAIL")
    }

    pub fn email(&self) -> anyhow::Result<String> {
        self.opt_email()?.ok_or_else(|| {
            anyhow::anyhow!(
                "Please specify the govee account email either via the \
                --govee-email parameter or by setting $GOVEE_EMAIL"
            )
        })
    }

    pub fn opt_password(&self) -> anyhow::Result<Option<String>> {
        opt_arg_or_env(&self.govee_password, "GOVEE_PASSWORD")
    }

    pub fn password(&self) -> anyhow::Result<String> {
        self.opt_password()?.ok_or_else(|| {
            anyhow::anyhow!(
                "Please specify the govee account password either via the \
                --govee-password parameter or by setting $GOVEE_PASSWORD"
            )
        })
    }

    pub fn opt_2fa_code(&self) -> anyhow::Result<Option<String>> {
        opt_arg_or_env(&self.govee_2fa_code, "GOVEE_2FA_CODE")
    }

    pub fn api_client(&self) -> anyhow::Result<GoveeUndocumentedApi> {
        let email = self.email()?;
        let password = self.password()?;
        let code = self.opt_2fa_code()?;
        Ok(GoveeUndocumentedApi::new(email, password).with_code(code))
    }
}

#[derive(Clone)]
pub struct GoveeUndocumentedApi {
    email: String,
    password: String,
    /// Optional 2FA verification code. Govee accepts this as the `code` field on
    /// the login request body when the account has two-factor enabled. The code
    /// is config-driven: the addon reads it from `govee_2fa_code` / GOVEE_2FA_CODE
    /// at startup and it lives on this struct for the process lifetime. If Govee
    /// rejects it (status 455), the addon bails uncacheable and the user supplies
    /// a fresh code on the next restart.
    code: Option<String>,
    client_id: String,
}

impl GoveeUndocumentedApi {
    pub fn new<E: Into<String>, P: Into<String>>(email: E, password: P) -> Self {
        let email = email.into();
        let password = password.into();
        let client_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, email.as_bytes());
        let client_id = format!("{}", client_id.simple());
        Self {
            email,
            password,
            code: None,
            client_id,
        }
    }

    /// Builder-style setter for the 2FA verification code. Pass `None` if 2FA is
    /// not enabled on the account; pass `Some(code)` after an earlier login
    /// returned status 454 and the user has retrieved the code from email.
    ///
    /// The code is normalized: surrounding whitespace is stripped and an empty
    /// result is treated as `None`. This means `with_code(Some(""))` and
    /// `with_code(Some("  \n"))` both leave the client in the no-code state,
    /// rather than sending an empty `code` field that Govee would reject as
    /// invalid with a misleading message.
    pub fn with_code(mut self, code: Option<String>) -> Self {
        self.code = normalize_2fa_code(code);
        self
    }

    #[allow(unused)]
    pub async fn get_iot_key(&self, token: &str) -> anyhow::Result<IotKey> {
        cache_get(
            CacheGetOptions {
                topic: "undoc-api",
                key: "iot-key",
                soft_ttl: HALF_DAY,
                hard_ttl: HALF_DAY,
                negative_ttl: Duration::from_secs(10),
                allow_stale: false,
            },
            async {
                let response = reqwest::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()?
                    .request(Method::GET, "https://app2.govee.com/app/v1/account/iot/key")
                    .header("Authorization", format!("Bearer {token}"))
                    .header("appVersion", APP_VERSION)
                    .header("clientId", &self.client_id)
                    .header("clientType", "1")
                    .header("iotVersion", "0")
                    .header("timestamp", ms_timestamp())
                    .header("User-Agent", user_agent())
                    .send()
                    .await?;

                #[derive(Deserialize, Debug)]
                #[allow(non_snake_case, dead_code)]
                struct Response {
                    data: IotKey,
                    message: String,
                    status: u64,
                }

                let resp: Response = http_response_body(response).await?;

                Ok(CacheComputeResult::Value(resp.data))
            },
        )
        .await
    }

    pub fn invalidate_account_login(&self) {
        crate::cache::invalidate_key("undoc-api", "account-info").ok();
    }

    /// Build the JSON body sent on every login request. The `code` field is
    /// added only when a 2FA verification code has been configured; sending an
    /// empty `code` to Govee for an account without 2FA causes a different
    /// rejection. Pulled out as a helper so the shape can be unit-tested.
    fn build_login_body(&self) -> serde_json::Value {
        let mut body = serde_json::json!({
            "email": self.email,
            "password": self.password,
            "client": &self.client_id,
        });
        if let Some(code) = &self.code {
            body["code"] = serde_json::Value::String(code.clone());
        }
        body
    }

    async fn login_account_impl(&self) -> anyhow::Result<CacheComputeResult<LoginAccountResponse>> {
        let body = self.build_login_body();

        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?
            .request(
                Method::POST,
                "https://app2.govee.com/account/rest/account/v2/login",
            )
            .header("appVersion", APP_VERSION)
            .header("clientId", &self.client_id)
            .header("clientType", "1")
            .header("iotVersion", "0")
            .header("timestamp", ms_timestamp())
            .header("User-Agent", user_agent())
            .json(&body)
            .send()
            .await?;

        // Read the response body manually so we can check for 454 (2FA required)
        // and 455 (invalid/expired code) before attempting deserialization. Both
        // statuses are wrapped in NoCacheError so cache_get skips the negative
        // cache write — the user must be able to retry with a fresh code within
        // the ~15 minute validity window.
        let url = response.url().clone();
        let status = response.status();
        let body_bytes = response.bytes().await?;

        if let Some(api_status) = classify_login_status(&body_bytes) {
            if let Some(err) = build_2fa_error(api_status, self.code.is_some()) {
                // Defense-in-depth: clear any pre-existing entry under this key
                // before bailing. The caller's `cache_get` already skips the
                // negative-cache write because the error wraps NoCacheError, but
                // a stale entry written by an older fork version (with a longer
                // negative_ttl) could still trap retries. Invalidating here
                // guarantees the very next call re-executes login_account_impl
                // with whatever fresh code the user has set.
                self.invalidate_account_login();
                return Err(anyhow::Error::from(err));
            }
        }

        if !status.is_success() {
            let msg = format!(
                "request {url} status {}: {}. Response body: {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or(""),
                String::from_utf8_lossy(&body_bytes)
            );
            return Err(classify_login_http_error(status.as_u16(), msg));
        }

        #[derive(Deserialize, Serialize, Debug)]
        #[allow(non_snake_case, dead_code)]
        struct Response {
            client: LoginAccountResponse,
            message: String,
            status: u64,
        }

        let resp: Response = serde_json::from_slice(&body_bytes).with_context(|| {
            format!(
                "parsing {url} login response: {}",
                String::from_utf8_lossy(&body_bytes)
            )
        })?;

        let ttl = Duration::from_secs(resp.client.token_expire_cycle as u64);
        Ok(CacheComputeResult::WithTtl(resp.client, ttl))
    }

    pub async fn login_account_cached(&self) -> anyhow::Result<LoginAccountResponse> {
        cache_get(
            CacheGetOptions {
                topic: "undoc-api",
                key: "account-info",
                soft_ttl: HALF_DAY,
                hard_ttl: HALF_DAY,
                // Short negative TTL is a fallback only — 2FA failures (454/455)
                // and 5xx HTTP errors bypass the negative cache entirely via the
                // NoCacheError marker, so any retry happens on the very next call
                // rather than waiting this out. This 10-second floor only catches
                // hard 4xx/parse failures we genuinely expect to remain wrong.
                negative_ttl: Duration::from_secs(10),
                allow_stale: false,
            },
            async { self.login_account_impl().await },
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn login_account(&self) -> anyhow::Result<LoginAccountResponse> {
        let value = self.login_account_impl().await?;
        Ok(value.into_inner())
    }

    pub async fn get_device_list(&self, token: &str) -> anyhow::Result<DevicesResponse> {
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?
            .request(
                Method::POST,
                "https://app2.govee.com/device/rest/devices/v1/list",
            )
            .header("Authorization", format!("Bearer {token}"))
            .header("appVersion", APP_VERSION)
            .header("clientId", &self.client_id)
            .header("clientType", "1")
            .header("iotVersion", "0")
            .header("timestamp", ms_timestamp())
            .header("User-Agent", user_agent())
            .send()
            .await?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.invalidate_account_login();
        }

        let resp: DevicesResponse = http_response_body(response).await?;

        Ok(resp)
    }

    pub fn invalidate_community_login(&self) {
        crate::cache::invalidate_key("undoc-api", "community-login").ok();
    }

    /// Login to community-api.govee.com and return the bearer token
    pub async fn login_community(&self) -> anyhow::Result<String> {
        cache_get(
            CacheGetOptions {
                topic: "undoc-api",
                key: "community-login",
                soft_ttl: ONE_DAY,
                hard_ttl: HALF_DAY,
                negative_ttl: Duration::from_secs(10),
                allow_stale: false,
            },
            async {
                let response = reqwest::Client::builder()
                    .timeout(Duration::from_secs(60))
                    .build()?
                    .request(Method::POST, "https://community-api.govee.com/os/v1/login")
                    .json(&serde_json::json!({
                        "email": self.email,
                        "password": self.password,
                    }))
                    .send()
                    .await?;

                #[derive(Deserialize, Debug)]
                #[allow(non_snake_case, dead_code)]
                struct Response {
                    data: ResponseData,
                    message: String,
                    status: u64,
                }

                #[derive(Deserialize, Debug)]
                #[allow(non_snake_case, dead_code)]
                struct ResponseData {
                    email: String,
                    expiredAt: u64,
                    headerUrl: String,
                    id: u64,
                    nickName: String,
                    token: String,
                }

                let resp: Response = http_response_body(response).await?;

                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("unix epoch in the past")
                    .as_millis();

                let ttl_ms = resp.data.expiredAt as u128 - ts_ms;
                let ttl = Duration::from_millis(ttl_ms as u64).min(ONE_DAY);

                Ok(CacheComputeResult::WithTtl(resp.data.token, ttl))
            },
        )
        .await
    }

    pub async fn get_scenes_for_device(sku: &str) -> anyhow::Result<Vec<LightEffectCategory>> {
        let key = format!("scenes-{sku}");

        cache_get(
            CacheGetOptions {
                topic: "undoc-api",
                key: &key,
                soft_ttl: ONE_DAY,
                hard_ttl: ONE_WEEK,
                negative_ttl: Duration::from_secs(1),
                allow_stale: true,
            },
            async {
                let response = reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()?
                    .request(
                        Method::GET,
                        format!(
                            "https://app2.govee.com/appsku/v1/light-effect-libraries?sku={sku}"
                        ),
                    )
                    .header("AppVersion", APP_VERSION)
                    .header("User-Agent", user_agent())
                    .send()
                    .await?;

                let resp: LightEffectLibraryResponse = http_response_body(response).await?;

                Ok(CacheComputeResult::Value(resp.data.categories))
            },
        )
        .await
    }

    /// This is present primarily to workaround a bug where Govee aren't returning
    /// the full list of scenes via their supported platform API
    pub async fn synthesize_platform_api_scene_list(
        sku: &str,
    ) -> anyhow::Result<Vec<DeviceCapability>> {
        let catalog = Self::get_scenes_for_device(sku).await?;
        let mut options = vec![];

        for c in catalog {
            for s in c.scenes {
                if let Some(param_id) = s.light_effects.first().map(|e| e.scence_param_id) {
                    options.push(EnumOption {
                        name: s.scene_name,
                        value: json!({
                            "paramId": param_id,
                            "id": s.scene_id,
                        }),
                        extras: Default::default(),
                    });
                }
            }
        }

        Ok(vec![DeviceCapability {
            kind: DeviceCapabilityKind::DynamicScene,
            parameters: Some(DeviceParameters::Enum { options }),
            alarm_type: None,
            event_state: None,
            instance: "lightScene".to_string(),
        }])
    }

    pub async fn get_saved_one_click_shortcuts(
        &self,
        community_token: &str,
    ) -> anyhow::Result<Vec<OneClickComponent>> {
        cache_get(
            CacheGetOptions {
                topic: "undoc-api",
                key: "one-click-shortcuts",
                soft_ttl: ONE_DAY,
                hard_ttl: ONE_WEEK,
                negative_ttl: Duration::from_secs(1),
                allow_stale: true,
            },
            async {
                let response = reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()?
                    .request(
                        Method::GET,
                        "https://app2.govee.com/bff-app/v1/exec-plat/home",
                    )
                    .header("Authorization", format!("Bearer {community_token}"))
                    .header("appVersion", APP_VERSION)
                    .header("clientId", &self.client_id)
                    .header("clientType", "1")
                    .header("iotVersion", "0")
                    .header("timestamp", ms_timestamp())
                    .header("User-Agent", user_agent())
                    .send()
                    .await?;

                if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                    self.invalidate_community_login();
                }

                let resp: OneClickResponse = http_response_body(response).await?;

                Ok(CacheComputeResult::Value(resp.data.components))
            },
        )
        .await
    }

    pub async fn parse_one_clicks(&self) -> anyhow::Result<Vec<ParsedOneClick>> {
        let token = self.login_community().await?;
        let res = self.get_saved_one_click_shortcuts(&token).await?;
        let mut result = vec![];

        for group in res {
            for oc in group.one_clicks {
                if oc.iot_rules.is_empty() {
                    continue;
                }

                let name = format!("One-Click: {}: {}", group.name, oc.name);

                let mut entries = vec![];
                for rule in oc.iot_rules {
                    if let Some(topic) = rule.device_obj.topic {
                        let msgs = rule.rule.into_iter().map(|r| r.iot_msg).collect();
                        entries.push(ParsedOneClickEntry { topic, msgs });
                    }
                }

                result.push(ParsedOneClick { name, entries });
            }
        }
        Ok(result)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedOneClick {
    pub name: String,
    pub entries: Vec<ParsedOneClickEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedOneClickEntry {
    pub topic: Redacted<String>,
    pub msgs: Vec<JsonValue>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
#[serde(rename_all = "camelCase")]
pub struct IotKey {
    pub endpoint: String,
    pub log: String,
    pub p12: Redacted<String>,
    pub p12_pass: Redacted<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LightEffectLibraryResponse {
    pub data: LightEffectLibraryCategoryList,
    pub message: String,
    pub status: u32,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LightEffectLibraryCategoryList {
    pub categories: Vec<LightEffectCategory>,
    pub support_speed: u8,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LightEffectCategory {
    pub category_id: u32,
    pub category_name: String,
    pub scenes: Vec<LightEffectScene>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LightEffectScene {
    pub scene_id: u32,
    pub icon_urls: Vec<String>,
    pub scene_name: String,
    pub analytic_name: String,
    pub scene_type: u32,
    pub scene_code: u32,
    pub scence_category_id: u32,
    pub pop_up_prompt: u32,
    pub scenes_hint: String,
    /// Eg: min/max applicable device version constraints
    pub rule: JsonValue,
    pub light_effects: Vec<LightEffectEntry>,
    pub voice_url: String,
    pub create_time: u64,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LightEffectEntry {
    pub scence_param_id: u32,
    pub scence_name: String,
    /// base64 encoded
    pub scence_param: String,
    pub scene_code: u16,
    pub special_effect: Vec<JsonValue>,
    pub cmd_version: Option<u32>,
    pub scene_type: u32,
    pub diy_effect_code: Vec<JsonValue>,
    pub diy_effect_str: String,
    pub rules: Vec<JsonValue>,
    pub speed_info: JsonValue,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickResponse {
    pub data: OneClickComponentList,
    pub message: String,
    pub status: u32,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickComponentList {
    pub components: Vec<OneClickComponent>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickComponent {
    pub can_disable: Option<u8>,
    #[serde(deserialize_with = "boolean_int")]
    pub can_manage: bool,

    pub feast_type: Option<u64>,
    #[serde(default)]
    pub feasts: Vec<JsonValue>,

    #[serde(default)]
    pub groups: Vec<JsonValue>,

    pub main_device: Option<JsonValue>,

    pub component_id: u64,
    #[serde(default)]
    pub environments: Vec<JsonValue>,
    pub name: String,
    #[serde(rename = "type")]
    pub component_type: u64,

    pub guide_url: Option<String>,
    pub h5_url: Option<String>,
    pub video_url: Option<String>,

    #[serde(default)]
    pub one_clicks: Vec<OneClick>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClick {
    pub name: String,
    pub plan_type: i64,
    pub preset_id: i64,
    pub preset_state: i64,
    pub siri_engine_id: i64,
    #[serde(rename = "type")]
    pub rule_type: i64,
    pub desc: String,
    #[serde(default)]
    pub exec_rules: Vec<JsonValue>,
    pub group_id: i64,
    pub group_name: String,
    #[serde(default)]
    pub iot_rules: Vec<OneClickIotRule>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickIotRule {
    pub device_obj: OneClickIotRuleDevice,
    pub rule: Vec<OneClickIotRuleEntry>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickIotRuleEntry {
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub blue_msg: JsonValue,
    pub cmd_type: u64,
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub cmd_val: OneClickIotRuleEntryCmd,
    pub device_type: u32,
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub iot_msg: JsonValue,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickIotRuleEntryCmd {
    pub open: Option<u32>,
    pub scenes_code: Option<u16>,
    pub scence_id: Option<u16>,
    pub scenes_str: Option<String>,
    pub scence_param_id: Option<u16>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct OneClickIotRuleDevice {
    pub name: Option<String>,
    pub device: Option<String>,
    pub sku: Option<String>,

    pub topic: Option<Redacted<String>>,

    pub ble_address: Option<String>,
    pub ble_name: Option<String>,
    pub device_splicing_status: u32,
    pub feast_id: u64,
    pub feast_name: String,
    pub feast_type: u64,
    pub goods_type: Option<u64>,
    pub ic: Option<u32>,
    #[serde(rename = "ic_sub_1")]
    pub ic_sub_1: Option<u32>,
    #[serde(rename = "ic_sub_2")]
    pub ic_sub_2: Option<u32>,
    #[serde(deserialize_with = "boolean_int")]
    pub is_feast: bool,
    pub pact_type: Option<u32>,
    pub pact_code: Option<u32>,

    pub settings: Option<JsonValue>,
    pub spec: Option<String>,
    pub sub_device: String,
    pub sub_device_num: u64,
    pub sub_devices: Option<JsonValue>,

    pub version_hard: Option<String>,
    pub version_soft: Option<String>,
    pub wifi_soft_version: Option<String>,
    pub wifi_hard_version: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LoginAccountResponse {
    #[serde(rename = "A")]
    pub a: Redacted<String>,
    #[serde(rename = "B")]
    pub b: Redacted<String>,
    pub account_id: Redacted<u64>,
    /// this is the client id that we passed in
    pub client: Redacted<String>,
    pub is_savvy_user: bool,
    pub refresh_token: Option<Redacted<String>>,
    pub client_name: Option<String>,
    pub push_token: Option<Redacted<String>>,
    pub version_code: Option<String>,
    pub version_name: Option<String>,
    pub sys_version: Option<String>,
    pub token: Redacted<String>,
    pub token_expire_cycle: u32,
    pub topic: Redacted<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DevicesResponse {
    pub devices: Vec<DeviceEntry>,
    pub groups: Vec<GroupEntry>,
    pub message: String,
    pub status: u16,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GroupEntry {
    pub group_id: u64,
    pub group_name: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct DeviceEntry {
    pub attributes_id: u32,
    pub device_id: Option<u32>,
    pub device: String,
    pub device_ext: DeviceEntryExt,
    pub device_name: String,
    pub goods_type: u32,
    pub group_id: u64,
    pub pact_code: Option<u32>,
    pub pact_type: Option<u32>,
    pub share: Option<u32>,
    pub sku: String,
    pub spec: String,
    #[serde(deserialize_with = "boolean_int")]
    pub support_scene: bool,
    pub version_hard: String,
    pub version_soft: String,
    pub gid_confirmed: Option<bool>,
}

impl DeviceEntry {
    pub fn device_topic(&self) -> anyhow::Result<&str> {
        self.device_ext
            .device_settings
            .topic
            .as_ref()
            .map(|t| t.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "device {id} has no topic, is it a BLE-only device?",
                    id = self.device
                )
            })
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct DeviceEntryExt {
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub device_settings: DeviceSettings,
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub ext_resources: ExtResources,
    #[serde(deserialize_with = "embedded_json", serialize_with = "as_json")]
    pub last_device_data: LastDeviceData,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct DeviceSettings {
    /// Maybe be absent for BLE devices
    pub wifi_name: Option<String>,
    pub address: Option<String>,
    pub ble_name: Option<String>,
    pub topic: Option<Redacted<String>>,
    pub wifi_mac: Option<String>,
    pub pact_type: Option<u32>,
    pub pact_code: Option<u32>,
    pub dsp_version_soft: Option<JsonValue>,
    pub wifi_soft_version: Option<String>,
    pub wifi_hard_version: Option<String>,
    pub ic: Option<u32>,
    #[serde(rename = "ic_sub_1")]
    pub ic_sub_1: Option<u32>,
    #[serde(rename = "ic_sub_2")]
    pub ic_sub_2: Option<u32>,
    pub secret_code: Option<Redacted<String>>,
    #[serde(deserialize_with = "boolean_int", default)]
    pub boil_water_completed_noti_on_off: bool,
    #[serde(deserialize_with = "boolean_int", default)]
    pub boil_water_exception_noti_on_off: bool,
    #[serde(deserialize_with = "boolean_int", default)]
    pub completion_noti_on_off: bool,
    #[serde(deserialize_with = "boolean_int", default)]
    pub auto_shut_down_on_off: bool,
    #[serde(deserialize_with = "boolean_int", default)]
    pub water_shortage_on_off: bool,
    #[serde(deserialize_with = "boolean_int", default)]
    pub air_quality_on_off: bool,
    pub mcu_soft_version: Option<String>,
    pub mcu_hard_version: Option<String>,
    pub sku: Option<String>,
    pub device: Option<String>,
    pub device_name: Option<String>,
    pub version_hard: Option<String>,
    pub version_soft: Option<String>,
    pub play_state: Option<bool>,
    pub tem_min: Option<i64>,
    pub tem_max: Option<i64>,
    pub tem_warning: Option<bool>,
    pub fah_open: Option<bool>,
    pub tem_cali: Option<i64>,
    pub hum_min: Option<i64>,
    pub hum_max: Option<i64>,
    pub hum_warning: Option<bool>,
    pub hum_cali: Option<i64>,
    pub net_waring: Option<bool>,
    pub upload_rate: Option<i64>,
    pub battery: Option<i64>,
    /// millisecond timestamp
    pub time: Option<u64>,
    pub wifi_level: Option<i64>,

    pub pm25_min: Option<i64>,
    pub pm25_max: Option<i64>,
    pub pm25_warning: Option<bool>,

    /// `{"sub_0": {"name": "Device Name"}}`
    pub sub_devices: Option<JsonValue>,
    pub bd_type: Option<i64>,
    #[serde(deserialize_with = "boolean_int", default)]
    pub filter_expire_on_off: bool,

    /// eg: Glide Hexa. Value is base64 encoded data
    pub shapes: Option<String>,
    pub support_ble_broad_v3: Option<bool>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct ExtResources {
    pub sku_url: Option<String>,
    pub head_on_img_new: Option<String>,
    pub head_on_img: Option<String>,
    pub head_off_img: Option<String>,
    pub head_off_img_new: Option<String>,
    pub ext: Option<String>,
    pub ic: Option<u32>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct LastDeviceData {
    pub online: Option<bool>,
    pub bind: Option<bool>,

    pub tem: Option<i64>,
    pub hum: Option<i64>,
    /// timestamp in milliseconds
    pub last_time: Option<u64>,
    pub avg_day_tem: Option<i64>,
    pub avg_day_hum: Option<i64>,
}

pub fn as_json<S, T>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    T: Serialize,
    S: serde::Serializer,
{
    use serde::ser::Error as _;

    let s = serde_json::to_string(value).map_err(|e| S::Error::custom(format!("{e:#}")))?;

    s.serialize(serializer)
}

pub fn embedded_json<'de, T: DeserializeOwned, D: serde::de::Deserializer<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    use serde::de::Error as _;
    let s = String::deserialize(deserializer)?;
    from_json(if s.is_empty() { "null" } else { &s }).map_err(|e| {
        D::Error::custom(format!(
            "{} {e:#} while processing embedded json text {s}",
            std::any::type_name::<T>()
        ))
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::platform_api::from_json;

    #[test]
    fn get_device_scenes() {
        let resp: DevicesResponse =
            from_json(include_str!("../test-data/undoc-device-list.json")).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    #[test]
    fn get_one_click() {
        let resp: OneClickResponse =
            from_json(include_str!("../test-data/undoc-one-click.json")).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    #[test]
    fn issue36() {
        let resp: OneClickResponse =
            from_json(include_str!("../test-data/undoc-one-click-issue36.json")).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    #[test]
    fn light_effect_library() {
        let resp: LightEffectLibraryResponse =
            from_json(include_str!("../test-data/light-effect-library-h6072.json")).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    #[test]
    fn issue_14() {
        let resp: DevicesResponse = from_json(include_str!("../test-data/issue14.json")).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    #[test]
    fn issue_21() {
        let resp: DevicesResponse =
            from_json(include_str!("../test-data/undoc-device-list-issue-21.json")).unwrap();
        k9::assert_matches_snapshot!(format!("{resp:#?}"));
    }

    // --- 2FA login support ---

    #[test]
    fn login_body_omits_code_when_unset() {
        let api = GoveeUndocumentedApi::new("a@b.com", "pw");
        let body = api.build_login_body();
        assert_eq!(body["email"], "a@b.com");
        assert_eq!(body["password"], "pw");
        assert_eq!(
            body["client"], api.client_id,
            "client field must equal the deterministic client_id derived from email"
        );
        assert!(
            body.get("code").is_none(),
            "code field must be absent when self.code is None: {body}"
        );
    }

    #[test]
    fn login_body_includes_code_when_set() {
        let api = GoveeUndocumentedApi::new("a@b.com", "pw").with_code(Some("123456".to_string()));
        let body = api.build_login_body();
        assert_eq!(body["code"], "123456");
    }

    #[test]
    fn login_body_omits_code_when_explicit_none() {
        let api = GoveeUndocumentedApi::new("a@b.com", "pw").with_code(None);
        let body = api.build_login_body();
        assert!(body.get("code").is_none());
    }

    #[test]
    fn classify_454() {
        assert_eq!(
            classify_login_status(br#"{"status":454,"message":"need 2FA"}"#),
            Some(454)
        );
    }

    #[test]
    fn classify_455() {
        assert_eq!(classify_login_status(br#"{"status":455}"#), Some(455));
    }

    #[test]
    fn classify_200() {
        assert_eq!(
            classify_login_status(br#"{"status":200,"client":{}}"#),
            Some(200)
        );
    }

    #[test]
    fn classify_non_json_returns_none() {
        assert_eq!(classify_login_status(b"<html>Bad Gateway</html>"), None);
    }

    #[test]
    fn classify_missing_status_returns_none() {
        assert_eq!(classify_login_status(br#"{"message":"hi"}"#), None);
    }

    /// Load-bearing test for the cache-bypass contract: a NoCacheError wrapped
    /// in anyhow::Error MUST round-trip through downcast_ref. cache_get relies
    /// on this exact path to decide whether to skip the negative-cache write.
    #[test]
    fn no_cache_error_downcasts_via_anyhow() {
        let err: anyhow::Error = NoCacheError(anyhow::anyhow!("transient")).into();
        assert!(
            err.downcast_ref::<NoCacheError>().is_some(),
            "NoCacheError must downcast back from anyhow::Error or cache_get cannot detect it"
        );
        assert!(format!("{err:#}").contains("transient"));
    }

    /// Plain anyhow::Error must NOT downcast to NoCacheError. Guards against a
    /// future refactor that accidentally makes everything bypass the cache.
    #[test]
    fn plain_anyhow_error_does_not_downcast_to_no_cache_error() {
        let err: anyhow::Error = anyhow::anyhow!("not a 2FA error");
        assert!(err.downcast_ref::<NoCacheError>().is_none());
    }

    // --- code normalization (R1/R2: whitespace, empty string from CLI/HA) ---

    #[test]
    fn normalize_strips_surrounding_whitespace() {
        assert_eq!(
            normalize_2fa_code(Some("  123456  ".to_string())),
            Some("123456".to_string())
        );
    }

    #[test]
    fn normalize_strips_trailing_newline_from_email_paste() {
        assert_eq!(
            normalize_2fa_code(Some("123456\n".to_string())),
            Some("123456".to_string())
        );
    }

    #[test]
    fn normalize_treats_empty_string_as_none() {
        assert_eq!(normalize_2fa_code(Some(String::new())), None);
    }

    #[test]
    fn normalize_treats_whitespace_only_as_none() {
        assert_eq!(normalize_2fa_code(Some("  \t \n".to_string())), None);
    }

    #[test]
    fn normalize_passes_none_through() {
        assert_eq!(normalize_2fa_code(None), None);
    }

    #[test]
    fn with_code_normalizes_input() {
        let api =
            GoveeUndocumentedApi::new("a@b.com", "pw").with_code(Some("  654321\n".to_string()));
        let body = api.build_login_body();
        assert_eq!(
            body["code"], "654321",
            "with_code must trim whitespace before storing"
        );
    }

    #[test]
    fn with_code_empty_string_does_not_set_code_field() {
        let api = GoveeUndocumentedApi::new("a@b.com", "pw").with_code(Some(String::new()));
        let body = api.build_login_body();
        assert!(
            body.get("code").is_none(),
            "with_code(Some(\"\")) must not set code field; got {body}"
        );
    }

    // --- 2FA error helper (build_2fa_error) ---

    #[test]
    fn build_2fa_error_returns_none_for_success_status() {
        assert!(build_2fa_error(200, false).is_none());
        assert!(build_2fa_error(200, true).is_none());
    }

    #[test]
    fn build_2fa_error_returns_none_for_unrelated_status() {
        assert!(build_2fa_error(401, false).is_none());
        assert!(build_2fa_error(500, true).is_none());
    }

    #[test]
    fn build_2fa_error_454_no_code_mentions_mobile_app() {
        let err = build_2fa_error(454, false).expect("454 with no code must produce error");
        let msg = format!("{}", err.0);
        assert!(
            msg.contains("Govee mobile app"),
            "user must be told to use mobile app: {msg}"
        );
        assert!(
            msg.contains("govee_2fa_code"),
            "user must see the config field name: {msg}"
        );
        assert!(
            msg.contains("15 minutes"),
            "user must see the validity window: {msg}"
        );
    }

    #[test]
    fn build_2fa_error_454_with_code_mentions_expired() {
        let err = build_2fa_error(454, true).expect("454 with code must produce error");
        let msg = format!("{}", err.0);
        assert!(
            msg.contains("expired") || msg.contains("incorrect"),
            "user must understand the code itself was rejected: {msg}"
        );
        assert!(
            msg.contains("Generate a fresh code"),
            "user must be told to refresh: {msg}"
        );
    }

    #[test]
    fn build_2fa_error_454_messages_differ_by_code_state() {
        let no_code_msg = format!("{}", build_2fa_error(454, false).unwrap().0);
        let with_code_msg = format!("{}", build_2fa_error(454, true).unwrap().0);
        assert_ne!(
            no_code_msg, with_code_msg,
            "454 message must distinguish 'no code yet' from 'code rejected'"
        );
    }

    #[test]
    fn build_2fa_error_455_mentions_invalid_or_expired() {
        let err = build_2fa_error(455, true).expect("455 must produce error");
        let msg = format!("{}", err.0);
        assert!(
            msg.contains("invalid") || msg.contains("expired"),
            "455 message must explain the rejection: {msg}"
        );
        assert!(
            msg.contains("455"),
            "user must see the status code for log searches: {msg}"
        );
    }

    /// 454/455 errors MUST flow through cache_get without being negative-cached.
    /// This is the contract that lets users retry inside the 15-min window.
    #[test]
    fn build_2fa_error_results_downcast_to_no_cache_error_via_anyhow() {
        for status in [454_u64, 455_u64] {
            let err = build_2fa_error(status, false).expect("must produce error");
            let any_err: anyhow::Error = err.into();
            assert!(
                any_err.downcast_ref::<NoCacheError>().is_some(),
                "status {status} error must round-trip as NoCacheError so cache_get bypasses negative cache"
            );
        }
    }

    // --- HTTP error classification (P2.1: 5xx must bypass negative cache) ---

    #[test]
    fn http_500_classifies_as_no_cache_error() {
        let err = classify_login_http_error(500, "internal".to_string());
        assert!(
            err.downcast_ref::<NoCacheError>().is_some(),
            "5xx must bypass negative cache so transient gateway issues don't slow retries"
        );
    }

    #[test]
    fn http_503_classifies_as_no_cache_error() {
        let err = classify_login_http_error(503, "unavailable".to_string());
        assert!(err.downcast_ref::<NoCacheError>().is_some());
    }

    #[test]
    fn http_599_classifies_as_no_cache_error() {
        // Boundary: highest 5xx still bypasses cache.
        let err = classify_login_http_error(599, "edge".to_string());
        assert!(err.downcast_ref::<NoCacheError>().is_some());
    }

    #[test]
    fn http_403_classifies_as_plain_error_for_short_negative_caching() {
        let err = classify_login_http_error(403, "forbidden".to_string());
        assert!(
            err.downcast_ref::<NoCacheError>().is_none(),
            "4xx is deterministic — short negative cache is fine and avoids hammering"
        );
    }

    #[test]
    fn http_400_classifies_as_plain_error() {
        let err = classify_login_http_error(400, "bad request".to_string());
        assert!(err.downcast_ref::<NoCacheError>().is_none());
    }

    #[test]
    fn http_499_classifies_as_plain_error_just_below_5xx_boundary() {
        // Boundary: highest non-5xx stays on the plain path.
        let err = classify_login_http_error(499, "client closed".to_string());
        assert!(err.downcast_ref::<NoCacheError>().is_none());
    }

    #[test]
    fn http_error_message_propagates_through_classify() {
        let err = classify_login_http_error(502, "bad gateway upstream".to_string());
        assert!(format!("{err:#}").contains("bad gateway upstream"));
    }

    // --- DRY refactor: opt_arg_or_env behavior preserved ---

    #[test]
    fn opt_arg_or_env_returns_field_when_set() {
        let field = Some("from-cli".to_string());
        let v = opt_arg_or_env(&field, "NEVER_LOOKED_UP_BECAUSE_FIELD_SET").unwrap();
        assert_eq!(v, Some("from-cli".to_string()));
    }

    #[test]
    fn opt_arg_or_env_returns_none_when_neither_set() {
        let field: Option<String> = None;
        // Use a name unlikely to collide with anything in the environment.
        let v = opt_arg_or_env(&field, "GOVEE_TEST_DEFINITELY_UNSET_VAR_XYZ123").unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn build_2fa_error_does_not_leak_email_or_response_body() {
        // Defensive contract: error messages are hardcoded strings, never
        // built from server response bodies. If a future refactor adds
        // {raw_body} interpolation here, this test fails as a tripwire.
        for (status, code_set) in [(454_u64, false), (454_u64, true), (455_u64, true)] {
            let msg = format!("{}", build_2fa_error(status, code_set).unwrap().0);
            assert!(
                !msg.contains("@") && !msg.contains("<html"),
                "2FA error message must not contain user email or HTML response body: {msg}"
            );
        }
    }

    // --- UndocApiArguments → api_client wiring ---

    #[test]
    fn api_client_threads_2fa_code_into_login_body() {
        let args = UndocApiArguments {
            govee_email: Some("a@b.com".to_string()),
            govee_password: Some("pw".to_string()),
            govee_2fa_code: Some("987654".to_string()),
            govee_iot_key: PathBuf::from("/tmp/k"),
            govee_iot_cert: PathBuf::from("/tmp/c"),
            amazon_root_ca: PathBuf::from("/tmp/ca"),
        };
        let client = args.api_client().expect("api_client builds");
        let body = client.build_login_body();
        assert_eq!(body["code"], "987654");
        assert_eq!(body["email"], "a@b.com");
    }

    #[test]
    fn api_client_with_no_2fa_code_omits_code_field() {
        let args = UndocApiArguments {
            govee_email: Some("a@b.com".to_string()),
            govee_password: Some("pw".to_string()),
            govee_2fa_code: None,
            govee_iot_key: PathBuf::from("/tmp/k"),
            govee_iot_cert: PathBuf::from("/tmp/c"),
            amazon_root_ca: PathBuf::from("/tmp/ca"),
        };
        let client = args.api_client().expect("api_client builds");
        let body = client.build_login_body();
        assert!(body.get("code").is_none());
    }

    #[test]
    fn api_client_normalizes_2fa_code_with_whitespace() {
        let args = UndocApiArguments {
            govee_email: Some("a@b.com".to_string()),
            govee_password: Some("pw".to_string()),
            govee_2fa_code: Some("  111222\n".to_string()),
            govee_iot_key: PathBuf::from("/tmp/k"),
            govee_iot_cert: PathBuf::from("/tmp/c"),
            amazon_root_ca: PathBuf::from("/tmp/ca"),
        };
        let client = args.api_client().expect("api_client builds");
        let body = client.build_login_body();
        assert_eq!(
            body["code"], "111222",
            "whitespace from email paste must be normalized end-to-end"
        );
    }
}
