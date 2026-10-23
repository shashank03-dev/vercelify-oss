//! Parsing of `ffmpeg -f avfoundation -list_devices true -i ""` (macOS) to
//! find capturable screens for the Mac -> Linux display stream.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct CaptureScreen {
    /// Index to pass as `-i "<index>:none"`.
    pub index: u32,
    pub name: String,
}

/// Extract `Capture screen N` video devices from ffmpeg's stderr listing.
pub fn parse_screens(stderr: &str) -> Vec<CaptureScreen> {
    let mut in_video = false;
    let mut out = Vec::new();
    for line in stderr.lines() {
        if line.contains("AVFoundation video devices") {
            in_video = true;
            continue;
        }
        if line.contains("AVFoundation audio devices") {
            in_video = false;
            continue;
        }
        if !in_video {
            continue;
        }
        // "[AVFoundation indev @ 0x7f8] [3] Capture screen 0"
        let Some(rest) = line.rsplit_once("] [").map(|(_, r)| r) else {
            continue;
        };
        let Some((idx, name)) = rest.split_once("] ") else {
            continue;
        };
        if let Ok(index) = idx.parse() {
            if name.starts_with("Capture screen") {
                out.push(CaptureScreen {
                    index,
                    name: name.trim().to_string(),
                });
            }
        }
    }
    out
}
