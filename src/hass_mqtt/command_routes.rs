//! Home Assistant command topics and the MQTT router must share one inventory.
//!
//! Discovery payloads are built in `hass_mqtt` constructors. The broker
//! subscriptions live in `rebuild_router`. A new entity can advertise
//! `gv2mqtt/{id}/set-foo` while nobody ever `.route`s it: Home Assistant
//! renders a working control that writes to a topic the bridge never reads.
//!
//! `REGISTERED_COMMAND_ROUTES` is the list `rebuild_router` must subscribe
//! to. Constructors instantiate those patterns instead of hand-written
//! `format!` strings. Tests parse the live (non-commented) sources so a
//! commented-out `.route` or a fresh `format!("gv2mqtt/...` cannot pass.

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
}

/// Substitute `:param` path segments. Replacement is per-segment so `:id`
/// cannot steal the prefix of `:identity`.
pub fn instantiate_route(route: &str, params: &[(&str, &str)]) -> String {
    route
        .split('/')
        .map(|segment| match segment.strip_prefix(':') {
            Some(name) => params
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| *value)
                .unwrap_or(segment),
            None => segment,
        })
        .collect::<Vec<_>>()
        .join("/")
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

    fn nested_fn_body<'a>(src: &'a str, signature: &str) -> &'a str {
        let start = src
            .find(signature)
            .unwrap_or_else(|| panic!("source must contain {signature}"));
        let after_sig = &src[start..];
        let brace = after_sig
            .find('{')
            .unwrap_or_else(|| panic!("{signature} must have a body"));
        let bytes = after_sig.as_bytes();
        let mut depth = 0;
        let mut i = brace;
        let mut in_string = false;
        while i < bytes.len() {
            let b = bytes[i];
            if in_string {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if b == b'"' {
                    in_string = false;
                }
                i += 1;
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &after_sig[brace..=i];
                    }
                }
                _ => {}
            }
            i += 1;
        }
        panic!("{signature} body was not closed");
    }

    fn first_call_arg(after_open_paren: &str) -> String {
        let bytes = after_open_paren.as_bytes();
        let mut depth = 0;
        let mut in_string = false;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if in_string {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if b == b'"' {
                    in_string = false;
                }
                i += 1;
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'(' | b'{' => depth += 1,
                b')' | b'}' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                b',' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        after_open_paren[..i]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn route_first_args(rebuild_router: &str) -> Vec<String> {
        let mut args = Vec::new();
        let mut rest = rebuild_router;
        while let Some(offset) = rest.find(".route(") {
            rest = &rest[offset + ".route(".len()..];
            args.push(first_call_arg(rest));
        }
        args
    }

    fn leading_ident_path(arg: &str) -> &str {
        let end = arg
            .char_indices()
            .find(|(_, ch)| !ch.is_ascii_alphanumeric() && *ch != '_' && *ch != ':')
            .map(|(idx, _)| idx)
            .unwrap_or(arg.len());
        &arg[..end]
    }

    fn inventory_name(arg: &str) -> Option<&'static str> {
        let path = leading_ident_path(arg);
        let name = path.rsplit("::").next().unwrap_or(path);
        REGISTERED_COMMAND_ROUTE_NAMES
            .iter()
            .copied()
            .find(|registered| *registered == name)
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
        assert_eq!(
            instantiate_route(
                "gv2mqtt/:id/:identity",
                &[("id", "aabb"), ("identity", "scene")]
            ),
            "gv2mqtt/aabb/scene"
        );
        assert_eq!(instantiate_route(ONECLICK_ROUTE, &[]), ONECLICK_ROUTE);
    }

    #[test]
    fn every_device_scoped_route_exposes_its_id_segment() {
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
            );
            assert_eq!(
                device_id_segment(&topic),
                Some("AABBCC"),
                "route {route} instantiated to {topic}"
            );
        }
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

    /// `rebuild_router` is the live subscription table. Every inventory
    /// const must be passed to `.route` there, and the only non-inventory
    /// `.route` is Home Assistant's `{disco_prefix}/status` topic.
    #[test]
    fn rebuild_router_registers_exactly_the_command_route_inventory() {
        let hass = read_src("service/hass.rs");
        let live = live_rust(&hass);
        let rebuild = nested_fn_body(&live, "async fn rebuild_router");
        assert!(
            !rebuild.contains("\"gv2mqtt/"),
            "rebuild_router must not hardcode gv2mqtt route strings:\n{rebuild}"
        );

        let args = route_first_args(rebuild);
        let mut inventory_hits = Vec::new();
        let mut others = Vec::new();
        for arg in &args {
            match inventory_name(arg) {
                Some(name) => inventory_hits.push(name),
                None => others.push(arg.as_str()),
            }
        }

        assert_eq!(
            others,
            ["format!(\"{disco_prefix}/status\")"],
            "rebuild_router .route args outside the inventory: {args:?}"
        );
        assert_eq!(
            inventory_hits.len(),
            REGISTERED_COMMAND_ROUTE_NAMES.len(),
            "rebuild_router registered {inventory_hits:?}"
        );
        for name in REGISTERED_COMMAND_ROUTE_NAMES {
            let hits = inventory_hits.iter().filter(|hit| *hit == name).count();
            assert_eq!(hits, 1, "{name} must be passed to .route exactly once");
        }
    }

    /// A constructor that `format!`s a gv2mqtt command topic by hand can
    /// drift from the router. Command-shaped literals must go through
    /// `instantiate_route` (or a helper that returns an inventory const).
    #[test]
    fn advertised_command_topics_are_not_raw_format_strings() {
        let mut offenders = Vec::new();
        for (name, src) in hass_mqtt_entity_sources() {
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
    fn oneclick_and_purge_helpers_return_inventory_routes() {
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
    }
}
