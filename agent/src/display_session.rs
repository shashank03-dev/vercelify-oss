//! Resolve a logged-in Linux desktop without hard-coded display/auth paths.
use crate::{config::Config, exec};
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Default)]
pub struct Session {
    pub user: Option<String>,
    pub env: BTreeMap<String, String>,
}

fn properties(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
}

fn desktop(props: &BTreeMap<String, String>) -> bool {
    props.get("Active").is_some_and(|v| v == "yes")
        && props.get("Type").is_some_and(|v| v == "x11" || v == "wayland")
        && props.get("Remote").is_some_and(|v| v == "no")
        && props.get("User").is_some_and(|v| v != "0" && v.parse::<u32>().is_ok())
}

pub async fn resolve(config: &Config) -> Result<Session, String> {
    if !cfg!(target_os = "linux") {
        return Ok(Session::default());
    }
    resolve_with(config, "loginctl").await
}

async fn resolve_with(config: &Config, loginctl: &str) -> Result<Session, String> {
    // Explicit configuration is a supported escape hatch. Test/null outputs
    // must also work on a machine with an unrelated graphical login.
    if config.display_backend != "auto"
        || !config.display_env.is_empty()
        || config.mpv_extra_args.iter().any(|a| a.starts_with("--vo="))
    {
        return Ok(Session::default());
    }
    let timeout = Duration::from_secs(2);
    let Ok(list) = exec::run(loginctl, &["list-sessions", "--no-legend", "--no-pager"], timeout).await else {
        return Ok(Session::default());
    };
    if !list.success() {
        return Ok(Session::default());
    }
    for line in list.stdout.lines().take(32) {
        let Some(id) = line.split_whitespace().next() else {
            continue;
        };
        let Ok(info) = exec::run(
            loginctl,
            &[
                "show-session",
                id,
                "-p",
                "Active",
                "-p",
                "Type",
                "-p",
                "Name",
                "-p",
                "User",
                "-p",
                "Remote",
                "-p",
                "Display",
            ],
            timeout,
        )
        .await
        else {
            continue;
        };
        let props = properties(&info.stdout);
        if !info.success() || !desktop(&props) {
            continue;
        }
        let user = props
            .get("Name")
            .filter(|v| !v.is_empty())
            .ok_or("The desktop session has no user. Log out and back in on Linux.")?;
        let uid = &props["User"];
        let manager = format!("{user}@");
        let env = exec::run(
            &config.systemctl_bin,
            &["--user", "-M", &manager, "show-environment"],
            timeout,
        )
        .await
        .map_err(|_| "Could not read the Linux desktop session. Log out and back in, then retry screen sharing.")?;
        if !env.success() {
            return Err("The Linux desktop session is unavailable. Log in on the Linux screen, then retry.".into());
        }
        let mut values = properties(&env.stdout);
        values.retain(|k, v| {
            !v.is_empty() && matches!(k.as_str(), "DISPLAY" | "WAYLAND_DISPLAY" | "XAUTHORITY" | "HOME")
        });
        values.insert("XDG_RUNTIME_DIR".into(), format!("/run/user/{uid}"));
        if props["Type"] == "wayland" {
            values.remove("DISPLAY");
            if !values.contains_key("WAYLAND_DISPLAY") {
                return Err(
                    "The Linux Wayland session has no display address. Log out and back in, then retry.".into(),
                );
            }
        } else {
            values.remove("WAYLAND_DISPLAY");
            if !values.contains_key("DISPLAY") {
                if let Some(display) = props.get("Display").filter(|v| !v.is_empty()) {
                    values.insert("DISPLAY".into(), display.clone());
                }
            }
            if !values.contains_key("DISPLAY") {
                return Err("The Linux X11 session has no display address. Log out and back in, then retry.".into());
            }
        }
        return Ok(Session {
            user: Some(user.clone()),
            env: values,
        });
    }
    Ok(Session::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn resolves_actual_session_environment_without_guessing_auth_paths() {
        use std::os::unix::fs::PermissionsExt;
        for kind in ["x11", "wayland"] {
            let dir = tempfile::tempdir().unwrap();
            let login = dir.path().join("loginctl");
            let manager = dir.path().join("systemctl");
            std::fs::write(&login, format!("#!/bin/sh\nif [ \"$1\" = list-sessions ]; then echo '1 1000 alice seat0'; else printf 'Active=yes\\nType={kind}\\nRemote=no\\nName=alice\\nUser=1000\\nDisplay=:7\\n'; fi\n")).unwrap();
            std::fs::write(&manager, "#!/bin/sh\nprintf 'WAYLAND_DISPLAY=wayland-2\\nXAUTHORITY=/run/user/1000/gdm/Xauthority\\nHOME=/home/alice\\nSECRET=ignored\\n'\n").unwrap();
            for bin in [&login, &manager] {
                std::fs::set_permissions(bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let config = Config {
                systemctl_bin: manager.to_string_lossy().into(),
                ..Default::default()
            };
            let session = resolve_with(&config, &login.to_string_lossy()).await.unwrap();
            assert_eq!(session.user.as_deref(), Some("alice"));
            assert_eq!(session.env["XDG_RUNTIME_DIR"], "/run/user/1000");
            assert_eq!(session.env["XAUTHORITY"], "/run/user/1000/gdm/Xauthority");
            assert!(!session.env.contains_key("SECRET"));
            assert_eq!(session.env.contains_key("WAYLAND_DISPLAY"), kind == "wayland");
            assert_eq!(session.env.contains_key("DISPLAY"), kind == "x11");
            if kind == "x11" {
                assert_eq!(session.env["DISPLAY"], ":7");
            }
            std::fs::write(&manager, "#!/bin/sh\nexit 1\n").unwrap();
            assert!(resolve_with(&config, &login.to_string_lossy()).await.is_err());
        }
    }

    #[test]
    fn only_active_local_non_root_desktops() {
        let base = "Active=yes\nType=wayland\nRemote=no\nName=alice\nUser=1000\n";
        assert!(desktop(&properties(base)));
        for (a, b) in [
            ("Active=yes", "Active=no"),
            ("Remote=no", "Remote=yes"),
            ("User=1000", "User=0"),
            ("Type=wayland", "Type=tty"),
        ] {
            assert!(!desktop(&properties(&base.replace(a, b))));
        }
    }
}
