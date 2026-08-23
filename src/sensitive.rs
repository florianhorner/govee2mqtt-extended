use crate::lan_api::truthy;
use crate::opt_env_var;

pub(crate) const MAX_SENSITIVE_DIAGNOSTIC_CHARS: usize = 512;

pub(crate) fn should_log_sensitive_data() -> bool {
    if let Ok(Some(value)) = opt_env_var::<String>("GOVEE_LOG_SENSITIVE_DATA") {
        truthy(&value).unwrap_or(false)
    } else {
        false
    }
}

pub(crate) fn describe_sensitive_body_with_policy(body: &[u8], allow_sensitive: bool) -> String {
    if !allow_sensitive {
        return format!("Response body withheld ({} bytes)", body.len());
    }

    let text = String::from_utf8_lossy(body);
    match text.char_indices().nth(MAX_SENSITIVE_DIAGNOSTIC_CHARS) {
        Some((cut, _)) => format!(
            "Response body: {}… ({} bytes total)",
            &text[..cut],
            body.len()
        ),
        None => format!("Response body: {text} ({} bytes total)", body.len()),
    }
}

pub(crate) fn describe_json_error_with_policy<T>(
    error: &serde_json_path_to_error::Error,
    body: &[u8],
    allow_sensitive: bool,
) -> String {
    let source = error.inner();
    format!(
        "{} JSON {:?} error at line {}, column {}. {}",
        std::any::type_name::<T>(),
        source.classify(),
        source.line(),
        source.column(),
        describe_sensitive_body_with_policy(body, allow_sensitive)
    )
}

pub(crate) fn describe_request_url(url: &reqwest::Url) -> String {
    let mut sanitized = url.clone();
    if sanitized.set_username("").is_err() || sanitized.set_password(None).is_err() {
        return format!("{}:<endpoint withheld>", url.scheme());
    }
    sanitized.set_query(None);
    sanitized.set_fragment(None);
    sanitized.to_string()
}

#[cfg(test)]
mod test {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct NumericValue {
        value: u64,
    }

    #[test]
    fn sensitive_body_is_withheld_by_default() {
        let body = br#"{"password":"PASSWORD_SENTINEL"}"#;
        let description = describe_sensitive_body_with_policy(body, false);

        assert!(!description.contains("PASSWORD_SENTINEL"));
        assert_eq!(
            description,
            format!("Response body withheld ({} bytes)", body.len())
        );
    }

    #[test]
    fn sensitive_body_opt_in_is_unicode_safe_and_capped() {
        let body = "é".repeat(MAX_SENSITIVE_DIAGNOSTIC_CHARS + 1);
        let description = describe_sensitive_body_with_policy(body.as_bytes(), true);
        let quoted = description
            .strip_prefix("Response body: ")
            .and_then(|value| value.split_once('…').map(|(prefix, _)| prefix))
            .expect("opted-in oversized body must be quoted and truncated");

        assert_eq!(quoted.chars().count(), MAX_SENSITIVE_DIAGNOSTIC_CHARS);
        assert!(description.contains(&format!("{} bytes total", body.len())));
    }

    #[test]
    fn json_error_diagnostic_does_not_quote_the_rejected_value() {
        let body = br#"{"value":"PASSWORD_SENTINEL"}"#;
        let error = serde_json_path_to_error::from_slice::<NumericValue>(body)
            .expect_err("string must not deserialize as u64");
        let description = describe_json_error_with_policy::<NumericValue>(&error, body, false);

        assert!(!description.contains("PASSWORD_SENTINEL"));
        assert!(description.contains("Data"));
        assert!(description.contains("line 1"));
        assert!(description.contains(&format!("{} bytes", body.len())));
    }

    #[test]
    fn request_url_omits_credentials_query_and_fragment() {
        let url = reqwest::Url::parse(
            "https://USER_SENTINEL:PASSWORD_SENTINEL@example.com/path?token=TOKEN_SENTINEL#part",
        )
        .expect("test URL must parse");
        let description = describe_request_url(&url);

        for secret in [
            "USER_SENTINEL",
            "PASSWORD_SENTINEL",
            "TOKEN_SENTINEL",
            "part",
        ] {
            assert!(
                !description.contains(secret),
                "leaked {secret}: {description}"
            );
        }
        assert_eq!(description, "https://example.com/path");
    }
}
