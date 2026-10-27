//! Short pairing codes. `vercelify-agent pair --short` (run by
//! `sudo vercelify pair`) writes a one-time code to the run directory; the
//! cockpit, having found this host on the LAN (mDNS), trades it for the full
//! pairing code at the unauthenticated `POST /v1/pair/claim`.

use crate::config::Config;
use crate::error::AppError;
use axum::http::StatusCode;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use vercelify_core::pairing::{short_code, ShortCodeFile, SHORT_CODE_MAX_ATTEMPTS, SHORT_CODE_TTL_SECS};

pub fn code_file(cfg: &Config) -> PathBuf {
    cfg.run_dir.join("pair-code.json")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Create a fresh short code for `pairing` (replacing any previous one).
pub fn create(cfg: &Config, pairing: &str, ttl_secs: Option<u64>) -> std::io::Result<ShortCodeFile> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|e| std::io::Error::other(e.to_string()))?;
    let file = ShortCodeFile {
        code: short_code(&random),
        expires_at: unix_now() + ttl_secs.unwrap_or(SHORT_CODE_TTL_SECS).clamp(60, 24 * 3600),
        pairing: pairing.to_string(),
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&cfg.run_dir)?;
    let path = code_file(cfg);
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(&serde_json::to_vec(&file).expect("serializable"))?;
    f.sync_all()?;
    std::fs::rename(&tmp, &path)?;
    Ok(file)
}

fn read(cfg: &Config) -> Option<ShortCodeFile> {
    serde_json::from_str(&std::fs::read_to_string(code_file(cfg)).ok()?).ok()
}

pub fn is_open(cfg: &Config) -> bool {
    read(cfg).is_some_and(|f| !f.is_expired(unix_now()))
}

/// Failed attempts against the current code.
#[derive(Default)]
pub struct Claims {
    failures: Mutex<(String, u32)>,
}

impl Claims {
    pub async fn claim(&self, cfg: &Config, presented: &str) -> Result<String, AppError> {
        // One claim at a time, and every failure costs the caller a second.
        let mut failures = self.failures.lock().await;
        let Some(file) = read(cfg) else {
            return Err(AppError::not_found(
                "no pairing code is waiting on this server — run `sudo vercelify pair` there",
            ));
        };
        if file.is_expired(unix_now()) {
            let _ = std::fs::remove_file(code_file(cfg));
            return Err(AppError {
                status: StatusCode::GONE,
                message: "that pairing code expired — run `sudo vercelify pair` for a new one".into(),
            });
        }
        if failures.0 != file.code {
            *failures = (file.code.clone(), 0);
        }
        if file.matches(presented) {
            let _ = std::fs::remove_file(code_file(cfg));
            *failures = Default::default();
            return Ok(file.pairing);
        }
        failures.1 += 1;
        let left = SHORT_CODE_MAX_ATTEMPTS.saturating_sub(failures.1);
        tokio::time::sleep(Duration::from_secs(1)).await;
        if left == 0 {
            let _ = std::fs::remove_file(code_file(cfg));
            *failures = Default::default();
            return Err(AppError {
                status: StatusCode::FORBIDDEN,
                message: "too many wrong codes, so that one is gone — run `sudo vercelify pair` for a new one".into(),
            });
        }
        Err(AppError {
            status: StatusCode::FORBIDDEN,
            message: format!(
                "wrong pairing code ({left} attempt{} left)",
                if left == 1 { "" } else { "s" }
            ),
        })
    }
}

impl Claims {
    /// The mesh's short-code claim (design §13.3), on the control listener:
    /// SPAKE2 with the code, the pairing sealed under the key. The server
    /// can't tell a right code from a wrong one (only the app can open the
    /// answer), so every request spends one of the code's attempts and a
    /// second, as a wrong code does on `/v1/pair/claim`.
    pub async fn pake(
        &self,
        cfg: &Config,
        req: &vercelify_mesh::pake::PairRequest,
    ) -> Result<vercelify_mesh::pake::PairAnswer, AppError> {
        let mut failures = self.failures.lock().await;
        let Some(file) = read(cfg) else {
            return Err(AppError::not_found(
                "no pairing code is waiting on this server — run `sudo vercelify pair` there",
            ));
        };
        if file.is_expired(unix_now()) {
            let _ = std::fs::remove_file(code_file(cfg));
            return Err(AppError {
                status: StatusCode::GONE,
                message: "that pairing code expired — run `sudo vercelify pair` for a new one".into(),
            });
        }
        if failures.0 != file.code {
            *failures = (file.code.clone(), 0);
        }
        failures.1 += 1;
        if failures.1 > SHORT_CODE_MAX_ATTEMPTS {
            let _ = std::fs::remove_file(code_file(cfg));
            *failures = Default::default();
            return Err(AppError {
                status: StatusCode::FORBIDDEN,
                message: "too many tries, so that code is gone — run `sudo vercelify pair` for a new one".into(),
            });
        }
        let answer = vercelify_mesh::pake::server_answer(&file.code, req, file.pairing.as_bytes())
            .map_err(|e| AppError::bad_request(e.to_string()))?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &std::path::Path) -> Config {
        Config {
            run_dir: dir.join("run"),
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn one_time_claim() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        assert!(!is_open(&cfg));
        let f = create(&cfg, "vfy1.full", None).unwrap();
        assert!(is_open(&cfg));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(code_file(&cfg)).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let claims = Claims::default();
        assert_eq!(claims.claim(&cfg, &f.code.to_lowercase()).await.unwrap(), "vfy1.full");
        assert_eq!(
            claims.claim(&cfg, &f.code).await.unwrap_err().status,
            StatusCode::NOT_FOUND
        );
        assert!(!is_open(&cfg));
    }

    #[tokio::test(start_paused = true)]
    async fn attempts_are_limited() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let f = create(&cfg, "vfy1.full", None).unwrap();
        let wrong = if f.code == "2222-2222" {
            "3333-3333"
        } else {
            "2222-2222"
        };
        let claims = Claims::default();
        for i in 1..SHORT_CODE_MAX_ATTEMPTS {
            let e = claims.claim(&cfg, wrong).await.unwrap_err();
            assert_eq!(e.status, StatusCode::FORBIDDEN);
            assert!(
                e.message.contains(&format!("{} attempt", SHORT_CODE_MAX_ATTEMPTS - i)),
                "{}",
                e.message
            );
        }
        assert!(claims.claim(&cfg, wrong).await.unwrap_err().message.contains("gone"));
        // even the right code is useless now
        assert_eq!(
            claims.claim(&cfg, &f.code).await.unwrap_err().status,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pake_claims() {
        use vercelify_mesh::pake::app_start;
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let f = create(&cfg, "vfy2.full", None).unwrap();
        let claims = Claims::default();
        // The right code opens the answer; the code stays until it runs out
        // of attempts or expires (the server can't tell a right one).
        let (app, req) = app_start(&f.code.to_lowercase());
        let ans = claims.pake(&cfg, &req).await.unwrap();
        assert_eq!(app.finish(&ans).unwrap(), b"vfy2.full");
        // A wrong code gets an answer it can't open.
        let (app, req) = app_start("2222-2223");
        let ans = claims.pake(&cfg, &req).await.unwrap();
        assert!(app.finish(&ans).is_err());
        for _ in 2..SHORT_CODE_MAX_ATTEMPTS {
            claims.pake(&cfg, &app_start("2222-2223").1).await.unwrap();
        }
        let e = claims.pake(&cfg, &app_start(&f.code).1).await.unwrap_err();
        assert_eq!(e.status, StatusCode::FORBIDDEN);
        assert!(!is_open(&cfg), "five tries and the code is gone");
    }

    #[tokio::test]
    async fn expired_codes() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let mut f = create(&cfg, "vfy1.full", None).unwrap();
        f.expires_at = 1;
        std::fs::write(code_file(&cfg), serde_json::to_vec(&f).unwrap()).unwrap();
        assert!(!is_open(&cfg));
        assert_eq!(
            Claims::default().claim(&cfg, &f.code).await.unwrap_err().status,
            StatusCode::GONE
        );
        assert!(!code_file(&cfg).exists());
    }
}
