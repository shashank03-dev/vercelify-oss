//! Command lines for the Mac side of display streaming: the ffmpeg screen
//! sender (Mac -> Linux panel) and the Moonlight client (Linux -> Mac).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum VideoEncoder {
    /// Apple VideoToolbox (macOS) — near-zero CPU.
    Hardware,
    /// libx264 ultrafast/zerolatency — works everywhere.
    Software,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CaptureSource {
    /// macOS screen via AVFoundation (`screen_index` from `capture_screens`).
    Avfoundation,
    /// Linux X11 display `:0` (for running the cockpit on a Linux desktop).
    X11,
    /// Generated test pattern — verifies the pipe without screen permissions.
    TestPattern,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct SenderConfig {
    /// Destination from the agent, e.g. `udp://100.x.y.z:5600?pkt_size=1316`.
    pub url: String,
    pub source: CaptureSource,
    #[serde(default)]
    pub screen_index: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Output width; height follows the aspect ratio. `None` = native.
    #[serde(default)]
    pub width: Option<u32>,
    pub encoder: VideoEncoder,
    #[serde(default = "yes")]
    pub show_cursor: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, PartialEq)]
pub enum StreamError {
    BadUrl,
    BadParams(&'static str),
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::BadUrl => f.write_str("stream URL must be udp:// or srt://"),
            StreamError::BadParams(m) => f.write_str(m),
        }
    }
}

/// ffmpeg arguments (without the binary) for a low-latency MPEG-TS sender.
pub fn sender_args(cfg: &SenderConfig) -> Result<Vec<String>, StreamError> {
    if !(cfg.url.starts_with("udp://") || cfg.url.starts_with("srt://")) || cfg.url.contains(char::is_whitespace) {
        return Err(StreamError::BadUrl);
    }
    if !(1..=120).contains(&cfg.fps) {
        return Err(StreamError::BadParams("fps must be 1-120"));
    }
    if !(500..=100_000).contains(&cfg.bitrate_kbps) {
        return Err(StreamError::BadParams("bitrate must be 500-100000 kbps"));
    }
    if cfg.width.is_some_and(|w| !(320..=7680).contains(&w)) {
        return Err(StreamError::BadParams("width must be 320-7680"));
    }
    let fps = cfg.fps.to_string();
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "warning", "-nostats"]
        .map(String::from)
        .to_vec();
    match cfg.source {
        CaptureSource::Avfoundation => {
            a.extend(
                [
                    "-f",
                    "avfoundation",
                    "-capture_cursor",
                    if cfg.show_cursor { "1" } else { "0" },
                ]
                .map(String::from),
            );
            a.extend(["-framerate".into(), fps.clone(), "-pixel_format".into(), "nv12".into()]);
            a.extend(["-i".into(), format!("{}:none", cfg.screen_index)]);
        }
        CaptureSource::X11 => {
            a.extend(["-f", "x11grab", "-draw_mouse", if cfg.show_cursor { "1" } else { "0" }].map(String::from));
            a.extend([
                "-framerate".into(),
                fps.clone(),
                "-i".into(),
                format!(":{}.0", cfg.screen_index),
            ]);
        }
        CaptureSource::TestPattern => {
            a.extend([
                "-re".into(),
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                format!("testsrc2=size=1280x720:rate={fps}"),
            ]);
        }
    }
    let mut vf = vec![];
    if let Some(w) = cfg.width {
        vf.push(format!("scale={w}:-2"));
    }
    if cfg.encoder == VideoEncoder::Software {
        vf.push("format=yuv420p".into());
    }
    if !vf.is_empty() {
        a.extend(["-vf".into(), vf.join(",")]);
    }
    let br = format!("{}k", cfg.bitrate_kbps);
    // Half a second of buffer keeps rate control tight without stalls.
    let buf = format!("{}k", cfg.bitrate_kbps / 2);
    let gop = (cfg.fps * 2).to_string();
    match cfg.encoder {
        VideoEncoder::Hardware => a.extend(
            [
                "-c:v",
                "h264_videotoolbox",
                "-realtime",
                "1",
                "-allow_sw",
                "1",
                "-profile:v",
                "main",
            ]
            .map(String::from),
        ),
        VideoEncoder::Software => a.extend(
            [
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-profile:v",
                "main",
            ]
            .map(String::from),
        ),
    }
    a.extend([
        "-b:v".into(),
        br.clone(),
        "-maxrate".into(),
        br,
        "-bufsize".into(),
        buf,
        "-g".into(),
        gop,
        "-bf".into(),
        "0".into(),
        "-an".into(),
        "-f".into(),
        "mpegts".into(),
        "-flush_packets".into(),
        "1".into(),
        "-muxdelay".into(),
        "0".into(),
        "-muxpreload".into(),
        "0".into(),
        cfg.url.clone(),
    ]);
    Ok(a)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct MoonlightLaunch {
    /// Host running Sunshine (mesh or LAN address).
    pub host: String,
    /// Sunshine application, usually `Desktop`.
    pub app: String,
    /// e.g. `1920x1080`; `None` = Moonlight's default.
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub fps: Option<u32>,
    #[serde(default)]
    pub bitrate_kbps: Option<u32>,
}

fn check_moonlight_host(host: &str) -> Result<(), StreamError> {
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    if !host_ok || host.starts_with('-') {
        return Err(StreamError::BadParams("invalid host"));
    }
    Ok(())
}

/// moonlight-qt CLI arguments.
pub fn moonlight_args(m: &MoonlightLaunch) -> Result<Vec<String>, StreamError> {
    check_moonlight_host(&m.host)?;
    if m.app.is_empty() || m.app.starts_with('-') || m.app.len() > 100 {
        return Err(StreamError::BadParams("invalid app name"));
    }
    let mut a = vec!["stream".to_string(), m.host.clone(), m.app.clone()];
    if let Some(r) = &m.resolution {
        let valid = r
            .split_once('x')
            .is_some_and(|(w, h)| w.parse::<u32>().is_ok() && h.parse::<u32>().is_ok());
        if !valid {
            return Err(StreamError::BadParams("resolution must look like 1920x1080"));
        }
        a.extend(["--resolution".into(), r.clone()]);
    }
    if let Some(f) = m.fps {
        a.extend(["--fps".into(), f.to_string()]);
    }
    if let Some(b) = m.bitrate_kbps {
        a.extend(["--bitrate".into(), b.to_string()]);
    }
    Ok(a)
}

/// `moonlight pair HOST --pin NNNN`: pairs with the PIN the agent hands to
/// Sunshine, so nobody types it.
pub fn moonlight_pair_args(host: &str, pin: &str) -> Result<Vec<String>, StreamError> {
    check_moonlight_host(host)?;
    if pin.len() != 4 || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err(StreamError::BadParams("the PIN is four digits"));
    }
    Ok(vec!["pair".into(), host.into(), "--pin".into(), pin.into()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moonlight_pairs_with_a_given_pin() {
        assert_eq!(
            moonlight_pair_args("127.0.0.1", "0412").unwrap(),
            ["pair", "127.0.0.1", "--pin", "0412"]
        );
        for (host, pin) in [
            ("-x", "1234"),
            ("a b", "1234"),
            ("h", "123"),
            ("h", "12345"),
            ("h", "12a4"),
        ] {
            assert!(moonlight_pair_args(host, pin).is_err(), "{host} {pin}");
        }
    }

    fn cfg() -> SenderConfig {
        SenderConfig {
            url: "udp://100.1.2.3:5600?pkt_size=1316".into(),
            source: CaptureSource::Avfoundation,
            screen_index: 2,
            fps: 60,
            bitrate_kbps: 12000,
            width: Some(1920),
            encoder: VideoEncoder::Hardware,
            show_cursor: true,
        }
    }

    #[test]
    fn mac_hardware_sender() {
        let a = sender_args(&cfg()).unwrap().join(" ");
        assert!(
            a.contains("-f avfoundation -capture_cursor 1 -framerate 60 -pixel_format nv12 -i 2:none"),
            "{a}"
        );
        assert!(a.contains("-vf scale=1920:-2"), "{a}");
        assert!(a.contains("-c:v h264_videotoolbox -realtime 1"), "{a}");
        assert!(
            a.contains("-b:v 12000k -maxrate 12000k -bufsize 6000k -g 120 -bf 0"),
            "{a}"
        );
        assert!(
            a.ends_with("-f mpegts -flush_packets 1 -muxdelay 0 -muxpreload 0 udp://100.1.2.3:5600?pkt_size=1316"),
            "{a}"
        );
    }

    #[test]
    fn software_test_pattern() {
        let mut c = cfg();
        c.source = CaptureSource::TestPattern;
        c.encoder = VideoEncoder::Software;
        c.width = None;
        let a = sender_args(&c).unwrap().join(" ");
        assert!(a.contains("-re -f lavfi -i testsrc2=size=1280x720:rate=60"), "{a}");
        assert!(a.contains("-vf format=yuv420p"), "{a}");
        assert!(a.contains("-c:v libx264 -preset ultrafast -tune zerolatency"), "{a}");
    }

    #[test]
    fn rejects_bad_input() {
        let mut c = cfg();
        c.url = "file:///etc/passwd".into();
        assert_eq!(sender_args(&c), Err(StreamError::BadUrl));
        let mut c = cfg();
        c.url = "udp://x:1 -y /tmp/o".into();
        assert_eq!(sender_args(&c), Err(StreamError::BadUrl));
        let mut c = cfg();
        c.fps = 0;
        assert!(sender_args(&c).is_err());
    }

    #[test]
    fn moonlight() {
        let m = MoonlightLaunch {
            host: "vercelify-host.vfy".into(),
            app: "Desktop".into(),
            resolution: Some("2560x1600".into()),
            fps: Some(60),
            bitrate_kbps: None,
        };
        assert_eq!(
            moonlight_args(&m).unwrap(),
            [
                "stream",
                "vercelify-host.vfy",
                "Desktop",
                "--resolution",
                "2560x1600",
                "--fps",
                "60"
            ]
        );
        let bad = MoonlightLaunch {
            host: "--evil".into(),
            ..m.clone()
        };
        assert!(moonlight_args(&bad).is_err());
        let bad = MoonlightLaunch {
            resolution: Some("big".into()),
            ..m
        };
        assert!(moonlight_args(&bad).is_err());
    }
}
