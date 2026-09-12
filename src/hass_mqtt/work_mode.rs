use crate::platform_api::{DeviceCapability, DeviceParameters, EnumOption};
use crate::service::device::Device as ServiceDevice;
use anyhow::anyhow;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::ops::Range;

#[derive(Default, Debug)]
pub struct ParsedWorkMode {
    pub modes: BTreeMap<String, WorkMode>,
}

impl ParsedWorkMode {
    pub fn with_device(device: &ServiceDevice) -> anyhow::Result<Self> {
        let info = device
            .http_device_info
            .as_ref()
            .ok_or_else(|| anyhow!("no platform state, so no known work mode"))?;
        let cap = info
            .capability_by_instance("workMode")
            .ok_or_else(|| anyhow!("device has no workMode capability"))?;
        let mut parsed = Self::with_capability(cap)?;
        parsed.adjust_for_device(&device.sku);
        Ok(parsed)
    }

    pub fn with_capability(cap: &DeviceCapability) -> anyhow::Result<Self> {
        let mut work_modes = Self::default();

        let wm = cap
            .struct_field_by_name("workMode")
            .ok_or_else(|| anyhow!("workMode not found in {cap:?}"))?;

        if let DeviceParameters::Enum { options } = &wm.field_type {
            for opt in options {
                work_modes.add(opt.name.to_string(), opt.value.clone());
            }
        }

        if let Some(mv) = cap.struct_field_by_name("modeValue") {
            if let DeviceParameters::Enum { options } = &mv.field_type {
                let mut joined = 0usize;
                for opt in options {
                    let mode_name = &opt.name;
                    if let Some(work_mode) = work_modes.get_mut(mode_name) {
                        work_mode.add_values(opt);
                        joined += 1;
                    }
                }

                // The two enums are joined by exact name. Govee returns
                // localized names for some accounts (upstream
                // wez/govee2mqtt#645), and when only one side is translated
                // every join misses: the modes survive but lose their speed
                // values, silently. Say so, rather than presenting a device
                // that looks like it simply has no speeds.
                if !options.is_empty() && joined == 0 {
                    log::warn!(
                        "workMode metadata for {cap:?} has {n} modeValue \
                         option(s) but none of their names match a workMode \
                         name ({names:?}); treating the device as having no \
                         speed values. Localized enum names are the usual cause.",
                        n = options.len(),
                        names = work_modes.get_mode_names(),
                    );
                }
            }
        }
        Ok(work_modes)
    }

    pub fn add(&mut self, name: String, value: JsonValue) {
        self.modes.insert(
            name.clone(),
            WorkMode {
                name,
                value,
                ..WorkMode::default()
            },
        );
    }

    pub fn get_mut(&mut self, mode: &str) -> Option<&mut WorkMode> {
        self.modes.get_mut(mode)
    }

    pub fn adjust_for_device(&mut self, sku: &str) {
        match sku {
            "H7160" | "H7143" => {
                if let Some(m) = self.modes.get_mut("Manual") {
                    m.label = "Manual: Mist Level".to_string();
                }
            }
            "H7131" | "H7173" => {
                if let Some(m) = self.modes.get_mut("gearMode") {
                    m.label = "Heat".to_string();
                }
            }
            _ => {
                for mode in self.modes.values_mut() {
                    mode.label = mode.name.clone();
                }
            }
        }
    }

    pub fn mode_for_value(&self, value: &JsonValue) -> Option<&WorkMode> {
        self.modes
            .values()
            .find(|&mode| mode.value == *value)
            .map(|v| v as _)
    }

    pub fn mode_by_name(&self, name: &str) -> Option<&WorkMode> {
        self.modes.get(name)
    }

    #[allow(unused)]
    pub fn mode_by_label(&self, name: &str) -> Option<&WorkMode> {
        self.modes
            .values()
            .find(|&mode| mode.label() == name)
            .map(|v| v as _)
    }

    pub fn get_mode_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self
            .modes
            .values()
            .map(|mode| mode.name.to_string())
            .collect();
        names.sort();
        names
    }

    #[allow(unused)]
    pub fn get_mode_labels(&self) -> Vec<String> {
        let mut names: Vec<_> = self
            .modes
            .values()
            .map(|mode| mode.label().to_string())
            .collect();
        names.sort();
        names
    }

    #[allow(unused)]
    pub fn modes_with_values(&self) -> impl Iterator<Item = &WorkMode> {
        self.modes.values().filter(|mode| !mode.values.is_empty())
    }
}

#[derive(Default, Debug)]
pub struct WorkMode {
    pub name: String,
    pub value: JsonValue,
    pub default_value: Option<JsonValue>,
    pub label: String,
    pub values: Vec<WorkModeValue>,
    pub value_range: Option<Range<i64>>,
}

#[derive(Debug)]
pub struct WorkModeValue {
    pub value: JsonValue,
    pub name: Option<String>,
    pub computed_label: String,
}

impl WorkMode {
    pub fn add_values(&mut self, opt: &EnumOption) {
        self.default_value = opt.extras.get("defaultValue").cloned();

        #[derive(Deserialize)]
        struct ModeRange {
            min: i64,
            max: i64,
        }

        if let Some(range) = opt.extras.get("range") {
            if let Ok(range) = serde_json::from_value::<ModeRange>(range.clone()) {
                self.value_range = Some(range.min..range.max + 1);
                return;
            }
        }

        #[derive(Deserialize)]
        struct ModeOption {
            name: Option<String>,
            value: JsonValue,
        }

        let Some(options) = opt.extras.get("options") else {
            return;
        };

        let Ok(options) = serde_json::from_value::<Vec<ModeOption>>(options.clone()) else {
            return;
        };

        for opt in options {
            self.values.push(WorkModeValue {
                value: opt.value,
                name: opt.name,
                computed_label: String::new(),
            });
        }

        if let Some(range) = self.contiguous_value_range() {
            self.values.clear();
            self.value_range.replace(range);
        } else {
            // Now spend the resources updating the labels
            for v in &mut self.values {
                let option_name = match &v.name {
                    Some(name) => name.to_string(),
                    None => v.value.to_string(),
                };
                let computed_label = format!("Activate {} Preset {option_name}", self.name);
                v.computed_label = computed_label;
            }
        }
    }

    pub fn label(&self) -> &str {
        if self.label.is_empty() {
            &self.name
        } else {
            &self.label
        }
    }

    pub fn default_value(&self) -> i64 {
        self.default_value
            .as_ref()
            .and_then(|v| v.as_i64())
            .or_else(|| self.values.first().and_then(|wmv| wmv.value.as_i64()))
            .or_else(|| self.value_range.as_ref().map(|r| r.start))
            .unwrap_or(0)
    }

    pub fn contiguous_value_range(&self) -> Option<Range<i64>> {
        if let Some(range) = &self.value_range {
            return Some(range.clone());
        }

        let mut values = vec![];
        for v in &self.values {
            let item_value = v.value.as_i64()?;
            if v.name.is_some() {
                // It's a preset mode, so it's not a contiguous
                // slider value
                return None;
            }
            values.push(item_value);
        }
        values.sort();

        let min = *values.iter().min()?;
        let max = *values.iter().max()?;

        for (expect, item) in (min..).zip(values) {
            if item != expect {
                return None;
            }
        }

        Some(min..max + 1)
    }

    pub fn should_show_as_preset(&self) -> bool {
        self.contiguous_value_range().is_none() && self.values.is_empty()
    }
}

/// How a Home Assistant fan's percentage maps onto Govee's `workMode` command.
///
/// Govee exposes fan speed in three incompatible shapes, and only one of them
/// is a per-mode value list. `steps` flattens all of them: ordinal `n` (HA's
/// 1-based percentage step) commands `steps[n - 1]`, as a
/// `(workMode, modeValue)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeedAxis {
    /// The single `workMode` that owns this axis, when the speed lives in that
    /// mode's own `modeValue` list. `None` when the axis spans several
    /// top-level work modes instead (each speed IS its own work mode).
    ///
    /// State read-back needs the distinction: with an owner, the device's
    /// reported `modeValue` selects the ordinal; without one, its `workMode`
    /// does.
    pub owner: Option<i64>,
    /// `(workMode, modeValue)` per ordinal, lowest speed first.
    pub steps: Vec<(i64, i64)>,
}

impl SpeedAxis {
    /// HA's `speed_range_max`. `speed_range_min` is always 1; 0 is reserved by
    /// HA for "off" and is never a step.
    pub fn max_ordinal(&self) -> usize {
        self.steps.len()
    }

    /// The command pair for a 1-based ordinal, or `None` when out of range.
    pub fn command_for_ordinal(&self, ordinal: usize) -> Option<(i64, i64)> {
        ordinal
            .checked_sub(1)
            .and_then(|index| self.steps.get(index))
            .copied()
    }

    /// The ordinal a reported device state sits on, or `None` when the device
    /// is currently in a preset rather than on this axis.
    pub fn ordinal_for_state(&self, work_mode: i64, mode_value: Option<i64>) -> Option<usize> {
        match self.owner {
            Some(owner) => {
                if work_mode != owner {
                    return None;
                }
                let mode_value = mode_value?;
                self.steps
                    .iter()
                    .position(|(_, value)| *value == mode_value)
                    .map(|index| index + 1)
            }
            None => self
                .steps
                .iter()
                .position(|(mode, _)| *mode == work_mode)
                .map(|index| index + 1),
        }
    }
}

/// The speed axis and preset list a Home Assistant fan should advertise.
#[derive(Debug)]
pub struct FanControls<'a> {
    /// `None` means the device has no usable speed axis, so the fan advertises
    /// `preset_modes` only and omits every `percentage_*` key. That is the
    /// honest outcome for a device whose work modes are not a speed scale --
    /// inventing an axis would command values the device has no mode for.
    pub axis: Option<SpeedAxis>,
    /// Every work mode that is not part of the axis.
    pub presets: Vec<&'a WorkMode>,
}

impl ParsedWorkMode {
    /// Split the parsed work modes into one speed axis plus presets.
    ///
    /// Three real shapes, measured from `test-data/list_devices_issue4.json`
    /// and upstream wez/govee2mqtt#510, need three rules. A single rule covers
    /// exactly one of them:
    ///
    /// | Shape | Example | Detected by |
    /// |---|---|---|
    /// | unnamed contiguous sub-options | H7111 `FanSpeed` 1..8 | `value_range` (`add_values` clears `values` for these) |
    /// | named sub-options | H7123 `gearMode` Low/Medium/High | non-empty `values` |
    /// | speeds as top-level modes | H7121 Low=1/Medium=2/High=3, Sleep=16 | contiguous run of `workMode` values |
    ///
    /// Only ever called for `DeviceType::Fan` / `AirPurifier`, which is what
    /// keeps the third rule safe: a kettle's `Tea`/`Coffee`/`DIY` are also
    /// contiguous top-level modes, but a kettle never gets a fan entity.
    pub fn classify_fan_controls(&self) -> FanControls<'_> {
        // Step 1: which modes could carry a speed axis in their own values?
        let mut candidates: Vec<(i64, SpeedAxis)> = vec![];

        for mode in self.modes.values() {
            let Some(mode_num) = mode.value.as_i64() else {
                // A mode with no integer value cannot be commanded at all;
                // `entities_for_work_mode` skips these too.
                continue;
            };

            if let Some(range) = &mode.value_range {
                let steps = range.clone().map(|value| (mode_num, value)).collect();
                candidates.push((
                    mode_num,
                    SpeedAxis {
                        owner: Some(mode_num),
                        steps,
                    },
                ));
            } else if !mode.values.is_empty() {
                let values: Option<Vec<i64>> = mode
                    .values
                    .iter()
                    .map(|value| value.value.as_i64())
                    .collect();
                if let Some(values) = values {
                    let steps = values.into_iter().map(|value| (mode_num, value)).collect();
                    candidates.push((
                        mode_num,
                        SpeedAxis {
                            owner: Some(mode_num),
                            steps,
                        },
                    ));
                }
            }
        }

        // Step 2: pick one. `modes` is a BTreeMap keyed by NAME, so its
        // iteration order is alphabetical and has nothing to do with the
        // order Govee sent -- tie-break on the numeric mode value instead.
        candidates.sort_by_key(|(mode_num, _)| *mode_num);
        if candidates.len() > 1 {
            log::warn!(
                "work mode metadata offers {n} candidate speed axes ({modes:?}); \
                 using the lowest workMode value and treating the rest as presets",
                n = candidates.len(),
                modes = candidates.iter().map(|(m, _)| *m).collect::<Vec<_>>(),
            );
        }

        if let Some((owner, axis)) = candidates.into_iter().next() {
            let presets = self
                .modes
                .values()
                .filter(|mode| mode.value.as_i64() != Some(owner))
                .collect();
            return FanControls {
                axis: Some(axis),
                presets,
            };
        }

        // Step 3: no mode carries its own speeds, so the top-level modes may
        // themselves be the speed scale. Take the contiguous run starting at
        // the lowest value; anything above the break (H7121's Sleep=16) is a
        // preset, not speed 4.
        let mut numbered: Vec<(i64, &WorkMode)> = self
            .modes
            .values()
            .filter_map(|mode| mode.value.as_i64().map(|value| (value, mode)))
            .collect();
        numbered.sort_by_key(|(value, _)| *value);

        let mut run_len = 0usize;
        for (index, (value, _)) in numbered.iter().enumerate() {
            match numbered.first() {
                Some((first, _)) if *value == first + index as i64 => run_len = index + 1,
                _ => break,
            }
        }

        // A run of one is not a scale -- that is just a device with a single
        // mode, and a 1-step slider would be a worse control than a button.
        if run_len < 2 {
            return FanControls {
                axis: None,
                presets: self.modes.values().collect(),
            };
        }

        let steps: Vec<(i64, i64)> = numbered[..run_len]
            .iter()
            .map(|(value, mode)| (*value, mode.default_value()))
            .collect();
        let presets = numbered[run_len..].iter().map(|(_, mode)| *mode).collect();

        FanControls {
            axis: Some(SpeedAxis { owner: None, steps }),
            presets,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::platform_api::{from_json, DeviceCapabilityKind, HttpDeviceInfo, StructField};
    use serde_json::json;
    use std::collections::HashMap;

    /// Pull one SKU's `workMode` capability out of the shared device-list
    /// fixture. These assertions are the whole reason the classifier has three
    /// rules instead of one: an earlier single-rule draft looked correct in
    /// prose and produced no speed axis for either device in this file.
    fn work_mode_cap_for(sku: &str) -> DeviceCapability {
        // A local shape rather than `GetDevicesResponse`, which is private to
        // `platform_api`; widening real visibility for a test would be the
        // wrong trade.
        #[derive(serde::Deserialize)]
        struct DeviceListFixture {
            data: Vec<HttpDeviceInfo>,
        }

        let resp: DeviceListFixture =
            from_json(include_str!("../../test-data/list_devices_issue4.json")).unwrap();
        resp.data
            .iter()
            .find(|device| device.sku == sku)
            .unwrap_or_else(|| panic!("{sku} is in list_devices_issue4.json"))
            .capabilities
            .iter()
            .find(|cap| cap.instance == "workMode")
            .unwrap_or_else(|| panic!("{sku} has a workMode capability"))
            .clone()
    }

    fn preset_names(controls: &FanControls<'_>) -> Vec<String> {
        let mut names: Vec<String> = controls
            .presets
            .iter()
            .map(|mode| mode.name.clone())
            .collect();
        names.sort();
        names
    }

    /// A work mode whose speeds are unnamed and contiguous: `add_values`
    /// collapses them into `value_range` and CLEARS `values`, so a rule that
    /// looks only at `values` finds nothing here.
    #[test]
    fn h7111_fan_speed_range_becomes_the_axis() {
        let wm = ParsedWorkMode::with_capability(&work_mode_cap_for("H7111")).unwrap();
        let controls = wm.classify_fan_controls();

        let presets = preset_names(&controls);
        let axis = controls.axis.expect("H7111 FanSpeed is a speed axis");
        assert_eq!(axis.owner, Some(1), "FanSpeed is workMode 1");
        assert_eq!(axis.max_ordinal(), 8, "FanSpeed exposes 8 speeds");
        assert_eq!(axis.command_for_ordinal(1), Some((1, 1)));
        assert_eq!(axis.command_for_ordinal(8), Some((1, 8)));
        assert_eq!(
            presets,
            vec!["Auto", "Custom", "Nature", "Sleep", "Storm"],
            "every non-speed mode is a preset"
        );
    }

    /// A purifier whose speeds ARE the top-level work modes, with one outlier.
    /// `values` and `value_range` are both empty for every mode, so rules 1
    /// and 2 find nothing -- this is the case that would otherwise ship a fan
    /// card with no speed slider at all.
    #[test]
    fn h7121_top_level_modes_become_the_axis_with_sleep_as_a_preset() {
        let wm = ParsedWorkMode::with_capability(&work_mode_cap_for("H7121")).unwrap();
        let controls = wm.classify_fan_controls();

        let presets = preset_names(&controls);
        let axis = controls
            .axis
            .expect("H7121 Low/Medium/High is a speed axis");
        assert_eq!(
            axis.owner, None,
            "the axis spans several work modes, so no single mode owns it"
        );
        assert_eq!(axis.max_ordinal(), 3, "Low=1, Medium=2, High=3");
        assert_eq!(axis.command_for_ordinal(1).map(|(mode, _)| mode), Some(1));
        assert_eq!(axis.command_for_ordinal(3).map(|(mode, _)| mode), Some(3));
        assert_eq!(
            presets,
            vec!["Sleep"],
            "Sleep=16 breaks the run, so it is a preset rather than speed 4"
        );
    }

    /// The named-sub-option shape. No purifier fixture has it yet (upstream
    /// #510 documents H7123); the H7131 space heater is the only device in the
    /// repo that does, so it stands in for the SHAPE here. A heater never gets
    /// a fan entity -- this asserts the classifier, not a product decision.
    #[test]
    fn named_sub_options_become_the_axis() {
        let wm = ParsedWorkMode::with_capability(&work_mode_cap_for("H7131")).unwrap();
        let controls = wm.classify_fan_controls();

        let axis = controls
            .axis
            .expect("gearMode Low/Medium/High is a speed axis");
        let gear_mode = wm.mode_by_name("gearMode").unwrap().value.as_i64().unwrap();
        assert_eq!(axis.owner, Some(gear_mode));
        assert_eq!(axis.max_ordinal(), 3);
        assert_eq!(axis.command_for_ordinal(1), Some((gear_mode, 1)));
        assert_eq!(axis.command_for_ordinal(3), Some((gear_mode, 3)));
    }

    fn modes_only(entries: &[(&str, i64)]) -> ParsedWorkMode {
        let mut wm = ParsedWorkMode::default();
        for (name, value) in entries {
            wm.add(name.to_string(), (*value).into());
        }
        wm
    }

    /// A single mode is not a scale. A one-step slider is a worse control than
    /// a button, and HA reserves 0% for off, so it would render as a toggle
    /// pretending to be a slider.
    #[test]
    fn a_single_mode_is_not_a_speed_axis() {
        // `FanControls` borrows the modes, so the parsed value must outlive it.
        let wm = modes_only(&[("Normal", 1)]);
        let controls = wm.classify_fan_controls();
        assert!(controls.axis.is_none());
        assert_eq!(preset_names(&controls), vec!["Normal"]);
    }

    /// Gaps mean the modes are a menu, not a scale. Presenting 1..5 would
    /// offer three positions the device has no mode for.
    #[test]
    fn non_contiguous_top_level_modes_are_all_presets() {
        // `FanControls` borrows the modes, so the parsed value must outlive it.
        let wm = modes_only(&[("A", 1), ("B", 3), ("C", 5)]);
        let controls = wm.classify_fan_controls();
        assert!(
            controls.axis.is_none(),
            "1, 3, 5 is not a contiguous run from the minimum"
        );
        assert_eq!(preset_names(&controls), vec!["A", "B", "C"]);
    }

    /// A run that does not start at the lowest value still yields an axis --
    /// what matters is contiguity from the minimum, not that it begins at 1.
    #[test]
    fn a_contiguous_run_need_not_start_at_one() {
        // `FanControls` borrows the modes, so the parsed value must outlive it.
        let wm = modes_only(&[("A", 5), ("B", 6), ("C", 7)]);
        let controls = wm.classify_fan_controls();
        let axis = controls.axis.expect("5, 6, 7 is contiguous");
        assert_eq!(axis.max_ordinal(), 3);
        assert_eq!(axis.command_for_ordinal(1).map(|(mode, _)| mode), Some(5));
    }

    /// `modes` is a `BTreeMap` keyed by NAME, so its order is alphabetical and
    /// unrelated to what Govee sent. With two candidate axes the tie-break
    /// must be the numeric mode value, or the chosen axis depends on spelling.
    #[test]
    fn multiple_candidate_axes_tie_break_on_mode_value_not_name() {
        let mut wm = ParsedWorkMode::default();
        // "Alpha" sorts first but carries the HIGHER mode value.
        wm.add("Alpha".to_string(), 9.into());
        wm.add("Zulu".to_string(), 2.into());
        wm.get_mut("Alpha").unwrap().value_range = Some(1..4);
        wm.get_mut("Zulu").unwrap().value_range = Some(1..4);

        let controls = wm.classify_fan_controls();
        let presets = preset_names(&controls);
        let axis = controls.axis.expect("both are candidates");
        assert_eq!(
            axis.owner,
            Some(2),
            "lowest workMode value wins, not the alphabetically first name"
        );
        assert_eq!(presets, vec!["Alpha"]);
    }

    /// Out-of-range ordinals are rejected rather than wrapping or saturating.
    /// HA publishes 0 for "off"; that must not silently become speed 1.
    #[test]
    fn ordinals_outside_the_axis_have_no_command() {
        let axis = SpeedAxis {
            owner: Some(1),
            steps: vec![(1, 1), (1, 2), (1, 3)],
        };
        assert_eq!(axis.command_for_ordinal(0), None, "0 is off, not a speed");
        assert_eq!(axis.command_for_ordinal(4), None);
        assert_eq!(axis.command_for_ordinal(usize::MAX), None);
    }

    /// State read-back differs by variant: an owned axis is selected by the
    /// reported `modeValue`, an unowned one by the reported `workMode`.
    #[test]
    fn state_read_back_uses_the_right_field_per_variant() {
        let owned = SpeedAxis {
            owner: Some(1),
            steps: vec![(1, 1), (1, 2), (1, 3)],
        };
        assert_eq!(owned.ordinal_for_state(1, Some(2)), Some(2));
        assert_eq!(
            owned.ordinal_for_state(3, Some(2)),
            None,
            "a different workMode means the device is in a preset"
        );
        assert_eq!(
            owned.ordinal_for_state(1, None),
            None,
            "an owned axis cannot be placed without a modeValue"
        );
        assert_eq!(owned.ordinal_for_state(1, Some(99)), None);

        let unowned = SpeedAxis {
            owner: None,
            steps: vec![(1, 0), (2, 0), (3, 0)],
        };
        assert_eq!(unowned.ordinal_for_state(2, None), Some(2));
        assert_eq!(
            unowned.ordinal_for_state(2, Some(0)),
            Some(2),
            "modeValue is ignored for an unowned axis"
        );
        assert_eq!(
            unowned.ordinal_for_state(16, None),
            None,
            "H7121's Sleep is a preset, not a speed"
        );
    }

    /// When Govee localizes one enum and not the other, every name join
    /// misses. The modes still exist but carry no values, so the classifier
    /// must fall through to the top-level rule rather than inventing an axis
    /// from a half-parsed mode. Upstream wez/govee2mqtt#645.
    #[test]
    fn a_failed_mode_value_name_join_yields_no_owned_axis() {
        let cap = DeviceCapability {
            kind: DeviceCapabilityKind::WorkMode,
            instance: "workMode".to_string(),
            alarm_type: None,
            event_state: None,
            parameters: Some(DeviceParameters::Struct {
                fields: vec![
                    StructField {
                        field_name: "workMode".to_string(),
                        field_type: DeviceParameters::Enum {
                            options: vec![EnumOption {
                                name: "gearMode".to_string(),
                                value: 1.into(),
                                extras: HashMap::new(),
                            }],
                        },
                        default_value: None,
                        required: true,
                    },
                    StructField {
                        field_name: "modeValue".to_string(),
                        field_type: DeviceParameters::Enum {
                            // Localized: does not match "gearMode".
                            options: vec![EnumOption {
                                name: "Gangschaltung".to_string(),
                                value: JsonValue::Null,
                                extras: [(
                                    "options".to_string(),
                                    json!([{"name": "Niedrig", "value": 1},
                                           {"name": "Hoch", "value": 3}]),
                                )]
                                .into_iter()
                                .collect(),
                            }],
                        },
                        default_value: None,
                        required: true,
                    },
                ],
            }),
        };

        let wm = ParsedWorkMode::with_capability(&cap).unwrap();
        assert!(
            wm.mode_by_name("gearMode").unwrap().values.is_empty(),
            "the join missed, so no values were attached"
        );

        let controls = wm.classify_fan_controls();
        assert!(
            controls.axis.is_none(),
            "one mode is not a run of two, so there is no axis to fabricate"
        );
        assert_eq!(preset_names(&controls), vec!["gearMode"]);
    }

    #[test]
    fn test_work_mode_parser() {
        let cap = DeviceCapability {
            kind: DeviceCapabilityKind::WorkMode,
            instance: "workMode".to_string(),
            alarm_type: None,
            event_state: None,
            parameters: Some(DeviceParameters::Struct {
                fields: vec![
                    StructField {
                        field_name: "workMode".to_string(),
                        field_type: DeviceParameters::Enum {
                            options: vec![EnumOption {
                                name: "Normal".to_string(),
                                value: 1.into(),
                                extras: HashMap::new(),
                            }],
                        },
                        default_value: None,
                        required: true,
                    },
                    StructField {
                        field_name: "modeValue".to_string(),
                        field_type: DeviceParameters::Enum {
                            options: vec![EnumOption {
                                name: "Normal".to_string(),
                                value: JsonValue::Null,
                                extras: [(
                                    "options".to_string(),
                                    json!([
                                            {"value": 1},
                                            {"value": 2},
                                            {"value": 3},
                                            {"value": 4},
                                            {"value": 5},
                                            {"value": 6},
                                            {"value": 7},
                                            {"value": 8},
                                    ]),
                                )]
                                .into_iter()
                                .collect(),
                            }],
                        },
                        default_value: None,
                        required: true,
                    },
                ],
            }),
        };

        let wm = ParsedWorkMode::with_capability(&cap).unwrap();

        // We shouldn't show this as a set of preset buttons, because
        // we should get a contiguous range that we can show as a slider
        assert!(wm
            .mode_by_name("Normal")
            .unwrap()
            .contiguous_value_range()
            .is_some());
        assert!(!wm.mode_by_name("Normal").unwrap().should_show_as_preset());

        k9::snapshot!(
            wm,
            r#"
ParsedWorkMode {
    modes: {
        "Normal": WorkMode {
            name: "Normal",
            value: Number(1),
            default_value: None,
            label: "",
            values: [],
            value_range: Some(
                1..9,
            ),
        },
    },
}
"#
        );
    }

    #[test]
    fn test_work_mode_parser2() {
        let cap: DeviceCapability =
            from_json(&include_str!("../../test-data/work-mode-issue-81.json")).unwrap();

        let wm = ParsedWorkMode::with_capability(&cap).unwrap();

        // We shouldn't show this as a set of preset buttons, because
        // we should get a contiguous range that we can show as a slider
        let auto_mode = wm.mode_by_name("Auto").unwrap();
        assert!(auto_mode.contiguous_value_range().is_some());
        k9::snapshot!(
            wm,
            r#"
ParsedWorkMode {
    modes: {
        "Auto": WorkMode {
            name: "Auto",
            value: Number(3),
            default_value: None,
            label: "",
            values: [],
            value_range: Some(
                40..81,
            ),
        },
        "Custom": WorkMode {
            name: "Custom",
            value: Number(2),
            default_value: Some(
                Number(0),
            ),
            label: "",
            values: [],
            value_range: None,
        },
        "Manual": WorkMode {
            name: "Manual",
            value: Number(1),
            default_value: None,
            label: "",
            values: [],
            value_range: Some(
                1..10,
            ),
        },
    },
}
"#
        );
    }

    #[test]
    fn test_work_mode_parser3() {
        let cap = DeviceCapability {
            kind: DeviceCapabilityKind::WorkMode,
            instance: "workMode".to_string(),
            alarm_type: None,
            event_state: None,
            parameters: Some(DeviceParameters::Struct {
                fields: vec![
                    StructField {
                        field_name: "workMode".to_string(),
                        field_type: DeviceParameters::Enum {
                            options: vec![EnumOption {
                                name: "Normal".to_string(),
                                value: 1.into(),
                                extras: HashMap::new(),
                            }],
                        },
                        default_value: None,
                        required: true,
                    },
                    StructField {
                        field_name: "modeValue".to_string(),
                        field_type: DeviceParameters::Enum {
                            options: vec![EnumOption {
                                name: "Normal".to_string(),
                                value: JsonValue::Null,
                                extras: [(
                                    "options".to_string(),
                                    json!([
                                            {"value": 1},
                                            {"value": 2},
                                            // hole here at 3
                                            {"value": 4},
                                    ]),
                                )]
                                .into_iter()
                                .collect(),
                            }],
                        },
                        default_value: None,
                        required: true,
                    },
                ],
            }),
        };

        let wm = ParsedWorkMode::with_capability(&cap).unwrap();

        assert!(wm
            .mode_by_name("Normal")
            .unwrap()
            .contiguous_value_range()
            .is_none());

        k9::snapshot!(
            wm,
            r#"
ParsedWorkMode {
    modes: {
        "Normal": WorkMode {
            name: "Normal",
            value: Number(1),
            default_value: None,
            label: "",
            values: [
                WorkModeValue {
                    value: Number(1),
                    name: None,
                    computed_label: "Activate Normal Preset 1",
                },
                WorkModeValue {
                    value: Number(2),
                    name: None,
                    computed_label: "Activate Normal Preset 2",
                },
                WorkModeValue {
                    value: Number(4),
                    name: None,
                    computed_label: "Activate Normal Preset 4",
                },
            ],
            value_range: None,
        },
    },
}
"#
        );
    }

    #[test]
    fn test_work_mode_parser4() {
        let cap: DeviceCapability =
            from_json(&include_str!("../../test-data/work-mode-issue-93.json")).unwrap();

        let wm = ParsedWorkMode::with_capability(&cap).unwrap();

        assert!(!wm.mode_by_name("FanSpeed").unwrap().should_show_as_preset());
        assert!(wm.mode_by_name("Auto").unwrap().should_show_as_preset());
        k9::snapshot!(
            wm,
            r#"
ParsedWorkMode {
    modes: {
        "Auto": WorkMode {
            name: "Auto",
            value: Number(3),
            default_value: Some(
                Number(0),
            ),
            label: "",
            values: [],
            value_range: None,
        },
        "Custom": WorkMode {
            name: "Custom",
            value: Number(2),
            default_value: Some(
                Number(0),
            ),
            label: "",
            values: [],
            value_range: None,
        },
        "FanSpeed": WorkMode {
            name: "FanSpeed",
            value: Number(1),
            default_value: None,
            label: "",
            values: [],
            value_range: Some(
                1..9,
            ),
        },
        "Nature": WorkMode {
            name: "Nature",
            value: Number(6),
            default_value: Some(
                Number(0),
            ),
            label: "",
            values: [],
            value_range: None,
        },
        "Sleep": WorkMode {
            name: "Sleep",
            value: Number(5),
            default_value: Some(
                Number(0),
            ),
            label: "",
            values: [],
            value_range: None,
        },
        "Storm": WorkMode {
            name: "Storm",
            value: Number(7),
            default_value: Some(
                Number(0),
            ),
            label: "",
            values: [],
            value_range: None,
        },
    },
}
"#
        );
    }

    #[test]
    fn test_issue100() {
        let cap: DeviceCapability =
            from_json(&include_str!("../../test-data/work-mode-issue-100.json")).unwrap();

        let mut wm = ParsedWorkMode::with_capability(&cap).unwrap();
        wm.adjust_for_device("H7173");

        k9::snapshot!(
            &wm,
            r#"
ParsedWorkMode {
    modes: {
        "Boiling": WorkMode {
            name: "Boiling",
            value: Number(2),
            default_value: Some(
                Number(0),
            ),
            label: "",
            values: [],
            value_range: None,
        },
        "Coffee": WorkMode {
            name: "Coffee",
            value: Number(4),
            default_value: None,
            label: "",
            values: [],
            value_range: Some(
                1..5,
            ),
        },
        "DIY": WorkMode {
            name: "DIY",
            value: Number(1),
            default_value: None,
            label: "",
            values: [],
            value_range: Some(
                1..5,
            ),
        },
        "Tea": WorkMode {
            name: "Tea",
            value: Number(3),
            default_value: None,
            label: "",
            values: [],
            value_range: Some(
                1..5,
            ),
        },
    },
}
"#
        );

        assert_eq!(wm.mode_by_name("Boiling").unwrap().default_value(), 0);
        assert_eq!(wm.mode_by_name("DIY").unwrap().default_value(), 1);
    }
}
