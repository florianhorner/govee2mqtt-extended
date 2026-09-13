//! Home Assistant command topics and the MQTT router must share one inventory.
//!
//! Discovery payloads are built in `hass_mqtt` constructors. The broker
//! subscriptions live in `rebuild_router`. A new entity can advertise
//! `gv2mqtt/{id}/set-foo` while nobody ever `.route`s it: Home Assistant
//! renders a working control that writes to a topic the bridge never reads.
//!
//! `REGISTERED_COMMAND_ROUTES` is the list the router must subscribe to.
//! Constructors instantiate those patterns instead of hand-written
//! `format!` strings, and route-backed discovery fields use `CommandTopic` so
//! arbitrary command strings cannot be advertised accidentally.

/// MQTT topic pattern with `:param` path segments, as `mosquitto-rs` matches them.
macro_rules! command_routes {
    ($($name:ident = $pattern:literal;)*) => {
        $(pub const $name: &str = $pattern;)*

        pub const REGISTERED_COMMAND_ROUTES: &[&str] = &[$($name),*];

        #[cfg(test)]
        const REGISTERED_COMMAND_ROUTE_NAMES: &[&str] = &[$(stringify!($name)),*];
    };
}

command_routes! {
    LIGHT_COMMAND_ROUTE = "gv2mqtt/light/:id/command";
    LIGHT_SEGMENT_COMMAND_ROUTE = "gv2mqtt/light/:id/command/:segment";
    SWITCH_COMMAND_ROUTE = "gv2mqtt/switch/:id/command/:instance";
    ONECLICK_ROUTE = "gv2mqtt/oneclick";
    PURGE_CACHES_ROUTE = "gv2mqtt/purge-caches";
    REQUEST_PLATFORM_DATA_ROUTE = "gv2mqtt/:id/request-platform-data";
    SCENE_NEXT_ROUTE = "gv2mqtt/:id/scene-next";
    SCENE_PREV_ROUTE = "gv2mqtt/:id/scene-prev";
    NUMBER_COMMAND_ROUTE = "gv2mqtt/number/:id/command/:mode_name/:work_mode";
    HUMIDIFIER_SET_MODE_ROUTE = "gv2mqtt/humidifier/:id/set-mode";
    SET_WORK_MODE_ROUTE = "gv2mqtt/:id/set-work-mode";
    MUSIC_SENSITIVITY_COMMAND_ROUTE = "gv2mqtt/:id/set-music-sensitivity";
    MUSIC_SENSITIVITY_CLEAR_ROUTE = "gv2mqtt/:id/clear-music-sensitivity";
    HUMIDIFIER_SET_TARGET_ROUTE = "gv2mqtt/humidifier/:id/set-target";
    SET_TEMPERATURE_ROUTE = "gv2mqtt/:id/set-temperature/:instance/:units";
    SET_MODE_SCENE_ROUTE = "gv2mqtt/:id/set-mode-scene";
    SET_MUSIC_PALETTE_ROUTE = "gv2mqtt/:id/set-music-palette";
    FAN_SET_PERCENTAGE_ROUTE = "gv2mqtt/fan/:id/set-percentage";
}

/// A command topic that was instantiated from a registered route pattern.
///
/// Keeping this type separate from arbitrary strings prevents discovery
/// constructors that accept command topics from accidentally advertising an
/// unregistered MQTT topic.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct CommandTopic(String);

impl CommandTopic {
    #[cfg(test)]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Substitute `:param` path segments. Replacement is per-segment so `:id`
/// cannot steal the prefix of `:identity`. Every placeholder must have an
/// exact parameter; malformed discovery configuration is returned to the
/// caller instead of being advertised with a literal `:param` segment.
pub fn instantiate_route(route: &str, params: &[(&str, &str)]) -> anyhow::Result<CommandTopic> {
    anyhow::ensure!(
        REGISTERED_COMMAND_ROUTES.contains(&route),
        "unknown MQTT command route: {route}"
    );

    let mut topic: Vec<&str> = Vec::new();
    for segment in route.split('/') {
        match segment.strip_prefix(':') {
            Some(name) => {
                let value = params
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| *value)
                    .ok_or_else(|| {
                        anyhow::anyhow!("missing parameter '{name}' for route {route}")
                    })?;
                anyhow::ensure!(
                    !value.is_empty(),
                    "empty parameter '{name}' for route {route}"
                );
                anyhow::ensure!(
                    !value.contains(['/', '+', '#', '\0']),
                    "parameter '{name}' for route {route} contains a character \
                     that is not valid inside a single MQTT topic segment: {value:?}"
                );
                topic.push(value);
            }
            None => topic.push(segment),
        }
    }

    Ok(CommandTopic(topic.join("/")))
}

/// The `:id` segment of `topic` when it matches a registered command route.
///
/// Global routes (`oneclick`, `purge-caches`) have no `:id` and return `None`.
pub fn device_id_segment(topic: &str) -> Option<&str> {
    REGISTERED_COMMAND_ROUTES
        .iter()
        .find_map(|route| param_from_topic(topic, route, "id"))
        .filter(|id| !id.is_empty())
}

fn param_from_topic<'a>(topic: &'a str, route: &str, param: &str) -> Option<&'a str> {
    let topic_segs: Vec<&str> = topic.split('/').collect();
    let route_segs: Vec<&str> = route.split('/').collect();
    if topic_segs.len() != route_segs.len() {
        return None;
    }
    let mut found = None;
    for (topic_seg, route_seg) in topic_segs.iter().zip(route_segs) {
        if let Some(name) = route_seg.strip_prefix(':') {
            if name == param {
                found = Some(*topic_seg);
            }
        } else if *topic_seg != route_seg {
            return None;
        }
    }
    found
}

#[cfg(test)]
mod test {
    use super::*;
    use std::path::{Path, PathBuf};

    fn production_src(src: &str) -> &str {
        src.split("#[cfg(test)]").next().unwrap_or(src)
    }

    /// Drop comments so a commented-out `.route` cannot satisfy the inventory.
    /// Strings are preserved; `https://` is not treated as a line comment.
    fn strip_comments(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut chars = src.chars().peekable();
        let mut in_string = false;
        let mut prev = '\0';
        while let Some(ch) = chars.next() {
            if in_string {
                out.push(ch);
                if ch == '\\' {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                    prev = ch;
                    continue;
                }
                if ch == '"' {
                    in_string = false;
                }
                prev = ch;
                continue;
            }
            if ch == '"' {
                in_string = true;
                out.push('"');
                prev = ch;
                continue;
            }
            if ch == '/' && chars.peek() == Some(&'/') {
                if prev == ':' {
                    out.push('/');
                    prev = ch;
                    continue;
                }
                while let Some(next) = chars.next() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
                prev = '\n';
                continue;
            }
            if ch == '/' && chars.peek() == Some(&'*') {
                chars.next();
                let mut last = '\0';
                for next in chars.by_ref() {
                    if last == '*' && next == '/' {
                        break;
                    }
                    last = next;
                }
                prev = ' ';
                continue;
            }
            out.push(ch);
            prev = ch;
        }
        out
    }

    fn live_rust(src: &str) -> String {
        strip_comments(production_src(src))
    }

    fn crate_src() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    fn read_src(relative: &str) -> String {
        std::fs::read_to_string(crate_src().join(relative))
            .unwrap_or_else(|error| panic!("read src/{relative}: {error}"))
    }

    fn hass_mqtt_entity_sources() -> Vec<(String, String)> {
        let dir = crate_src().join("hass_mqtt");
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("src/hass_mqtt") {
            let path = entry.expect("src/hass_mqtt entry").path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let name = path
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned();
            if name == "command_routes.rs" {
                continue;
            }
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            files.push((name, src));
        }
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }

    fn format_string_literals(src: &str) -> Vec<String> {
        let mut literals = Vec::new();
        let mut rest = src;
        while let Some(offset) = rest.find("format!(") {
            rest = &rest[offset + "format!(".len()..];
            let trimmed = rest.trim_start();
            if !trimmed.starts_with('"') {
                continue;
            }
            let bytes = trimmed.as_bytes();
            let mut i = 1;
            let mut lit = String::new();
            while i < bytes.len() {
                let b = bytes[i];
                if b == b'\\' && i + 1 < bytes.len() {
                    lit.push(bytes[i + 1] as char);
                    i += 2;
                    continue;
                }
                if b == b'"' {
                    break;
                }
                lit.push(b as char);
                i += 1;
            }
            literals.push(lit);
        }
        literals
    }

    fn is_command_shaped(format_literal: &str) -> bool {
        if !format_literal.starts_with("gv2mqtt/") {
            return false;
        }
        let skip = [
            "/state",
            "/notify",
            "/advise",
            "/attributes",
            "/scene-catalog",
        ];
        !skip.iter().any(|marker| format_literal.contains(marker))
    }

    fn instantiate_route_idents(src: &str) -> Vec<String> {
        let mut idents = Vec::new();
        let mut rest = src;
        while let Some(offset) = rest.find("instantiate_route(") {
            rest = &rest[offset + "instantiate_route(".len()..];
            let trimmed = rest.trim_start();
            let ident: String = trimmed
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == ':')
                .collect();
            if ident.is_empty() {
                continue;
            }
            let name = ident.rsplit("::").next().unwrap_or(&ident).to_string();
            idents.push(name);
        }
        idents
    }

    #[test]
    fn instantiate_route_fills_named_segments_without_prefix_theft() {
        let topic = instantiate_route(
            SET_TEMPERATURE_ROUTE,
            &[("id", "aabb"), ("instance", "identity"), ("units", "C")],
        )
        .expect("all route parameters are present");
        assert_eq!(topic.as_str(), "gv2mqtt/aabb/set-temperature/identity/C");
        assert_eq!(
            instantiate_route(ONECLICK_ROUTE, &[])
                .expect("global route has no parameters")
                .as_str(),
            ONECLICK_ROUTE
        );
    }

    /// `CommandTopic::as_str()` only exists under `#[cfg(test)]`; production
    /// code reaches the topic exclusively through `#[serde(transparent)]`
    /// when a discovery payload is serialized. If that attribute were ever
    /// dropped, every entity's `command_topic` field would silently become a
    /// JSON object (`{"0":"..."}`) instead of the plain string Home
    /// Assistant expects, while every `.as_str()`-based test kept passing.
    #[test]
    fn command_topic_serializes_to_a_bare_json_string() {
        let topic = instantiate_route(ONECLICK_ROUTE, &[]).expect("global route has no parameters");
        let json = serde_json::to_value(&topic).expect("CommandTopic must serialize");
        assert_eq!(json, serde_json::Value::String(ONECLICK_ROUTE.to_string()));
    }

    #[test]
    fn instantiate_route_rejects_unknown_or_unresolved_routes() {
        let missing = instantiate_route(SET_TEMPERATURE_ROUTE, &[("id", "aabb")])
            .expect_err("missing placeholders must not be advertised");
        assert!(missing.to_string().contains("missing parameter 'instance'"));

        let unknown = instantiate_route("gv2mqtt/:id/not-registered", &[("id", "aabb")])
            .expect_err("unknown route patterns must not be advertised");
        assert!(unknown.to_string().contains("unknown MQTT command route"));
    }

    /// A present-but-empty value (e.g. an empty Platform API instance name)
    /// is a different failure mode than an absent one: `params.iter().find`
    /// succeeds, so only the explicit `is_empty()` guard catches it. Without
    /// it, callers would silently advertise a topic with a missing segment
    /// (`.../command/`) that no route pattern actually matches.
    #[test]
    fn instantiate_route_rejects_an_empty_parameter_value() {
        let empty = instantiate_route(
            SET_TEMPERATURE_ROUTE,
            &[("id", "aabb"), ("instance", ""), ("units", "C")],
        )
        .expect_err("an empty parameter value must not be advertised");
        assert!(
            empty.to_string().contains("empty parameter 'instance'"),
            "unexpected error: {empty}"
        );
    }

    /// A `/` inside a substituted value would silently add an extra path
    /// segment (turning one `:instance` into two topic segments), so a topic
    /// built from it could stop matching its own route pattern -- Home
    /// Assistant would advertise a control the bridge's own router can no
    /// longer parse back. `+`/`#` are MQTT wildcard characters and a NUL
    /// byte is invalid in an MQTT topic string entirely, so all three are
    /// rejected the same way as an empty value.
    #[test]
    fn instantiate_route_rejects_parameter_values_with_mqtt_special_characters() {
        for bad_value in ["has/slash", "has+plus", "has#hash", "has\0nul"] {
            let error = instantiate_route(SCENE_NEXT_ROUTE, &[("id", bad_value)]).expect_err(
                "a parameter value with an MQTT-significant character must not be advertised",
            );
            assert!(
                error
                    .to_string()
                    .contains("not valid inside a single MQTT topic segment"),
                "unexpected error for {bad_value:?}: {error}"
            );
        }
    }

    #[test]
    fn every_device_scoped_route_exposes_its_id_segment() -> anyhow::Result<()> {
        for route in REGISTERED_COMMAND_ROUTES {
            if !route.split('/').any(|segment| segment == ":id") {
                continue;
            }
            let topic = instantiate_route(
                route,
                &[
                    ("id", "AABBCC"),
                    ("segment", "3"),
                    ("instance", "powerSwitch"),
                    ("mode_name", "manual"),
                    ("work_mode", "1"),
                    ("units", "C"),
                ],
            )?;
            assert_eq!(
                device_id_segment(topic.as_str()),
                Some("AABBCC"),
                "route {route} instantiated to {topic:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn global_routes_have_no_device_id_segment() {
        for route in REGISTERED_COMMAND_ROUTES {
            if route.split('/').any(|segment| segment == ":id") {
                continue;
            }
            assert_eq!(
                device_id_segment(route),
                None,
                "global route {route} must not join a device dispatch queue"
            );
        }
    }

    /// `device_id_segment` runs on every inbound MQTT message (via
    /// `mqtt_device_dispatch_label`), including ones this bridge does not
    /// own: the HA status topic, a truncated/malformed publish, or a topic
    /// under an unrelated prefix. None of those shapes may be mistaken for a
    /// registered route with a coincidentally-matching segment count.
    #[test]
    fn device_id_segment_returns_none_for_a_topic_shaped_like_no_route() {
        assert_eq!(device_id_segment("homeassistant/status"), None);
        assert_eq!(
            device_id_segment("gv2mqtt/light/AABBCC"),
            None,
            "truncated topic missing the /command suffix must not match"
        );
        assert_eq!(device_id_segment("totally/unrelated/topic/shape"), None);
    }

    #[test]
    fn registered_patterns_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for route in REGISTERED_COMMAND_ROUTES {
            assert!(
                seen.insert(*route),
                "duplicate command route pattern {route}"
            );
        }
        assert_eq!(
            REGISTERED_COMMAND_ROUTES.len(),
            REGISTERED_COMMAND_ROUTE_NAMES.len()
        );
    }

    #[test]
    fn live_rust_drops_commented_out_routes() {
        let src = r#"
            fn rebuild_router() {
                router.route(LIGHT_COMMAND_ROUTE, h);
                // router.route(SCENE_NEXT_ROUTE, h);
                /* router.route(SCENE_PREV_ROUTE, h); */
                router.route(format!("{disco_prefix}/status"), h);
            }
            #[cfg(test)]
            mod test { router.route(REQUEST_PLATFORM_DATA_ROUTE, h); }
        "#;
        let live = live_rust(src);
        assert!(live.contains("LIGHT_COMMAND_ROUTE"));
        assert!(
            !live.contains("SCENE_NEXT_ROUTE"),
            "line comments must not count as registration: {live}"
        );
        assert!(
            !live.contains("SCENE_PREV_ROUTE"),
            "block comments must not count as registration: {live}"
        );
        assert!(
            !live.contains("REQUEST_PLATFORM_DATA_ROUTE"),
            "#[cfg(test)] must not count as registration: {live}"
        );
    }

    #[test]
    fn advertised_command_topics_use_restricted_route_fields() {
        // Route-backed discovery fields are typed as `CommandTopic`, so a
        // direct literal or `concat!` cannot be assigned without an explicit
        // escape hatch. The type is the guard; these declarations keep every
        // entity module on that contract.
        let expected_fields = [
            ("button.rs", "pub command_topic: CommandTopic"),
            ("scene.rs", "pub command_topic: CommandTopic"),
            (
                "select.rs",
                "pub command_topic: crate::hass_mqtt::command_routes::CommandTopic",
            ),
            (
                "number.rs",
                "pub command_topic: crate::hass_mqtt::command_routes::CommandTopic",
            ),
            (
                "switch.rs",
                "pub command_topic: crate::hass_mqtt::command_routes::CommandTopic",
            ),
            (
                "light.rs",
                "pub command_topic: crate::hass_mqtt::command_routes::CommandTopic",
            ),
            (
                "humidifier.rs",
                "pub command_topic: crate::hass_mqtt::command_routes::CommandTopic",
            ),
        ];

        let sources = hass_mqtt_entity_sources()
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        for (name, declaration) in expected_fields {
            let source = sources
                .get(name)
                .unwrap_or_else(|| panic!("missing hass_mqtt/{name}"));
            assert!(
                live_rust(source).contains(declaration),
                "{name} must keep command topics route-derived"
            );
        }

        let mut offenders = Vec::new();
        for (name, src) in sources {
            let live = live_rust(&src);
            for literal in format_string_literals(&live) {
                if is_command_shaped(&literal) {
                    offenders.push(format!("{name}: format!({literal:?})"));
                }
            }
            if live.contains("command_topic = format!") || live.contains("command_topic: format!") {
                offenders.push(format!("{name}: command_topic assigned from format!"));
            }
        }
        assert!(
            offenders.is_empty(),
            "advertise command topics via instantiate_route(REGISTERED const), \
             not format!:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn instantiate_route_callers_use_registered_consts() {
        let mut unknown = Vec::new();
        for (name, src) in hass_mqtt_entity_sources() {
            for ident in instantiate_route_idents(&live_rust(&src)) {
                if ident == "route" {
                    continue;
                }
                if !REGISTERED_COMMAND_ROUTE_NAMES.contains(&ident.as_str()) {
                    unknown.push(format!("{name}: instantiate_route({ident})"));
                }
            }
        }
        assert!(
            unknown.is_empty(),
            "instantiate_route must be called with a REGISTERED command route:\n{}",
            unknown.join("\n")
        );
    }

    #[test]
    fn oneclick_and_purge_helpers_return_inventory_routes() -> anyhow::Result<()> {
        let hass = live_rust(&read_src("service/hass.rs"));
        assert!(
            hass.contains("ONECLICK_ROUTE"),
            "oneclick_topic must return ONECLICK_ROUTE"
        );
        assert!(
            hass.contains("PURGE_CACHES_ROUTE"),
            "purge_cache_topic must return PURGE_CACHES_ROUTE"
        );
        assert!(
            !production_src(&read_src("service/hass.rs")).contains("\"gv2mqtt/oneclick\""),
            "oneclick must not keep a parallel string literal"
        );
        assert!(
            !production_src(&read_src("service/hass.rs")).contains("\"gv2mqtt/purge-caches\""),
            "purge-caches must not keep a parallel string literal"
        );
        assert_eq!(
            crate::service::hass::oneclick_topic()?.as_str(),
            ONECLICK_ROUTE
        );
        assert_eq!(
            crate::service::hass::purge_cache_topic()?.as_str(),
            PURGE_CACHES_ROUTE
        );
        Ok(())
    }
}
