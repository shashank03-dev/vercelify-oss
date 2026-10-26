use std::fs;
use std::path::Path;
use vercelify_core::*;

fn write(root: &Path, rel: &str, val: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, format!("{val}\n")).unwrap();
}

fn laptop_power(ac: &str, cap: &str, status: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "AC/type", "Mains");
    write(r, "AC/online", ac);
    write(r, "BAT0/type", "Battery");
    write(r, "BAT0/present", "1");
    write(r, "BAT0/capacity", cap);
    write(r, "BAT0/status", status);
    write(r, "BAT0/energy_full", "38000000");
    write(r, "BAT0/energy_full_design", "45000000");
    write(r, "BAT0/charge_control_end_threshold", "80");
    // A wireless mouse must not count as a laptop battery.
    write(r, "hidpp_battery_0/type", "Battery");
    write(r, "hidpp_battery_0/scope", "Device");
    write(r, "hidpp_battery_0/capacity", "5");
    write(r, "hidpp_battery_0/status", "Discharging");
    d
}

#[test]
fn power_on_ac() {
    let d = laptop_power("1", "97", "Charging");
    let p = power::read_power(d.path());
    assert_eq!(p.ac_online, Some(true));
    assert!(!p.on_battery);
    assert_eq!(p.batteries.len(), 1);
    assert_eq!(p.lowest_capacity_pct, Some(97));
    let b = &p.batteries[0];
    assert_eq!(b.health_pct, Some(84.4));
    assert_eq!(b.charge_stop_threshold, Some(80));
}

#[test]
fn power_on_battery() {
    let d = laptop_power("0", "12", "Discharging");
    let p = power::read_power(d.path());
    assert_eq!(p.ac_online, Some(false));
    assert!(p.on_battery);
    assert_eq!(p.lowest_capacity_pct, Some(12));
}

#[test]
fn power_desktop_without_supply() {
    let d = tempfile::tempdir().unwrap();
    let p = power::read_power(d.path());
    assert_eq!(p, PowerStatus::default());
    let p = power::read_power("/definitely/missing");
    assert!(!p.on_battery);
}

#[test]
fn thermal_zones_and_hwmon() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "thermal/thermal_zone0/type", "x86_pkg_temp");
    write(r, "thermal/thermal_zone0/temp", "61000");
    write(r, "thermal/thermal_zone1/type", "iwlwifi_1");
    write(r, "thermal/thermal_zone1/temp", "-274000"); // bogus, dropped
    write(r, "thermal/cooling_device0/type", "Processor");
    write(r, "hwmon/hwmon3/name", "coretemp");
    write(r, "hwmon/hwmon3/temp1_input", "72500");
    write(r, "hwmon/hwmon3/temp1_label", "Package id 0");
    write(r, "hwmon/hwmon3/temp2_input", "70000");
    let t = thermal::read_thermal(r);
    let names: Vec<_> = t.sensors.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["x86_pkg_temp", "coretemp/Package id 0", "coretemp/temp2"]);
    assert_eq!(t.max_c, Some(72.5));
}

#[test]
fn pairing_roundtrip() {
    let p = pairing::Pairing {
        agent_url: "http://192.168.1.20:7420".into(),
        agent_token: "0123456789abcdef0123456789abcdef".into(),
        hostname: "vercelify-host".into(),
        role: Some("main".into()),
        mesh_join: None,
    };
    let code = pairing::encode(&p);
    assert!(code.starts_with("vfy1."));
    assert_eq!(pairing::decode(&code).unwrap(), p);
    // whitespace from copy/paste is tolerated
    let wrapped = format!("  {}\n{}  ", &code[..20], &code[20..]);
    assert_eq!(pairing::decode(&wrapped).unwrap(), p);
    assert_eq!(pairing::decode("abc"), Err(pairing::PairingError::Prefix));
    assert_eq!(pairing::decode("vfy1.!!!"), Err(pairing::PairingError::Encoding));

    // On the mesh: vfy2., with the join token, and only together.
    let mut m = p.clone();
    m.agent_url = "http://10.77.0.1:7420".into();
    m.mesh_join = Some("vfyj1.eyJ2IjoxfQ".into());
    let code = pairing::encode(&m);
    assert!(code.starts_with("vfy2."));
    assert_eq!(pairing::decode(&code).unwrap(), m);
    let mismatched = code.replacen("vfy2.", "vfy1.", 1);
    assert!(matches!(
        pairing::decode(&mismatched),
        Err(pairing::PairingError::Payload(_))
    ));
    m.mesh_join = Some("nonsense".into());
    assert!(matches!(
        pairing::decode(&pairing::encode(&m)),
        Err(pairing::PairingError::Payload(_))
    ));
}

#[test]
fn avfoundation_screens() {
    let s = avfoundation::parse_screens(include_str!("fixtures/avfoundation-devices.txt"));
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].index, 2);
    assert_eq!(s[0].name, "Capture screen 0");
    assert_eq!(s[1].index, 3);
}

#[test]
fn volume_vectors() {
    let vectors: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/volume-vectors.json")).unwrap();
    for v in vectors {
        let tier: StorageTier = serde_json::from_value(v["tier"].clone()).unwrap();
        let service = v["service"].as_str().unwrap();
        let name = v["name"].as_str();
        let got = volumes::resolve(tier, service, name);
        match v.get("error").and_then(|e| e.as_str()) {
            Some("service") => assert_eq!(got, Err(volumes::VolumeError::InvalidService), "{v}"),
            Some("name") => assert_eq!(got, Err(volumes::VolumeError::InvalidName), "{v}"),
            Some(other) => panic!("unknown error kind {other}"),
            None => {
                let (src, host) = got.unwrap_or_else(|e| panic!("{v}: {e}"));
                assert_eq!(src, v["source"].as_str().unwrap(), "{v}");
                assert_eq!(host, v["host_path"].as_str().unwrap(), "{v}");
            }
        }
    }
}

#[cfg(feature = "collect")]
#[test]
fn live_snapshot_smoke() {
    let c = collect::Collector::new(collect::SysPaths::default(), "test");
    let s = c.snapshot(None);
    assert!(s.cpu.cores > 0);
    assert!(s.memory.total_bytes > 0);
    assert!(!s.host.hostname.is_empty());
}

#[test]
fn pairing_fixture_from_agent_cli() {
    // Produced by `vercelify-agent pair` before the platform engine (it
    // carries Coolify's URL and token, which are ignored now); the cockpit's
    // TS decoder reads the same file.
    let p = pairing::decode(include_str!("fixtures/pairing-code.txt")).unwrap();
    assert_eq!(p.agent_url, "http://vercelify-host.tail1234.ts.net:7420");
    assert_eq!(p.hostname, "vercelify-host");
}

#[test]
fn pairing_without_role_still_decodes() {
    // Codes printed by 0.1 have no `role`.
    let code = format!(
        "vfy1.{}",
        base64_url(
            r#"{"agent_url":"http://h:7420","agent_token":"0123456789abcdef","coolify_url":"http://h:8000","hostname":"h"}"#
        )
    );
    let p = pairing::decode(&code).unwrap();
    assert_eq!(p.role, None);
    assert!(!pairing::encode(&p).is_empty());
}

fn base64_url(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
}

#[test]
fn short_codes() {
    let c = pairing::short_code(&[0, 1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(c.len(), 9);
    assert_eq!(&c[4..5], "-");
    assert!(c
        .bytes()
        .filter(|b| *b != b'-')
        .all(|b| pairing::SHORT_ALPHABET.contains(&b)));
    assert_ne!(c, pairing::short_code(&[7, 6, 5, 4, 3, 2, 1, 0]));

    assert_eq!(pairing::normalize_short_code(" k7qf-m2xd "), Some("K7QFM2XD".into()));
    assert_eq!(pairing::normalize_short_code("K7QF M2XD"), Some("K7QFM2XD".into()));
    assert_eq!(pairing::normalize_short_code("K7QF-M2X"), None);
    assert_eq!(
        pairing::normalize_short_code("K7QF-M2X0"),
        None,
        "0 is not in the alphabet"
    );
    assert_eq!(pairing::normalize_short_code("K7QF-M2XDD"), None);

    let f = pairing::ShortCodeFile {
        code: "K7QF-M2XD".into(),
        expires_at: 1000,
        pairing: "vfy1.x".into(),
    };
    assert!(f.matches("k7qfm2xd"));
    assert!(!f.matches("K7QF-M2XE"));
    assert!(!f.matches(""));
    assert!(!f.is_expired(999));
    assert!(f.is_expired(1000));
}

#[test]
fn cockpit_config_migrates_and_switches() {
    let old = r#"{"agent_url":"http://a:7420","agent_token":"t","coolify_url":"http://a:8000","coolify_token":null,"hostname":"a"}"#;
    let mut c = CockpitConfig::from_json(old).unwrap();
    assert_eq!(c.servers.len(), 1);
    assert_eq!(c.active_server().unwrap().hostname, "a");

    let b = CockpitSettings {
        agent_url: "http://b:7420".into(),
        agent_token: "t".into(),
        hostname: "b".into(),
        role: Some("worker".into()),
    };
    c.upsert(b.clone());
    assert_eq!(c.servers.len(), 2);
    assert_eq!(c.active_server().unwrap().hostname, "b");
    // re-pairing the same agent replaces it
    c.upsert(CockpitSettings {
        hostname: "b2".into(),
        ..b
    });
    assert_eq!(c.servers.len(), 2);
    assert_eq!(c.find("http://b:7420").unwrap().hostname, "b2");

    c.remove("http://b:7420");
    assert_eq!(c.servers.len(), 1);
    assert_eq!(c.active_server().unwrap().hostname, "a");
    c.remove("http://a:7420");
    assert!(c.active_server().is_none());
    assert_eq!(c.active, None);

    let round = serde_json::to_string(&c).unwrap();
    assert_eq!(CockpitConfig::from_json(&round).unwrap(), c);
    assert!(CockpitConfig::from_json("garbage").is_none());
}

#[test]
fn doctor_overall() {
    let check = |status| DoctorCheck {
        id: "x".into(),
        label: "x".into(),
        status,
        detail: String::new(),
        fix: None,
    };
    let mut r = DoctorReport {
        role: "main".into(),
        agent_version: "0".into(),
        checks: vec![check(CheckStatus::Ok)],
    };
    assert_eq!(r.overall(), CheckStatus::Ok);
    r.checks.push(check(CheckStatus::Warn));
    assert_eq!(r.overall(), CheckStatus::Warn);
    r.checks.push(check(CheckStatus::Bad));
    assert_eq!(r.overall(), CheckStatus::Bad);
}

/// Doc comments become JSDoc in the generated TypeScript: a `*/` in one
/// would end the comment early and break the cockpit's build.
#[test]
fn doc_comments_survive_typescript_generation() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            for (i, line) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
                let l = line.trim_start();
                assert!(
                    !(l.starts_with("///") && l.contains("*/")),
                    "{}:{}: `*/` in a doc comment breaks the generated TypeScript",
                    path.display(),
                    i + 1
                );
            }
        }
    }
}
