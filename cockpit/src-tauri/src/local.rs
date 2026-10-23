//! Things that run on the cockpit's own machine: its Tailscale status,
//! local disks, and the external tools used for display streaming.

use std::path::PathBuf;
use std::process::Command;
use vercelify_core::avfoundation::{parse_screens, CaptureScreen};
use vercelify_core::tailscale::{parse_status, unavailable, CLI_CANDIDATES};
use vercelify_core::{TailscaleStatus, ToolStatus};

/// GUI apps on macOS don't inherit the shell PATH, so look in the usual places.
fn find(candidates: &[&str]) -> Option<PathBuf> {
    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_absolute() {
            if p.is_file() {
                return Some(p);
            }
            continue;
        }
        let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/snap/bin"].map(PathBuf::from));
        if let Some(found) = dirs.into_iter().map(|d| d.join(c)).find(|p| p.is_file()) {
            return Some(found);
        }
    }
    None
}

pub const FFMPEG: &[&str] = &["ffmpeg"];
pub const MOONLIGHT: &[&str] = &[
    "/Applications/Moonlight.app/Contents/MacOS/Moonlight",
    "moonlight",
    "moonlight-qt",
    "/var/lib/flatpak/exports/bin/com.moonlight_stream.Moonlight",
];

pub fn ffmpeg() -> Option<PathBuf> {
    // Allow overriding for unusual installs (and tests).
    std::env::var_os("VERCELIFY_FFMPEG")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| find(FFMPEG))
}

pub fn moonlight() -> Option<PathBuf> {
    std::env::var_os("VERCELIFY_MOONLIGHT")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| find(MOONLIGHT))
}

pub fn tools() -> Vec<ToolStatus> {
    let mac = cfg!(target_os = "macos");
    let entry = |name: &str, path: Option<PathBuf>, hint: &str| ToolStatus {
        name: name.into(),
        installed: path.is_some(),
        path: path.map(|p| p.to_string_lossy().into_owned()),
        install_hint: hint.into(),
    };
    vec![
        entry(
            "tailscale",
            find(CLI_CANDIDATES),
            if mac {
                "Install Tailscale from the Mac App Store or tailscale.com/download"
            } else {
                "curl -fsSL https://tailscale.com/install.sh | sh"
            },
        ),
        entry(
            "ffmpeg",
            ffmpeg(),
            if mac {
                "brew install ffmpeg"
            } else {
                "sudo apt install ffmpeg"
            },
        ),
        entry(
            "moonlight",
            moonlight(),
            if mac {
                "Download Moonlight from moonlight-stream.org (or: brew install --cask moonlight)"
            } else {
                "flatpak install flathub com.moonlight_stream.Moonlight"
            },
        ),
    ]
}

pub fn tailscale_status() -> TailscaleStatus {
    let Some(bin) = find(CLI_CANDIDATES) else {
        return unavailable(false, "Tailscale is not installed on this computer");
    };
    match Command::new(&bin).args(["status", "--json"]).output() {
        Err(e) => unavailable(true, e.to_string()),
        Ok(o) => match parse_status(&String::from_utf8_lossy(&o.stdout)) {
            Ok(s) => s,
            Err(_) => unavailable(true, String::from_utf8_lossy(&o.stderr).trim().to_string()),
        },
    }
}

/// Screens ffmpeg can capture (macOS AVFoundation).
pub fn capture_screens() -> Result<Vec<CaptureScreen>, String> {
    if !cfg!(target_os = "macos") {
        return Ok(vec![]);
    }
    let bin = ffmpeg().ok_or("ffmpeg is not installed (brew install ffmpeg)")?;
    // Listing devices always "fails" with an I/O error; the list is on stderr.
    let out = Command::new(bin)
        .args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(parse_screens(&String::from_utf8_lossy(&out.stderr)))
}
