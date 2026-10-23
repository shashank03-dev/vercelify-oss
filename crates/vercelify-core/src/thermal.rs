//! Temperatures from `/sys/class/thermal` and `/sys/class/hwmon`.

use crate::model::{ThermalSensor, ThermalStatus};
use crate::sysfs::{children, read_parse, read_trim};
use std::path::Path;

/// Readings outside this range are firmware garbage (e.g. -273 or 255 °C).
const PLAUSIBLE_C: std::ops::RangeInclusive<f32> = -40.0..=130.0;

/// `sys_class` is normally `/sys/class`.
pub fn read_thermal(sys_class: impl AsRef<Path>) -> ThermalStatus {
    let sys_class = sys_class.as_ref();
    let mut sensors = Vec::new();

    for zone in children(sys_class.join("thermal")) {
        let fname = zone.file_name().unwrap_or_default().to_string_lossy().to_string();
        if !fname.starts_with("thermal_zone") {
            continue;
        }
        let Some(milli) = read_parse::<i64>(zone.join("temp")) else {
            continue;
        };
        let kind = read_trim(zone.join("type")).unwrap_or(fname);
        push(&mut sensors, kind, milli);
    }

    for hw in children(sys_class.join("hwmon")) {
        let chip = read_trim(hw.join("name")).unwrap_or_else(|| "hwmon".into());
        for entry in children(&hw) {
            let f = entry.file_name().unwrap_or_default().to_string_lossy().to_string();
            let Some(idx) = f.strip_prefix("temp").and_then(|r| r.strip_suffix("_input")) else {
                continue;
            };
            let Some(milli) = read_parse::<i64>(&entry) else {
                continue;
            };
            let label = read_trim(hw.join(format!("temp{idx}_label"))).unwrap_or_else(|| format!("temp{idx}"));
            push(&mut sensors, format!("{chip}/{label}"), milli);
        }
    }

    let max_c = sensors
        .iter()
        .map(|s| s.temp_c)
        .fold(None, |acc: Option<f32>, t| Some(acc.map_or(t, |a| a.max(t))));
    ThermalStatus { sensors, max_c }
}

fn push(out: &mut Vec<ThermalSensor>, name: String, milli: i64) {
    let temp_c = (milli as f32 / 100.0).round() / 10.0;
    if PLAUSIBLE_C.contains(&temp_c) {
        out.push(ThermalSensor { name, temp_c });
    }
}
