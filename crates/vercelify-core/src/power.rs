//! Battery / AC adapter state from `/sys/class/power_supply`.

use crate::model::{Battery, PowerStatus};
use crate::sysfs::{children, read_parse, read_trim};
use std::path::Path;

pub const DEFAULT_ROOT: &str = "/sys/class/power_supply";

/// Read power state from a `power_supply` class directory.
///
/// Peripheral batteries (wireless mice etc. report `scope=Device`) are ignored.
pub fn read_power(root: impl AsRef<Path>) -> PowerStatus {
    let mut ac_online: Option<bool> = None;
    let mut batteries = Vec::new();

    for dev in children(root) {
        let name = dev.file_name().unwrap_or_default().to_string_lossy().to_string();
        let kind = read_trim(dev.join("type")).unwrap_or_default();
        let scope = read_trim(dev.join("scope")).unwrap_or_default();
        if scope.eq_ignore_ascii_case("device") {
            continue;
        }
        match kind.as_str() {
            "Mains" | "USB" | "USB_C" | "USB_PD" => {
                if let Some(v) = read_parse::<u8>(dev.join("online")) {
                    // Any online adapter means we are on external power.
                    ac_online = Some(ac_online.unwrap_or(false) || v == 1);
                }
            }
            "Battery" => {
                if read_parse::<u8>(dev.join("present")) == Some(0) {
                    continue;
                }
                let full = read_parse::<f64>(dev.join("energy_full")).or_else(|| read_parse(dev.join("charge_full")));
                let design = read_parse::<f64>(dev.join("energy_full_design"))
                    .or_else(|| read_parse(dev.join("charge_full_design")));
                let health_pct = match (full, design) {
                    (Some(f), Some(d)) if d > 0.0 => Some(((f / d) * 1000.0).round() as f32 / 10.0),
                    _ => None,
                };
                batteries.push(Battery {
                    name,
                    capacity_pct: read_parse::<u8>(dev.join("capacity")).map(|c| c.min(100)),
                    status: read_trim(dev.join("status")).unwrap_or_else(|| "Unknown".into()),
                    health_pct,
                    charge_stop_threshold: read_parse(dev.join("charge_control_end_threshold")),
                });
            }
            _ => {}
        }
    }

    let lowest_capacity_pct = batteries.iter().filter_map(|b| b.capacity_pct).min();
    let discharging = batteries.iter().any(|b| b.status == "Discharging");
    let on_battery = !batteries.is_empty()
        && match ac_online {
            Some(online) => !online,
            None => discharging,
        };

    PowerStatus {
        ac_online,
        batteries,
        on_battery,
        lowest_capacity_pct,
    }
}
