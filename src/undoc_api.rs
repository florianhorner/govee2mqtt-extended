#![allow(unused)]
use crate::cache::{
    cache_get, cache_get_inner, CacheComputeResult, CacheGetOptions, NoCacheError, CACHE,
};
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
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

// <https://github.com/constructorfleet/homebridge-ultimate-govee/blob/main/src/data/clients/RestClient.ts>

const APP_VERSION: &str = "7.4.10";
const HALF_DAY: Duration = Duration::from_secs(3600 * 12);
const ONE_DAY: Duration = Duration::from_secs(86400);
const ONE_WEEK: Duration = Duration::from_secs(86400 * 7);
const FIFTEEN_MINS: Duration = Duration::from_secs(60 * 15);

/// Cap on the 2FA verification response we will buffer or quote back in an
/// error. Govee's real response is a few dozen bytes; anything larger is a
/// proxy error page or a captive portal, and there is no reason to hold it in
/// memory or copy it into the addon log. Enforced against the declared
/// Content-Length when there is one, and again as the bytes arrive, so a
/// chunked response cannot bypass it.
const MAX_VERIFICATION_BODY_BYTES: usize = 64 * 1024;

/// Split out so the cap is testable without standing up an HTTP server.
fn exceeds_verification_body_cap(so_far: usize, chunk_len: usize) -> bool {
    so_far.saturating_add(chunk_len) > MAX_VERIFICATION_BODY_BYTES
}

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

/// Trim whitespace from a pasted 2FA code and treat an empty value as unset.
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

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct TwoFactorLoginError {
    status: u64,
    code_was_set: bool,
    message: &'static str,
}

/// A verification request that failed *without* proving the email was not sent:
/// the request timed out, or the response could not be read after Govee had
/// already accepted it. Govee may well have delivered the code anyway, so the
/// caller applies the 15-minute suppression window regardless. Sending a second
/// email on every retry is a worse failure than making the user wait.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct VerificationDeliveryUnknown(String);

/// Decide whether a transport failure leaves the delivery outcome unknown.
///
/// `sending` a request that never completed means Govee never saw it, *unless*
/// it timed out — a timeout only tells us we stopped waiting. Once a status
/// line has arrived Govee has processed the request, so any later failure
/// (truncated body, decode error) is also unknown rather than a proven no-send.
fn classify_verification_transport_error(
    err: reqwest::Error,
    delivery_unknown: bool,
    context: &str,
) -> anyhow::Error {
    let message = format!("{context}: {err}");
    if delivery_unknown || err.is_timeout() {
        return VerificationDeliveryUnknown(message).into();
    }
    anyhow::anyhow!(message)
}

/// Truncate a response body before it reaches a log line or an error message.
fn truncate_body_for_diagnostics(body_bytes: &[u8]) -> String {
    const MAX_QUOTED: usize = 512;
    let text = String::from_utf8_lossy(body_bytes);
    match text.char_indices().nth(MAX_QUOTED) {
        Some((cut, _)) => format!("{}… ({} bytes total)", &text[..cut], body_bytes.len()),
        None => text.into_owned(),
    }
}

/// Extract only the non-secret 2FA state from a login error: the status and
/// whether a code was configured, never the message or any credential. The
/// user-facing error stays wrapped in `NoCacheError`, preserving the login
/// cache-bypass contract.
#[cfg(test)]
pub(crate) fn classify_2fa_login_error(err: &anyhow::Error) -> Option<(u64, bool)> {
    let no_cache = err.downcast_ref::<NoCacheError>()?;
    let two_factor = no_cache.0.downcast_ref::<TwoFactorLoginError>()?;
    Some((two_factor.status, two_factor.code_was_set))
}

/// Build the right NoCacheError for a Govee login response status, or None if
/// the status is not a 2FA condition. Pulled out of `login_account_impl` so
/// the user-facing messaging can be unit-tested without an HTTP mock.
fn build_2fa_error(status: u64, code_was_set: bool) -> Option<NoCacheError> {
    match status {
        454 => {
            let msg = if code_was_set {
                "Govee rejected the configured 2FA code with status 454. The \
                 code may be incorrect, expired, or from another login. Clear \
                 govee_2fa_code (or GOVEE_2FA_CODE), then restart without a \
                 code to request another email."
            } else {
                "Govee requires 2FA verification. Govee2MQTT requested a code \
                 by email. Set govee_2fa_code in the add-on configuration (or \
                 GOVEE_2FA_CODE) and restart within about 15 minutes."
            };
            Some(NoCacheError(
                TwoFactorLoginError {
                    status,
                    code_was_set,
                    message: msg,
                }
                .into(),
            ))
        }
        455 => {
            let msg = if code_was_set {
                "Govee rejected the configured 2FA code as invalid or expired \
                 (status 455). It may be from another login. Clear \
                 govee_2fa_code (or GOVEE_2FA_CODE), then restart without a \
                 code to request another email."
            } else {
                "Govee returned status 455 with no configured 2FA code. \
                 Restart without a code to retry login. If this continues, \
                 check the Govee account credentials."
            };
            Some(NoCacheError(
                TwoFactorLoginError {
                    status,
                    code_was_set,
                    message: msg,
                }
                .into(),
            ))
        }
        _ => None,
    }
}

/// Validate the body because Govee may return HTTP 200 for a failed request.
///
/// Both failure paths are `VerificationDeliveryUnknown`, not plain errors:
/// Govee answered, so it received the request, and this undocumented API
/// publishes no contract for which statuses mean "no email left the building".
/// Treating a reported failure as proof of non-delivery would send a second
/// code and invalidate the one the user is currently typing -- a worse outcome
/// than making them wait out the suppression window.
fn validate_2fa_verification_response(body_bytes: &[u8]) -> anyhow::Result<()> {
    match classify_login_status(body_bytes) {
        Some(200) => Ok(()),
        Some(status) => Err(VerificationDeliveryUnknown(format!(
            "Govee 2FA verification request failed with status {status}"
        ))
        .into()),
        None => Err(VerificationDeliveryUnknown(
            "Govee 2FA verification response has no numeric status".to_string(),
        )
        .into()),
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

    /// Verification code emailed by Govee. Leave unset on first start.
    /// On status 454, Govee2MQTT requests a code. Set it and restart within
    /// about 15 minutes. If Govee rejects it, clear it and restart without a
    /// code to request another. Defaults to GOVEE_2FA_CODE.
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

pub struct GoveeUndocumentedApi {
    email: String,
    password: String,
    /// Optional 2FA verification code. Govee accepts this as the `code` field on
    /// the login request body when the account has two-factor enabled. The code
    /// is config-driven: the addon reads it from `govee_2fa_code` / GOVEE_2FA_CODE
    /// at startup. After a successful login it is cleared so it is not replayed on
    /// subsequent token refreshes. A later 2FA challenge requires another code.
    code: std::sync::Mutex<Option<String>>,
    client_id: String,
}

impl Clone for GoveeUndocumentedApi {
    fn clone(&self) -> Self {
        Self {
            email: self.email.clone(),
            password: self.password.clone(),
            code: std::sync::Mutex::new(
                self.code.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            ),
            client_id: self.client_id.clone(),
        }
    }
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
            code: std::sync::Mutex::new(None),
            client_id,
        }
    }

    /// Builder-style setter for the 2FA verification code. Pass `None` if 2FA is
    /// not enabled on the account; pass `Some(code)` after status 454 triggers an
    /// email request.
    ///
    /// The code is normalized: surrounding whitespace is stripped and an empty
    /// result is treated as `None`. This means `with_code(Some(""))` and
    /// `with_code(Some("  \n"))` both leave the client in the no-code state,
    /// rather than sending an empty `code` field that Govee would reject as
    /// invalid with a misleading message.
    pub fn with_code(mut self, code: Option<String>) -> Self {
        *self.code.lock().unwrap_or_else(|e| e.into_inner()) = normalize_2fa_code(code);
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
        if let Some(code) = self
            .code
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
        {
            body["code"] = serde_json::Value::String(code.to_string());
        }
        body
    }

    /// Build the request separately so tests can inspect it without sending it.
    fn build_2fa_request(&self, client: &reqwest::Client) -> reqwest::RequestBuilder {
        client
            .request(
                Method::POST,
                "https://app2.govee.com/account/rest/account/v1/verification",
            )
            .header("appVersion", APP_VERSION)
            .header("clientId", &self.client_id)
            .header("clientType", "1")
            .header("iotVersion", "0")
            .header("timestamp", ms_timestamp())
            .header("User-Agent", user_agent())
            .json(&json!({
                "type": 8,
                "email": self.email,
            }))
    }

    async fn request_2fa_code_impl(&self) -> anyhow::Result<()> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("building the Govee 2FA verification HTTP client")?;
        let mut response = self
            .build_2fa_request(&client)
            .send()
            .await
            .map_err(|err| {
                classify_verification_transport_error(
                    err,
                    false,
                    "requesting a Govee 2FA verification code",
                )
            })?;

        let url = response.url().clone();
        let status = response.status();

        if let Some(length) = response.content_length() {
            if length > MAX_VERIFICATION_BODY_BYTES as u64 {
                // Govee accepted the request, so we cannot claim nothing was
                // sent -- but we will not buffer the body to find out.
                return Err(VerificationDeliveryUnknown(format!(
                    "request {url} status {} returned {length} bytes, over the \
                     {MAX_VERIFICATION_BODY_BYTES} byte limit",
                    status.as_u16()
                ))
                .into());
            }
        }

        // Past this point Govee has already processed the request, so every
        // remaining failure leaves delivery unknown rather than disproven.
        let mut body_bytes: Vec<u8> = Vec::new();
        // Read incrementally: a chunked response declares no Content-Length, so
        // the check above never sees it and `bytes()` would buffer the lot.
        while let Some(chunk) = response.chunk().await.map_err(|err| {
            classify_verification_transport_error(
                err,
                true,
                &format!(
                    "reading the response body of request {url} status {}",
                    status.as_u16()
                ),
            )
        })? {
            if exceeds_verification_body_cap(body_bytes.len(), chunk.len()) {
                return Err(VerificationDeliveryUnknown(format!(
                    "request {url} status {} exceeded the \
                     {MAX_VERIFICATION_BODY_BYTES} byte limit while reading the \
                     response body",
                    status.as_u16()
                ))
                .into());
            }
            body_bytes.extend_from_slice(&chunk);
        }

        if !status.is_success() {
            // Govee answered, so this is a reported failure, not a proven
            // non-delivery. Same reasoning as validate_2fa_verification_response.
            return Err(VerificationDeliveryUnknown(format!(
                "request {url} status {}: {}. Response body: {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or(""),
                truncate_body_for_diagnostics(&body_bytes)
            ))
            .into());
        }

        validate_2fa_verification_response(&body_bytes)
    }

    /// Derived from the *lowercased* email, unlike `client_id`, which hashes the
    /// address exactly as typed -- so `User@x.com` and `user@x.com` produce two
    /// different ids and would each get their own suppression window.
    ///
    /// Lowercasing `client_id` itself is not an option: Govee treats it as the
    /// device identity, so changing how it is derived would hand every existing
    /// installation a new device on upgrade and could re-trigger 2FA for people
    /// who have no problem today.
    ///
    /// Hashed rather than holding the address in plain text, because this key is
    /// stored in the on-disk cache and appears in error messages.
    fn verification_request_cache_key(&self) -> String {
        let id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, self.email.to_lowercase().as_bytes());
        format!("2fa-verification-request-{}", id.simple())
    }

    fn invalidate_2fa_request_cache(&self, cache: &sqlite_cache::Cache) -> anyhow::Result<()> {
        let cache_key = self.verification_request_cache_key();
        cache
            .topic("undoc-api")
            .context("opening the undocumented API cache for 2FA request invalidation")?
            .delete(&cache_key)
            .with_context(|| {
                // Key, not the raw account address: this string reaches the log.
                format!("invalidating the cached Govee 2FA verification request {cache_key}")
            })?;
        Ok(())
    }

    /// Suppress duplicate emails for one account for 15 minutes.
    ///
    /// Best-effort, not a lock: `cache_get_inner` builds a fresh `Topic` (and so
    /// a fresh per-key listener map) on every call, so two concurrent logins
    /// could both miss the marker. Not reachable today -- initial discovery in
    /// `serve` is sequential and bails before the periodic task is spawned.
    async fn request_2fa_code_cached<Fut>(
        &self,
        cache: &sqlite_cache::Cache,
        request: Fut,
    ) -> anyhow::Result<()>
    where
        Fut: Future<Output = anyhow::Result<()>>,
    {
        let cache_key = self.verification_request_cache_key();
        let _: String = cache_get_inner(
            cache,
            CacheGetOptions {
                topic: "undoc-api",
                key: &cache_key,
                soft_ttl: FIFTEEN_MINS,
                hard_ttl: FIFTEEN_MINS,
                negative_ttl: Duration::from_secs(10),
                allow_stale: false,
            },
            async {
                log::info!("Requesting a Govee 2FA verification code");
                match request.await {
                    Ok(()) => Ok(CacheComputeResult::Value(ms_timestamp())),
                    // Outcome unknown: Govee may have sent the mail even though
                    // we never saw a clean response. Claim the window anyway.
                    // Retrying would put a second code in the user's inbox on
                    // every restart, and only the newest one works.
                    Err(err) if err.downcast_ref::<VerificationDeliveryUnknown>().is_some() => {
                        log::warn!(
                            "Govee 2FA verification request outcome unknown, \
                             assuming the code was sent: {err:#}"
                        );
                        Ok(CacheComputeResult::Value(ms_timestamp()))
                    }
                    // Proven failure: nothing was sent. Let this land in the
                    // negative cache so an outage is retried on a bounded
                    // schedule rather than on every single login.
                    Err(err) => Err(err),
                }
            },
        )
        .await?;
        Ok(())
    }

    /// Request email on status 454 with no code. A rejected configured code
    /// clears the marker so the next no-code attempt can send another email.
    async fn handle_2fa_status<Fut>(
        &self,
        cache: &sqlite_cache::Cache,
        status: u64,
        request_code: Fut,
    ) -> anyhow::Result<Option<NoCacheError>>
    where
        Fut: Future<Output = anyhow::Result<()>>,
    {
        let code_was_set = self
            .code
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        if matches!(status, 454 | 455) && code_was_set {
            if let Err(err) = self.invalidate_2fa_request_cache(cache) {
                log::warn!(
                    "Could not invalidate the Govee 2FA request cache: {err:#}. \
                     Reporting the 2FA requirement anyway so the configuration \
                     steps stay visible."
                );
            }
        }
        if status == 454 && !code_was_set {
            // A failed request must never replace the 454 guidance. serve.rs
            // appends ISSUE_76_EXPLANATION to whatever surfaces here, and that
            // text tells the user to remove their Govee credentials and drop to
            // LAN-only -- ruinous advice for someone who only needed to paste a
            // code. Log the transport failure and fall through, so the message
            // the user actually reads always names govee_2fa_code.
            if let Err(err) = request_code.await {
                log::warn!(
                    "Could not request a Govee 2FA verification code: {err:#}. \
                     Reporting the 2FA requirement anyway so the configuration \
                     steps stay visible."
                );
            }
        }
        Ok(build_2fa_error(status, code_was_set))
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
            let cache = CACHE.load();
            let request_code = self.request_2fa_code_cached(&cache, self.request_2fa_code_impl());
            if let Some(err) = self
                .handle_2fa_status(&cache, api_status, request_code)
                .await?
            {
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

        let ttl =
            Duration::from_secs(resp.client.token_expire_cycle as u64).max(Duration::from_secs(60));
        *self.code.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fresh_cache() -> sqlite_cache::Cache {
        let connection = sqlite_cache::rusqlite::Connection::open_in_memory()
            .expect("open in-memory SQLite connection");
        sqlite_cache::Cache::new(
            sqlite_cache::CacheConfig {
                flush_gc_ratio: 1024,
                flush_interval: Duration::from_secs(900),
                max_ttl: None,
            },
            connection,
        )
        .expect("create in-memory cache")
    }

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
    fn verification_request_has_expected_endpoint_client_id_and_body() {
        let api = GoveeUndocumentedApi::new("a@b.com", "pw");
        let request = api
            .build_2fa_request(&reqwest::Client::new())
            .build()
            .expect("build verification request");

        assert_eq!(request.method(), Method::POST);
        assert_eq!(
            request.url().as_str(),
            "https://app2.govee.com/account/rest/account/v1/verification"
        );
        assert_eq!(
            request
                .headers()
                .get("clientId")
                .expect("clientId header")
                .to_str()
                .expect("clientId is valid header text"),
            api.client_id
        );

        let body_bytes = request
            .body()
            .and_then(|body| body.as_bytes())
            .expect("JSON request body");
        let body: JsonValue =
            serde_json::from_slice(body_bytes).expect("parse verification request body");
        assert_eq!(body, json!({"type": 8, "email": "a@b.com"}));
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

    #[test]
    fn verification_response_accepts_api_status_200() {
        validate_2fa_verification_response(br#"{"status":200}"#)
            .expect("status 200 must be accepted");
    }

    #[test]
    fn verification_response_rejects_non_200_api_status() {
        let err = validate_2fa_verification_response(br#"{"status":500,"message":"no"}"#)
            .expect_err("non-200 API status must fail");
        assert!(format!("{err:#}").contains("status 500"));
        assert!(
            err.downcast_ref::<VerificationDeliveryUnknown>().is_some(),
            "Govee answered, so delivery is unproven, not disproven: {err:#}"
        );
    }

    #[test]
    fn verification_response_rejects_missing_or_non_numeric_api_status() {
        for body in [
            br#"{"message":"ok"}"#.as_slice(),
            br#"{"status":"200"}"#.as_slice(),
            b"<html>unexpected response</html>".as_slice(),
        ] {
            let err = validate_2fa_verification_response(body)
                .expect_err("invalid verification response must fail");
            assert!(format!("{err:#}").contains("no numeric status"));
            assert!(
                err.downcast_ref::<VerificationDeliveryUnknown>().is_some(),
                "an unreadable response must claim the suppression window: {err:#}"
            );
        }
    }

    #[test]
    fn verification_body_cap_counts_accumulated_bytes() {
        assert!(!exceeds_verification_body_cap(
            0,
            MAX_VERIFICATION_BODY_BYTES
        ));
        assert!(exceeds_verification_body_cap(
            0,
            MAX_VERIFICATION_BODY_BYTES + 1
        ));
        // The chunked case the Content-Length check cannot see: many small
        // chunks, each far under the cap, that only overflow in aggregate.
        assert!(!exceeds_verification_body_cap(
            MAX_VERIFICATION_BODY_BYTES - 1,
            1
        ));
        assert!(exceeds_verification_body_cap(
            MAX_VERIFICATION_BODY_BYTES,
            1
        ));
        assert!(
            exceeds_verification_body_cap(usize::MAX, 1),
            "must not wrap"
        );
    }

    #[tokio::test]
    async fn repeated_454_retries_request_one_code_per_account() {
        let cache = fresh_cache();
        let requests = AtomicUsize::new(0);
        let api = GoveeUndocumentedApi::new("a@b.com", "pw");

        for _ in 0..2 {
            let request = api.request_2fa_code_cached(&cache, async {
                requests.fetch_add(1, Ordering::SeqCst);
                Ok::<(), anyhow::Error>(())
            });
            let err = api
                .handle_2fa_status(&cache, 454, request)
                .await
                .expect("verification request succeeds")
                .expect("454 produces a user-facing error");
            assert!(format!("{}", err.0).contains("requested a code by email"));
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "repeated 454 retries must not send duplicate emails"
        );

        let other_api = GoveeUndocumentedApi::new("other@example.com", "pw");
        let request = other_api.request_2fa_code_cached(&cache, async {
            requests.fetch_add(1, Ordering::SeqCst);
            Ok::<(), anyhow::Error>(())
        });
        other_api
            .handle_2fa_status(&cache, 454, request)
            .await
            .expect("second account verification request succeeds")
            .expect("454 produces a user-facing error");
        assert_eq!(
            requests.load(Ordering::SeqCst),
            2,
            "a different account must use a different request cache entry"
        );
    }

    #[tokio::test]
    async fn configured_code_paths_do_not_request_another_code() {
        let cache = fresh_cache();
        let requests = AtomicUsize::new(0);
        let api = GoveeUndocumentedApi::new("a@b.com", "pw").with_code(Some("1234".to_string()));

        for status in [454, 455] {
            let err = api
                .handle_2fa_status(&cache, status, async {
                    requests.fetch_add(1, Ordering::SeqCst);
                    Ok::<(), anyhow::Error>(())
                })
                .await
                .expect("configured-code status handling succeeds")
                .expect("2FA status produces a user-facing error");
            assert!(format!("{}", err.0).contains("Clear govee_2fa_code"));
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            0,
            "a rejected configured code must not trigger a replacement email"
        );
    }

    #[tokio::test]
    async fn rejected_configured_code_allows_one_fresh_request_for_454_and_455() {
        for rejected_status in [454, 455] {
            let cache = fresh_cache();
            let requests = AtomicUsize::new(0);
            let unexpected_requests = AtomicUsize::new(0);
            let no_code_api = GoveeUndocumentedApi::new("a@b.com", "pw");

            let initial_request = no_code_api.request_2fa_code_cached(&cache, async {
                requests.fetch_add(1, Ordering::SeqCst);
                Ok::<(), anyhow::Error>(())
            });
            no_code_api
                .handle_2fa_status(&cache, 454, initial_request)
                .await
                .expect("initial verification request succeeds")
                .expect("454 produces a user-facing error");
            assert_eq!(requests.load(Ordering::SeqCst), 1);

            let configured_api =
                GoveeUndocumentedApi::new("a@b.com", "pw").with_code(Some("1234".to_string()));
            configured_api
                .handle_2fa_status(&cache, rejected_status, async {
                    unexpected_requests.fetch_add(1, Ordering::SeqCst);
                    Ok::<(), anyhow::Error>(())
                })
                .await
                .expect("rejected configured-code handling succeeds")
                .expect("rejected 2FA status produces a user-facing error");
            assert_eq!(
                unexpected_requests.load(Ordering::SeqCst),
                0,
                "a rejected login must not send a replacement email"
            );

            for _ in 0..2 {
                let fresh_request = no_code_api.request_2fa_code_cached(&cache, async {
                    requests.fetch_add(1, Ordering::SeqCst);
                    Ok::<(), anyhow::Error>(())
                });
                no_code_api
                    .handle_2fa_status(&cache, 454, fresh_request)
                    .await
                    .expect("fresh verification request succeeds")
                    .expect("454 produces a user-facing error");
                assert_eq!(
                    requests.load(Ordering::SeqCst),
                    2,
                    "first no-code retry must send once; second must use the cache"
                );
            }
        }
    }

    #[tokio::test]
    async fn statuses_other_than_454_do_not_request_a_code() {
        let cache = fresh_cache();
        let requests = AtomicUsize::new(0);
        let api = GoveeUndocumentedApi::new("a@b.com", "pw");

        for status in [200, 455, 500] {
            api.handle_2fa_status(&cache, status, async {
                requests.fetch_add(1, Ordering::SeqCst);
                Ok::<(), anyhow::Error>(())
            })
            .await
            .expect("status handling succeeds");
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            0,
            "only status 454 without a configured code may request an email"
        );
    }

    #[tokio::test]
    async fn failed_verification_request_still_reports_actionable_2fa_guidance() {
        let cache = fresh_cache();
        let requests = AtomicUsize::new(0);
        let api = GoveeUndocumentedApi::new("a@b.com", "pw");

        let request = api.request_2fa_code_cached(&cache, async {
            requests.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(anyhow::anyhow!("simulated verification request failure"))
        });
        let err = api
            .handle_2fa_status(&cache, 454, request)
            .await
            .expect("a failed code request must not abort 2FA reporting")
            .expect("454 produces a user-facing error");
        let msg = format!("{}", err.0);
        assert!(
            msg.contains("govee_2fa_code"),
            "the user must still be told which setting to fill in: {msg}"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        // A proven failure lands in the negative cache, so an outage is not
        // re-hammered on every login.
        let retry = api.request_2fa_code_cached(&cache, async {
            requests.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(anyhow::anyhow!("simulated verification request failure"))
        });
        api.handle_2fa_status(&cache, 454, retry)
            .await
            .expect("a failed code request must not abort 2FA reporting")
            .expect("454 produces a user-facing error");
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "a proven request failure must be negatively cached, not retried at once"
        );
    }

    /// A request whose outcome we never learned (timeout, unreadable response)
    /// must claim the suppression window anyway. Retrying would put a second
    /// code in the user's inbox on every restart, and only the newest works.
    #[tokio::test]
    async fn unknown_delivery_outcome_claims_the_suppression_window() {
        let cache = fresh_cache();
        let requests = AtomicUsize::new(0);
        let api = GoveeUndocumentedApi::new("a@b.com", "pw");

        for _ in 0..2 {
            let request = api.request_2fa_code_cached(&cache, async {
                requests.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(VerificationDeliveryUnknown("simulated timeout".to_string()).into())
            });
            api.handle_2fa_status(&cache, 454, request)
                .await
                .expect("an unknown outcome must not abort 2FA reporting")
                .expect("454 produces a user-facing error");
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "an unknown outcome must suppress the next request like a success"
        );
    }

    /// F13: the defensive wrap in handle_2fa_status is only reachable when a
    /// caller passes a future that did not already go through
    /// request_2fa_code_cached. Exercise it directly so a future caller cannot
    /// silently lose the guidance.
    #[tokio::test]
    async fn raw_request_error_still_yields_the_2fa_message() {
        let cache = fresh_cache();
        let api = GoveeUndocumentedApi::new("a@b.com", "pw");

        let err = api
            .handle_2fa_status(&cache, 454, async {
                Err::<(), _>(anyhow::anyhow!("unwrapped transport failure"))
            })
            .await
            .expect("a raw request error must not abort 2FA reporting")
            .expect("454 produces a user-facing error");
        assert!(format!("{}", err.0).contains("govee_2fa_code"));
    }

    #[test]
    fn oversized_response_bodies_are_truncated_before_logging() {
        let body = vec![b'x'; 4096];
        let quoted = truncate_body_for_diagnostics(&body);
        assert!(
            quoted.len() < 700,
            "body must be truncated: {} chars",
            quoted.len()
        );
        assert!(quoted.contains("4096 bytes total"));
        assert_eq!(truncate_body_for_diagnostics(b"short"), "short");
    }

    #[test]
    fn verification_cache_key_ignores_email_case() {
        let lower = GoveeUndocumentedApi::new("user@example.com", "pw");
        let upper = GoveeUndocumentedApi::new("User@Example.com", "pw");
        assert_eq!(
            lower.verification_request_cache_key(),
            upper.verification_request_cache_key(),
            "case variants of one account must share a suppression window"
        );
        assert_ne!(
            lower.client_id, upper.client_id,
            "client_id derivation must stay untouched -- it is Govee's device identity"
        );
        // The key lands in the on-disk cache and in error messages.
        let key = lower.verification_request_cache_key();
        assert!(
            !key.contains("user@") && !key.contains("example.com"),
            "the cache key must not carry the account address in plain text: {key}"
        );
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

    #[test]
    fn code_is_cleared_after_successful_login_impl() {
        let api = GoveeUndocumentedApi::new("a@b.com", "pw").with_code(Some("123456".into()));
        // Simulate what login_account_impl does on success:
        *api.code.lock().unwrap() = None;
        let body = api.build_login_body();
        assert!(
            body.get("code").is_none(),
            "code must be absent after clearing"
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
    fn build_2fa_error_454_no_code_confirms_email_request() {
        let err = build_2fa_error(454, false).expect("454 with no code must produce error");
        let msg = format!("{}", err.0);
        assert!(
            msg.contains("requested a code by email"),
            "message must confirm the email request: {msg}"
        );
        assert!(
            !msg.contains("mobile app"),
            "trusted mobile-app login does not request an email: {msg}"
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
            msg.contains("Clear govee_2fa_code") && msg.contains("restart without a code"),
            "user must be told how to trigger a fresh request: {msg}"
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
        for (status, code_was_set) in [
            (454_u64, false),
            (454_u64, true),
            (455_u64, false),
            (455_u64, true),
        ] {
            let err = build_2fa_error(status, code_was_set).expect("must produce error");
            let any_err: anyhow::Error = err.into();
            assert!(
                any_err.downcast_ref::<NoCacheError>().is_some(),
                "status {status} error must round-trip as NoCacheError so cache_get bypasses negative cache"
            );
            assert_eq!(
                classify_2fa_login_error(&any_err),
                Some((status, code_was_set)),
                "the live-test probe must see only status and code state"
            );
        }
    }

    #[test]
    fn unrelated_no_cache_error_is_not_classified_as_two_factor() {
        let err: anyhow::Error = NoCacheError(anyhow::anyhow!("transient transport error")).into();
        assert_eq!(classify_2fa_login_error(&err), None);
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
