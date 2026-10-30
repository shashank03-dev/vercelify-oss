//! Pairs Moonlight with the desktop's Sunshine without the user making a
//! Sunshine login or typing a PIN. The first time, the agent sets Sunshine's
//! login itself (Sunshine allows that while it has none): a random password
//! kept in the state directory, readable by root only. Then, for each
//! pairing, it hands Sunshine the PIN the Mac started `moonlight pair` with.
//!
//! Sunshine's web interface is HTTPS with its own self-signed certificate.
//! curl talks to it (no TLS stack in the agent), pinned to the public key in
//! that certificate, so another program on its port never sees the login.

use crate::config::Config;
use crate::error::AppError;
use crate::watch::write_private;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use vercelify_core::SunshinePairOutcome;

const LOGIN_FILE: &str = "sunshine-login.json";
const LOGIN_USER: &str = "vercelify";
/// How long Moonlight gets to ask Sunshine to pair.
const WAIT: Duration = Duration::from_secs(25);
const POLL: Duration = Duration::from_millis(700);

const MANUAL: &str = "Sunshine already has a login that Vercelify didn't make. Reset it with `sudo vercelify desktop reset-login` on the server, or pair by hand: Open Sunshine setup, sign in and enter the PIN Moonlight shows";

/// One pairing at a time: two would race for Sunshine's first login.
static PAIRING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Serialize, Deserialize)]
struct Login {
    user: String,
    password: String,
}

pub async fn pair(cfg: &Config, pin: &str, name: Option<&str>) -> Result<SunshinePairOutcome, AppError> {
    pair_within(cfg, pin, name, WAIT).await
}

async fn pair_within(
    cfg: &Config,
    pin: &str,
    name: Option<&str>,
    wait: Duration,
) -> Result<SunshinePairOutcome, AppError> {
    if pin.len() != 4 || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AppError::bad_request("the PIN is four digits"));
    }
    let name = device_name(name)?;
    let _one = PAIRING.lock().await;
    let dir = sunshine_dir(cfg)
        .ok_or_else(|| AppError::unavailable("remote desktop is off here: sudo vercelify desktop on"))?;
    let key = cert_pin(&dir.join("credentials/cacert.pem")).ok_or_else(|| {
        AppError::unavailable(
            "Sunshine hasn't started on the desktop yet: log in on the laptop's screen, then try again",
        )
    })?;
    let api = Api { cfg, key };
    let login = login(&api).await?;
    let deadline = Instant::now() + wait;
    loop {
        let (code, body) = api.call("GET", "/api/pin", Some(&login), None).await?;
        match code {
            401 => return Err(AppError::conflict(MANUAL)),
            200 => match pending(&body) {
                Some(ids) => match ids.as_slice() {
                    [] => {}
                    [id] => return submit(&api, &login, Some(id), pin, &name).await,
                    _ => {
                        return Err(AppError::conflict(
                            "several devices are pairing with Sunshine at once: try again in a minute",
                        ))
                    }
                },
                // Sunshine before pairing ids: no list, the PIN goes straight in.
                None => {
                    if submit(&api, &login, None, pin, &name).await? == SunshinePairOutcome::Paired {
                        return Ok(SunshinePairOutcome::Paired);
                    }
                }
            },
            404 => {
                if submit(&api, &login, None, pin, &name).await? == SunshinePairOutcome::Paired {
                    return Ok(SunshinePairOutcome::Paired);
                }
            }
            _ => return Err(AppError::unavailable(error_of(code, &body))),
        }
        if Instant::now() >= deadline {
            return Ok(SunshinePairOutcome::NoRequest);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Hands Sunshine the PIN. With a pairing id (current Sunshine) a refusal is
/// an error; without one (older Sunshine) it may only mean Moonlight hasn't
/// asked yet, so the caller tries again.
async fn submit(
    api: &Api<'_>,
    login: &Login,
    id: Option<&str>,
    pin: &str,
    name: &str,
) -> Result<SunshinePairOutcome, AppError> {
    let mut body = json!({ "pin": pin, "name": name });
    if let Some(id) = id {
        body["pairing_id"] = json!(id);
    }
    let (code, resp) = api.call("POST", "/api/pin", Some(login), Some(&body)).await?;
    if code == 401 {
        return Err(AppError::conflict(MANUAL));
    }
    if code != 200 {
        return Err(AppError::unavailable(error_of(code, &resp)));
    }
    match (status_of(&resp), id) {
        (true, _) => Ok(SunshinePairOutcome::Paired),
        (false, None) => Ok(SunshinePairOutcome::NoRequest),
        (false, Some(_)) => Err(AppError::conflict(
            "Sunshine refused the PIN: start Launch Moonlight again",
        )),
    }
}

/// The login the agent made, or a new one Sunshine takes as its first.
async fn login(api: &Api<'_>) -> Result<Login, AppError> {
    let path = api.cfg.state_dir.join(LOGIN_FILE);
    if let Some(l) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
        return Ok(l);
    }
    let mut random = [0u8; 24];
    getrandom::fill(&mut random).map_err(|e| AppError::internal(format!("random: {e}")))?;
    let login = Login {
        user: LOGIN_USER.into(),
        password: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random),
    };
    // Saved first: a login Sunshine took is never lost to a crash.
    let saved = serde_json::to_vec(&login).map_err(|e| AppError::internal(e.to_string()))?;
    write_private(&path, &saved).map_err(|e| AppError::internal(format!("writing {}: {e}", path.display())))?;
    let body = json!({
        "currentUsername": "",
        "currentPassword": "",
        "newUsername": login.user,
        "newPassword": login.password,
        "confirmNewPassword": login.password,
    });
    let res = api.call("POST", "/api/password", None, Some(&body)).await;
    match res {
        Ok((200, resp)) if status_of(&resp) => Ok(login),
        other => {
            let _ = std::fs::remove_file(&path);
            match other {
                Ok((200 | 400 | 401, _)) => Err(AppError::conflict(MANUAL)),
                Ok((code, resp)) => Err(AppError::unavailable(error_of(code, &resp))),
                Err(e) => Err(e),
            }
        }
    }
}

struct Api<'a> {
    cfg: &'a Config,
    /// `sha256//…` of Sunshine's public key.
    key: String,
}

impl Api<'_> {
    /// One request through curl: the login and body stay out of `ps`.
    async fn call(
        &self,
        method: &str,
        path: &str,
        login: Option<&Login>,
        body: Option<&Value>,
    ) -> Result<(u16, String), AppError> {
        let base = self.cfg.sunshine_url.trim_end_matches('/');
        if base.contains(['"', '\n', '\\']) {
            return Err(AppError::internal("sunshine_url is not a plain URL"));
        }
        // The pin replaces certificate checks: Sunshine's certificate names no host.
        let mut conf = format!(
            "url = \"{base}{path}\"\nrequest = \"{method}\"\ninsecure\npinnedpubkey = \"{}\"\n",
            self.key
        );
        if let Some(l) = login {
            conf.push_str(&format!("user = \"{}:{}\"\n", l.user, l.password));
        }
        if body.is_some() {
            conf.push_str("header = \"Content-Type: application/json\"\ndata-binary = \"@-\"\n");
        }
        let mut random = [0u8; 6];
        let _ = getrandom::fill(&mut random);
        let file = self.cfg.run_dir.join(format!(
            "sunshine-{}.curl",
            random.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        write_private(&file, conf.as_bytes())
            .map_err(|e| AppError::internal(format!("writing {}: {e}", file.display())))?;
        let result = async {
            let mut child = tokio::process::Command::new(&self.cfg.curl_bin)
                .args(["-sS", "-m", "30", "-w", "\n%{http_code}", "-K"])
                .arg(&file)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| AppError::internal(format!("running curl: {e}")))?;
            if let Some(mut stdin) = child.stdin.take() {
                if let Some(b) = body {
                    let _ = stdin.write_all(b.to_string().as_bytes()).await;
                }
            }
            let out = tokio::time::timeout(Duration::from_secs(40), child.wait_with_output())
                .await
                .map_err(|_| AppError::unavailable("Sunshine did not answer in time"))?
                .map_err(|e| AppError::internal(e.to_string()))?;
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let (resp, code) = text.rsplit_once('\n').unwrap_or(("", text.as_str()));
            let code: u16 = code.trim().parse().unwrap_or(0);
            if code == 0 {
                let err = String::from_utf8_lossy(&out.stderr);
                return Err(AppError::unavailable(if err.contains("pinned") {
                    "Sunshine's certificate doesn't match the one on disk: restart Sunshine (Second screen → Restart) and try again".to_string()
                } else {
                    format!("can't reach Sunshine on this server: {}", err.trim())
                }));
            }
            Ok((code, resp.to_string()))
        }
        .await;
        let _ = std::fs::remove_file(&file);
        result
    }
}

fn status_of(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("status")
                .and_then(|s| s.as_bool().or_else(|| s.as_str().map(|s| s == "true")))
        })
        .unwrap_or(false)
}

fn error_of(code: u16, body: &str) -> String {
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_default();
    format!(
        "Sunshine answered HTTP {code}{}{msg}",
        if msg.is_empty() { "" } else { ": " }
    )
}

/// Waiting pairing requests (`GET /api/pin`); `None` from a Sunshine that
/// doesn't list them.
fn pending(body: &str) -> Option<Vec<String>> {
    let v: Value = serde_json::from_str(body).ok()?;
    let list = v.get("pairings")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|p| p.get("id")?.as_str().map(String::from))
            .collect(),
    )
}

fn device_name(name: Option<&str>) -> Result<String, AppError> {
    let name = name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Vercelify");
    if name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(AppError::bad_request(
            "the device name is at most 64 printable characters",
        ));
    }
    Ok(name.to_string())
}

/// Sunshine's configuration directory: set in the config, else the desktop
/// user's `~/.config/sunshine`.
fn sunshine_dir(cfg: &Config) -> Option<PathBuf> {
    if let Some(d) = &cfg.sunshine_dir {
        return Some(d.clone());
    }
    let user = cfg.desktop_user.as_deref()?;
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    Some(home_in(&passwd, user)?.join(".config/sunshine"))
}

fn home_in(passwd: &str, user: &str) -> Option<PathBuf> {
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 7 && f[0] == user && f[5].starts_with('/')).then(|| PathBuf::from(f[5]))
    })
}

/// curl's `sha256//…` pin of the public key in a PEM certificate.
fn cert_pin(path: &Path) -> Option<String> {
    let pem = std::fs::read_to_string(path).ok()?;
    let b64: String = pem
        .split("-----BEGIN CERTIFICATE-----")
        .nth(1)?
        .split("-----END CERTIFICATE-----")
        .next()?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let der = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let key = spki(&der)?;
    Some(format!(
        "sha256//{}",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(key))
    ))
}

/// The SubjectPublicKeyInfo element of a DER certificate.
fn spki(cert: &[u8]) -> Option<&[u8]> {
    let (0x30, cert, _, _) = der(cert)? else { return None };
    let (0x30, mut tbs, _, _) = der(cert)? else { return None };
    // [0] version (optional), serial, signature, issuer, validity, subject; then the key.
    if tbs.first() == Some(&0xa0) {
        tbs = der(tbs)?.3;
    }
    for _ in 0..5 {
        tbs = der(tbs)?.3;
    }
    let (tag, _, whole, _) = der(tbs)?;
    (tag == 0x30).then_some(whole)
}

/// A DER element: tag, contents, the whole element and what follows it.
type Der<'a> = (u8, &'a [u8], &'a [u8], &'a [u8]);

/// The DER element at the start of `b`.
fn der(b: &[u8]) -> Option<Der<'_>> {
    let (&tag, rest) = b.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        (rest[..n].iter().fold(0usize, |a, &d| (a << 8) | d as usize), &rest[n..])
    };
    if rest.len() < len {
        return None;
    }
    let head = b.len() - rest.len();
    Some((tag, &rest[..len], &b[..head + len], &rest[len..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A self-signed P-256 certificate like Sunshine's, and its pin from
    /// `openssl x509 -pubkey -noout | openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | base64`.
    const CERT: &str = "-----BEGIN CERTIFICATE-----
MIIBfTCCASKgAwIBAgIUM4/EvzQZdMt3+9PvD3s7ySLOqk8wCgYIKoZIzj0EAwIw
IzEhMB8GA1UEAwwYU3Vuc2hpbmUgR2FtZXN0cmVhbSBIb3N0MCAXDTI2MTAyOTE4
MjQzNVoYDzIxMjYxMDA1MTgyNDM1WjAjMSEwHwYDVQQDDBhTdW5zaGluZSBHYW1l
c3RyZWFtIEhvc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAS0q7NRXgYBcivx
VCIiJEptZWvYSN1nzawIanGIIZ8kmsklgsSkCi8PhPlIAWhcQOLMiJS04qVX0pxv
Rvc8QjjiozIwMDAdBgNVHQ4EFgQU8y0vHk82hX26i2CvL/y2razEgLgwDwYDVR0T
AQH/BAUwAwEB/zAKBggqhkjOPQQDAgNJADBGAiEAvaKFmF8fISVcrdxOXRoYp0d1
jFlsxeNdId4s06vKvisCIQCoBABe4APUo4nHXCZ/WyZhXlmuRSUAXGS5VFkEeFTx
7w==
-----END CERTIFICATE-----
";
    const PIN: &str = "sha256//O/6FPyYjArqRmBLDWDNiQGDo95wHKfRzOIHHG6/YwR0=";

    #[test]
    fn pins_the_certificates_public_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cacert.pem");
        std::fs::write(&path, CERT).unwrap();
        assert_eq!(cert_pin(&path).as_deref(), Some(PIN));
        std::fs::write(&path, "not a certificate").unwrap();
        assert_eq!(cert_pin(&path), None);
        assert_eq!(cert_pin(&dir.path().join("missing.pem")), None);
        // Truncated DER is refused, not misread.
        assert_eq!(spki(&[0x30, 0x82, 0xff, 0xff, 0x30]), None);
    }

    #[test]
    fn reads_pending_pairings_and_names() {
        let body = r#"{"pairings":[{"id":"0123456789abcdef0123456789abcdef","name":"","address":"10.77.0.2"}]}"#;
        assert_eq!(pending(body).unwrap(), ["0123456789abcdef0123456789abcdef"]);
        assert_eq!(pending(r#"{"pairings":[]}"#).unwrap(), Vec::<String>::new());
        assert_eq!(pending("<html>"), None);
        assert_eq!(pending(r#"{"status":true}"#), None);
        assert!(status_of(r#"{"status":true}"#) && status_of(r#"{"status":"true"}"#));
        assert!(!status_of(r#"{"status":false}"#) && !status_of("x"));
        assert_eq!(device_name(None).unwrap(), "Vercelify");
        assert_eq!(device_name(Some("  Ana's Mac ")).unwrap(), "Ana's Mac");
        assert!(device_name(Some("a\nb")).is_err());
        assert!(device_name(Some(&"x".repeat(65))).is_err());
        let passwd = "root:x:0:0:root:/root:/bin/bash\nana:x:1000:1000:Ana:/home/ana:/bin/bash\n";
        assert_eq!(home_in(passwd, "ana"), Some(PathBuf::from("/home/ana")));
        assert_eq!(home_in(passwd, "an"), None);
    }

    /// A stand-in for curl and Sunshine: answers by method and path, records
    /// each request's curl config and body.
    fn fake(dir: &Path, script: &str) -> Config {
        let bin = dir.join("curl");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nconf=$7\nurl=$(sed -n 's/^url = \"\\(.*\\)\"$/\\1/p' \"$conf\")\nmethod=$(sed -n 's/^request = \"\\(.*\\)\"$/\\1/p' \"$conf\")\nbody=$(cat)\nlog={log}\ncat \"$conf\" >>\"$log\"; printf 'BODY %s\\n' \"$body\" >>\"$log\"\nn=$(grep -c '^url' \"$log\")\n{script}\n",
                log = dir.join("log").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sun = dir.join("sunshine/credentials");
        std::fs::create_dir_all(&sun).unwrap();
        std::fs::write(sun.join("cacert.pem"), CERT).unwrap();
        Config {
            curl_bin: bin.display().to_string(),
            state_dir: dir.join("state"),
            run_dir: dir.join("run"),
            sunshine_dir: Some(dir.join("sunshine")),
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn sets_the_first_login_then_hands_over_the_pin() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake(
            dir.path(),
            r#"case "$method $url" in
  "POST https://127.0.0.1:47990/api/password") printf '{"status":true}\n200' ;;
  "GET https://127.0.0.1:47990/api/pin") if [ "$n" -lt 3 ]; then printf '{"pairings":[]}\n200'; else printf '{"pairings":[{"id":"0123456789abcdef0123456789abcdef"}]}\n200'; fi ;;
  "POST https://127.0.0.1:47990/api/pin") printf '{"status":true}\n200' ;;
  *) printf '\n404' ;;
esac"#,
        );
        let out = pair(&cfg, "4821", Some("Ana's Mac")).await.unwrap();
        assert_eq!(out, SunshinePairOutcome::Paired);
        let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
        // Pinned, the login sent once as Sunshine's first, then as basic auth.
        assert!(log
            .lines()
            .filter(|l| l.starts_with("pinnedpubkey"))
            .all(|l| l == format!("pinnedpubkey = \"{PIN}\"")));
        let saved: Login = serde_json::from_slice(&std::fs::read(cfg.state_dir.join(LOGIN_FILE)).unwrap()).unwrap();
        assert_eq!(saved.user, "vercelify");
        assert!(saved.password.len() >= 32);
        let mode = std::fs::metadata(cfg.state_dir.join(LOGIN_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(log.contains(&format!("\"newPassword\":\"{}\"", saved.password)));
        assert!(log.contains(&format!("user = \"vercelify:{}\"", saved.password)));
        assert!(log.contains(r#""pairing_id":"0123456789abcdef0123456789abcdef""#) && log.contains(r#""pin":"4821""#));
        assert!(log.contains(r#""name":"Ana's Mac""#));
        // Nothing left in the run directory, the login is never on a command line.
        assert_eq!(std::fs::read_dir(&cfg.run_dir).unwrap().count(), 0);

        // The next pairing reuses the login.
        std::fs::remove_file(dir.path().join("log")).unwrap();
        assert_eq!(pair(&cfg, "1234", None).await.unwrap(), SunshinePairOutcome::Paired);
        let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
        assert!(!log.contains("/api/password"));
    }

    #[tokio::test]
    async fn a_login_made_by_someone_else_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake(dir.path(), r#"printf '{"error":"Unauthorized"}\n401'"#);
        let err = pair(&cfg, "4821", None).await.unwrap_err();
        assert!(format!("{err:?}").contains("reset-login"), "{err:?}");
        assert!(!cfg.state_dir.join(LOGIN_FILE).exists());
    }

    #[tokio::test]
    async fn no_request_when_moonlight_never_asks() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake(
            dir.path(),
            r#"case "$url" in
  */api/password) printf '{"status":true}\n200' ;;
  *) printf '{"pairings":[]}\n200' ;;
esac"#,
        );
        let out = pair_within(&cfg, "4821", None, Duration::from_secs(2)).await.unwrap();
        assert_eq!(out, SunshinePairOutcome::NoRequest);
    }

    #[tokio::test]
    async fn older_sunshine_takes_the_pin_without_an_id() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake(
            dir.path(),
            r#"case "$method $url" in
  "POST https://127.0.0.1:47990/api/password") printf '{"status":true}\n200' ;;
  "POST https://127.0.0.1:47990/api/pin") printf '{"status":true}\n200' ;;
  *) printf 'Not Found\n404' ;;
esac"#,
        );
        assert_eq!(pair(&cfg, "4821", None).await.unwrap(), SunshinePairOutcome::Paired);
        let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
        assert!(!log.contains("pairing_id"));
    }

    #[tokio::test]
    async fn refuses_bad_input_and_a_missing_desktop() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fake(dir.path(), "exit 1");
        for bad in ["", "123", "12345", "12a4", "١٢٣٤"] {
            assert!(pair(&cfg, bad, None).await.is_err(), "{bad}");
        }
        let off = Config {
            sunshine_dir: None,
            desktop_user: None,
            ..cfg.clone()
        };
        assert!(format!("{:?}", pair(&off, "1234", None).await.unwrap_err()).contains("desktop on"));
        let unstarted = Config {
            sunshine_dir: Some(dir.path().join("nowhere")),
            ..cfg
        };
        assert!(format!("{:?}", pair(&unstarted, "1234", None).await.unwrap_err()).contains("hasn't started"));
    }
}
