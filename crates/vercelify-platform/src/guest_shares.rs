//! Guest capabilities never grant agent/registry access. Negative authentication
//! decisions are Ok(None), so clock/expiry writes commit even on denied requests.
use crate::{
    apps, deployments, previews,
    secrets::{self, MasterKey},
    store, Error, Result,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;
const CONFIG: &str = "guest_domain";
const PROBE: &str = "guest-security/key-probe/v1";
pub const SESSION_SECONDS: i64 = 900;
#[derive(Debug, Clone, Serialize)]
pub struct Grant {
    pub id: String,
    pub app_id: String,
    pub pr_number: u32,
    pub deployment_id: String,
    pub host: String,
    pub created_by: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub single_use: bool,
    pub redeemed: u32,
    pub revoked_at: Option<i64>,
}
pub struct Created {
    pub grant: Grant,
    pub secret: Zeroizing<String>,
}
pub struct Session {
    pub grant: Grant,
    pub secret: Zeroizing<String>,
    pub expires_at: i64,
}
#[derive(Debug, Clone)]
pub struct Allowed {
    pub grant: Grant,
    pub expires_at: i64,
    pub asleep: bool,
    pub private_url: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub domain: Option<String>,
    #[serde(default = "default_port")]
    pub public_port: u16,
}
fn default_port() -> u16 {
    443
}
impl Default for Config {
    fn default() -> Self {
        Self {
            domain: None,
            public_port: 443,
        }
    }
}
pub fn config(c: &Connection) -> Result<Config> {
    Ok(store::get_setting(c, CONFIG)?.unwrap_or_default())
}
pub fn domains(c: &Connection) -> Result<Vec<String>> {
    let mut q = c.prepare("SELECT domain FROM guest_domains ORDER BY domain")?;
    let rows = q.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
fn overlaps(a: &str, b: &str) -> bool {
    a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"))
}
pub fn require_app_domain(c: &Connection, host: &str) -> Result<()> {
    if domains(c)?
        .iter()
        .any(|d| host == d || host.ends_with(&format!(".{d}")))
    {
        return Err(Error::Conflict(
            "this namespace is reserved for authenticated guest previews".into(),
        ));
    }
    Ok(())
}
pub fn require_app_wildcard(c: &Connection, host: &str) -> Result<()> {
    if domains(c)?.iter().any(|d| overlaps(host, d)) {
        return Err(Error::Conflict(
            "application wildcard overlaps a reserved guest namespace".into(),
        ));
    }
    Ok(())
}
pub fn configure(c: &Connection, domain: Option<&str>) -> Result<Config> {
    configure_at(c, domain, 443)
}
pub fn configure_at(c: &Connection, domain: Option<&str>, public_port: u16) -> Result<Config> {
    if public_port == 0 {
        return Err(Error::Invalid("public HTTPS port must be 1–65535".into()));
    }
    let domain = domain.map(crate::validate::host).transpose()?;
    if let Some(domain) = &domain {
        if domain.len() > 230
            || !domain.contains('.')
            || domain.ends_with(".vfy")
            || domain.ends_with(".local")
            || domain.ends_with(".localhost")
            || domain.parse::<std::net::IpAddr>().is_ok()
        {
            return Err(Error::Invalid(
                "use a dedicated HTTPS DNS domain for guest previews".into(),
            ));
        }
        let mut q = c.prepare("SELECT host FROM app_domains")?;
        let app_domains = q
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let settings: apps::DomainSettings = store::get_setting(c, apps::DOMAIN_SETTING)?.unwrap_or_default();
        if app_domains
            .iter()
            .any(|host| host == domain || host.ends_with(&format!(".{domain}")))
            || settings.wildcard.iter().any(|host| overlaps(host, domain))
        {
            return Err(Error::Conflict(
                "guest and application domain namespaces must be separate".into(),
            ));
        }
        let reserved = domains(c)?;
        if reserved.iter().any(|old| old != domain && overlaps(old, domain)) {
            return Err(Error::Conflict(
                "guest domain overlaps a retained guest namespace".into(),
            ));
        }
        if !reserved.contains(domain) && reserved.len() >= 16 {
            return Err(Error::Capacity("guest namespace history is full".into()));
        }
        c.execute("INSERT OR IGNORE INTO guest_domains(domain) VALUES(?1)", [domain])?;
    }
    let old = config(c)?;
    if old.domain != domain || old.public_port != public_port {
        revoke_all(c, chrono::Utc::now().timestamp())?;
    }
    let cfg = Config { domain, public_port };
    store::put_setting(c, CONFIG, &cfg)?;
    Ok(cfg)
}
fn revoke_all(c: &Connection, now: i64) -> Result<()> {
    c.execute("UPDATE guest_grants SET revoked_at=?1 WHERE revoked_at IS NULL", [now])?;
    c.execute("DELETE FROM guest_sessions", [])?;
    Ok(())
}
pub fn rotate(c: &Connection, now: i64) -> Result<()> {
    revoke_all(c, now)?;
    c.execute("UPDATE guest_security SET clock=?1 WHERE id=1", [now])?;
    Ok(())
}
pub fn ensure_key(c: &Connection, key: &MasterKey, now: i64) -> Result<()> {
    let raw: Option<Vec<u8>> = c.query_row("SELECT key_probe FROM guest_security WHERE id=1", [], |r| r.get(0))?;
    if raw
        .as_ref()
        .is_some_and(|raw| key.open_string(PROBE, raw).is_ok_and(|v| v.as_str() == PROBE))
    {
        return Ok(());
    }
    // Unknown/corrupt probe is never treated as the same security epoch.
    rotate(c, now)?;
    c.execute(
        "UPDATE guest_security SET key_probe=?1 WHERE id=1",
        [key.seal_str(PROBE, PROBE)],
    )?;
    Ok(())
}
fn clock(c: &Connection, now: i64) -> Result<Option<i64>> {
    let previous: i64 = c.query_row("SELECT clock FROM guest_security WHERE id=1", [], |r| r.get(0))?;
    if !(0..=253402300799).contains(&now) || now.saturating_add(2) < previous {
        return Ok(None);
    }
    let effective = now.max(previous);
    c.execute(
        "UPDATE guest_security SET clock=?1 WHERE id=1 AND clock<?1",
        [effective],
    )?;
    // Permanent expiry prevents resurrection after the clock is corrected.
    c.execute(
        "UPDATE guest_grants SET revoked_at=?1 WHERE revoked_at IS NULL AND expires_at<=?1",
        [effective],
    )?;
    c.execute("DELETE FROM guest_sessions WHERE expires_at<=?1 OR grant_id IN (SELECT id FROM guest_grants WHERE revoked_at IS NOT NULL)",[effective])?;
    Ok(Some(effective))
}
pub fn observe_clock(c: &Connection, now: i64) -> Result<bool> {
    Ok(clock(c, now)?.is_some())
}
fn hash(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}
fn token() -> Zeroizing<String> {
    Zeroizing::new(URL_SAFE_NO_PAD.encode(secrets::random::<32>()))
}
fn valid_token(raw: &str) -> bool {
    raw.len() == 43
        && raw
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Grant> {
    Ok(Grant {
        id: r.get("id")?,
        app_id: r.get("app_id")?,
        pr_number: r.get("pr_number")?,
        deployment_id: r.get("deployment_id")?,
        host: r.get("host")?,
        created_by: r.get("created_by")?,
        created_at: r.get("created_at")?,
        expires_at: r.get("expires_at")?,
        single_use: r.get("single_use")?,
        redeemed: r.get("redeemed")?,
        revoked_at: r.get("revoked_at")?,
    })
}
pub fn get(c: &Connection, id: &str) -> Result<Grant> {
    c.query_row("SELECT * FROM guest_grants WHERE id=?1", [id], row)
        .optional()?
        .ok_or_else(|| Error::not_found("guest invitation"))
}
pub fn for_host(c: &Connection, host: &str) -> Result<Option<Grant>> {
    Ok(c.query_row("SELECT * FROM guest_grants WHERE host=?1", [host], row)
        .optional()?)
}
pub fn list(c: &Connection, app: &str, pr: u32) -> Result<Vec<Grant>> {
    let mut q = c.prepare(
        "SELECT * FROM guest_grants WHERE app_id=?1 AND pr_number=?2 ORDER BY created_at DESC,rowid DESC LIMIT 100",
    )?;
    let rows = q
        .query_map(params![app, pr], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
fn current(c: &Connection, grant: &Grant, now: i64) -> Result<Option<(bool, String)>> {
    if grant.revoked_at.is_some() || grant.expires_at <= now {
        return Ok(None);
    }
    let Some(domain) = config(c)?.domain else {
        return Ok(None);
    };
    if grant.host != format!("{}.{}", grant.id.replace('_', "-"), domain) {
        return Ok(None);
    }
    let Ok(app) = apps::get(c, &grant.app_id) else {
        return Ok(None);
    };
    if !app.previews.enabled
        || app.desired == vercelify_core::platform::Desired::Stopped
        || app.previews.access != vercelify_core::platform::PreviewAccess::Private
    {
        return Ok(None);
    }
    let Some(preview) = previews::get(c, &grant.app_id, grant.pr_number)? else {
        return Ok(None);
    };
    if preview.status != "active" || preview.deployment_id.as_deref() != Some(grant.deployment_id.as_str()) {
        return Ok(None);
    }
    let Ok(deployment) = deployments::get(c, &grant.deployment_id) else {
        return Ok(None);
    };
    let main = crate::nodes::main(c)?;
    let remote_instances: u32 = c.query_row(
        "SELECT count(*) FROM instances WHERE deployment_id=?1 AND node_id!=?2",
        params![grant.deployment_id, main.id],
        |r| r.get(0),
    )?;
    if deployment.node_id.as_deref() != Some(main.id.as_str()) || remote_instances > 0 {
        return Ok(None);
    }
    if deployment.status != vercelify_core::platform::DeploymentStatus::Healthy {
        return Ok(None);
    }
    Ok(Some((preview.asleep, preview.url)))
}
pub fn create(
    c: &Connection,
    app: &str,
    pr: u32,
    actor: &str,
    ttl: i64,
    single_use: bool,
    now: i64,
) -> Result<Created> {
    if !(60..=604800).contains(&ttl) {
        return Err(Error::Invalid("invitation lifetime must be 1 minute to 7 days".into()));
    }
    let now = clock(c, now)?.ok_or_else(|| {
        Error::Unavailable(
            "guest access paused after a backward clock change; an owner can rotate guest security".into(),
        )
    })?;
    let domain=config(c)?.domain.ok_or_else(||Error::Invalid("configure a dedicated public HTTPS guest domain first; private preview URLs still need the Vercelify network".into()))?;
    let app = apps::find(c, app)?;
    let preview = previews::get(c, &app.id, pr)?.ok_or_else(|| Error::not_found("active preview"))?;
    let deployment_id = preview
        .deployment_id
        .clone()
        .ok_or_else(|| Error::Invalid("the preview has no healthy generation".into()))?;
    let count: u32 = c.query_row("SELECT count(*) FROM guest_grants WHERE revoked_at IS NULL", [], |r| {
        r.get(0)
    })?;
    if count >= 100 {
        return Err(Error::Capacity(
            "finish or revoke existing invitations before creating more".into(),
        ));
    }
    let id = crate::ids::new_id("shr");
    let secret = token();
    let grant = Grant {
        id: id.clone(),
        app_id: app.id,
        pr_number: pr,
        deployment_id,
        host: format!("{}.{}", id.replace('_', "-"), domain),
        created_by: actor.into(),
        created_at: now,
        expires_at: now + ttl,
        single_use,
        redeemed: 0,
        revoked_at: None,
    };
    if current(c, &grant, now)?.is_none() || preview.url.is_empty() {
        return Err(Error::Invalid(
            "share only an active private preview with a healthy current generation".into(),
        ));
    }
    c.execute("INSERT INTO guest_grants(id,app_id,pr_number,deployment_id,host,token_hash,created_by,created_at,expires_at,single_use) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![grant.id,grant.app_id,pr,grant.deployment_id,grant.host,hash(&secret),actor,now,grant.expires_at,single_use])?;
    Ok(Created { grant, secret })
}
/// Authentication denial is None, not Err: callers must commit clock/expiry
/// changes before translating a denial to HTTP. Store transactions serialize a
/// single-use redemption together with session creation.
pub fn redeem(c: &Connection, host: &str, secret: &str, now: i64) -> Result<Option<Session>> {
    let Some(now) = clock(c, now)? else { return Ok(None) };
    if !valid_token(secret) {
        return Ok(None);
    }
    let Some(grant) = for_host(c, host)? else {
        return Ok(None);
    };
    if current(c, &grant, now)?.is_none() || (grant.single_use && grant.redeemed > 0) {
        return Ok(None);
    }
    let expected: String = c.query_row("SELECT token_hash FROM guest_grants WHERE id=?1", [&grant.id], |r| {
        r.get(0)
    })?;
    if expected != hash(secret) {
        return Ok(None);
    }
    let (total, owned): (u32, u32) = c.query_row(
        "SELECT count(*),coalesce(sum(CASE WHEN grant_id=?1 THEN 1 ELSE 0 END),0) FROM guest_sessions",
        [&grant.id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if total >= 128 || owned >= 8 {
        return Ok(None);
    }
    let secret = token();
    let expires_at = grant.expires_at.min(now + SESSION_SECONDS);
    c.execute("UPDATE guest_grants SET redeemed=redeemed+1 WHERE id=?1", [&grant.id])?;
    c.execute(
        "INSERT INTO guest_sessions(token_hash,grant_id,created_at,expires_at) VALUES(?1,?2,?3,?4)",
        params![hash(&secret), grant.id, now, expires_at],
    )?;
    Ok(Some(Session {
        grant: get(c, &grant.id)?,
        secret,
        expires_at,
    }))
}
pub fn authorize(c: &Connection, host: &str, secret: &str, now: i64) -> Result<Option<Allowed>> {
    let Some(now) = clock(c, now)? else { return Ok(None) };
    if !valid_token(secret) {
        return Ok(None);
    }
    let session: Option<(String, i64)> = c
        .query_row(
            "SELECT grant_id,expires_at FROM guest_sessions WHERE token_hash=?1",
            [hash(secret)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((id, expires_at)) = session else {
        return Ok(None);
    };
    let grant = get(c, &id)?;
    if grant.host != host || expires_at <= now {
        return Ok(None);
    }
    let Some((asleep, private_url)) = current(c, &grant, now)? else {
        return Ok(None);
    };
    Ok(Some(Allowed {
        grant,
        expires_at,
        asleep,
        private_url,
    }))
}
pub fn revoke(c: &Connection, id: &str, now: i64) -> Result<()> {
    get(c, id)?;
    c.execute(
        "UPDATE guest_grants SET revoked_at=coalesce(revoked_at,?2) WHERE id=?1",
        params![id, now],
    )?;
    c.execute("DELETE FROM guest_sessions WHERE grant_id=?1", [id])?;
    Ok(())
}
pub fn revoke_app(c: &Connection, app: &str) -> Result<()> {
    c.execute(
        "UPDATE guest_grants SET revoked_at=coalesce(revoked_at,?2) WHERE app_id=?1",
        params![app, chrono::Utc::now().timestamp()],
    )?;
    c.execute(
        "DELETE FROM guest_sessions WHERE grant_id IN (SELECT id FROM guest_grants WHERE app_id=?1)",
        [app],
    )?;
    Ok(())
}
pub fn revoke_preview(c: &Connection, app: &str, pr: u32) -> Result<()> {
    c.execute(
        "UPDATE guest_grants SET revoked_at=coalesce(revoked_at,?3) WHERE app_id=?1 AND pr_number=?2",
        params![app, pr, chrono::Utc::now().timestamp()],
    )?;
    c.execute(
        "DELETE FROM guest_sessions WHERE grant_id IN (SELECT id FROM guest_grants WHERE app_id=?1 AND pr_number=?2)",
        params![app, pr],
    )?;
    Ok(())
}

/// Only already-authorized main-node generations get bridge-only aliases.
/// These hosts never become mesh wake targets or public unauthenticated routers.
pub fn routing_hosts(c: &Connection) -> Result<std::collections::BTreeMap<String, Vec<String>>> {
    let mut map = std::collections::BTreeMap::<String, Vec<String>>::new();
    if config(c)?.domain.is_none() {
        return Ok(map);
    }
    let now = chrono::Utc::now().timestamp();
    let mut q = c.prepare("SELECT * FROM guest_grants WHERE revoked_at IS NULL AND expires_at>?1 LIMIT 100")?;
    let grants = q.query_map([now], row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    for grant in grants {
        if current(c, &grant, now)?.is_some() {
            map.entry(format!("{}/{}", grant.app_id, grant.pr_number))
                .or_default()
                .push(grant.host);
        }
    }
    Ok(map)
}
