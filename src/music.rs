//! Per-SKU music-mode style profiles for the LAN palette path.
//!
//! The Platform API can activate a music style, but its `musicMode.rgb`
//! field is a single packed integer: it cannot express a palette. Multiple
//! reactive colours are only reachable through the device's internal
//! protocol (`ptReal`), and the byte that selects the style there — the
//! "profile id" — is NOT the Platform enum value and is NOT portable
//! between SKUs. This table maps `(sku, style name)` to that byte.
//!
//! It is deliberately a standalone table rather than a `Quirk` field:
//! `Quirk` is a flat per-SKU struct, whereas this data is two-dimensional
//! (SKU × style → profile id).
//!
//! Every entry below was learned and verified on real hardware. The
//! procedure for mapping a new SKU is documented in `docs/MUSIC_MODE.md`;
//! contributions must state the SKU, firmware version and how the profile
//! id was captured.

struct SkuMusicProfiles {
    sku: &'static str,
    /// (style name, internal profile id)
    styles: &'static [(&'static str, u8)],
}

/// Verified 2026-08 on hardware owned by @peas (github.com/peas):
/// captured via app snapshots + `aa 05 13` status read-back, then each
/// style/palette combination confirmed visually. Firmware at capture
/// time: H607C 1.00.09, H6020 and H60B0 current as of 2026-08.
static MUSIC_PROFILES: &[SkuMusicProfiles] = &[
    SkuMusicProfiles {
        sku: "H607C",
        styles: &[
            ("Touching", 0x73),
            ("Rhythm", 0x72),
            ("Splash", 0x84),
            ("Stippling", 0x83),
            ("Hopping", 0x33),
            ("Luminous", 0x51),
            ("Blend", 0x4f),
            ("Fantasy", 0x74),
            ("Spring", 0x85),
        ],
    },
    SkuMusicProfiles {
        sku: "H6020",
        styles: &[
            ("Rhythm", 0x38),
            ("Beat A", 0x4e),
            ("Gridding", 0x44),
            ("Energic", 0x39),
            ("Dandelion", 0x4b),
            ("Drifting", 0x4c),
        ],
    },
    SkuMusicProfiles {
        sku: "H60B0",
        styles: &[
            ("Stippling", 0x83),
            ("Hopping", 0x33),
            ("Luminous", 0x51),
            ("Rhythm", 0x72),
            ("Flowing Light", 0x53),
            ("Sprouting", 0x30),
            ("Shiny", 0x31),
        ],
    },
];

/// Look up the internal profile id for a style on a given SKU.
/// Style names match the Govee app spelling, case-insensitively.
pub fn music_profile(sku: &str, style: &str) -> Option<u8> {
    MUSIC_PROFILES
        .iter()
        .find(|entry| entry.sku == sku)?
        .styles
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(style))
        .map(|(_, profile)| *profile)
}

/// Style names available for a SKU, for error messages and documentation.
pub fn music_styles(sku: &str) -> Option<Vec<&'static str>> {
    MUSIC_PROFILES
        .iter()
        .find(|entry| entry.sku == sku)
        .map(|entry| entry.styles.iter().map(|(name, _)| *name).collect())
}

/// Parse a `#rrggbb` colour string into an RGB triple.
pub fn parse_hex_color(color: &str) -> anyhow::Result<[u8; 3]> {
    let hex = color.strip_prefix('#').unwrap_or(color);
    anyhow::ensure!(
        hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()),
        "expected an rrggbb colour, got {color:?}"
    );
    let channel = |i| u8::from_str_radix(&hex[i..i + 2], 16).expect("validated hex");
    Ok([channel(0), channel(2), channel(4)])
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn profile_lookup_is_per_sku_and_case_insensitive() {
        // The same style name maps to different bytes on different SKUs —
        // the whole reason this table exists.
        assert_eq!(music_profile("H607C", "Rhythm"), Some(0x72));
        assert_eq!(music_profile("H6020", "rhythm"), Some(0x38));
        assert_eq!(music_profile("H6020", "Hopping"), None);
        assert_eq!(music_profile("H9999", "Rhythm"), None);
    }

    #[test]
    fn styles_listing_matches_table() {
        let styles = music_styles("H60B0").expect("H60B0 is mapped");
        assert!(styles.contains(&"Flowing Light"));
        assert_eq!(music_styles("H9999"), None);
    }

    #[test]
    fn hex_colors_parse_with_and_without_hash() {
        assert_eq!(parse_hex_color("#ff7a00").unwrap(), [0xff, 0x7a, 0x00]);
        assert_eq!(parse_hex_color("0000FF").unwrap(), [0x00, 0x00, 0xff]);
        assert!(parse_hex_color("#f00").is_err());
        assert!(parse_hex_color("red").is_err());
    }
}
