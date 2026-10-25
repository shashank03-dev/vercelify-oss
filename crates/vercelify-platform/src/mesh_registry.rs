//! The mesh's node registry (design §13.2–13.4), kept by the coordinator in
//! the platform database: invitations, enrollment (address allocation and
//! node tokens), endpoints, key rotation and revocation. Every change bumps
//! the map version, which is what tells the coordinator to push new maps.

use crate::ids::new_id;
use crate::secrets::random_token;
use crate::store::{get_setting, put_setting};
use crate::{now, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};
use vercelify_core::platform::MeshNode;
use vercelify_mesh::addr::{next_free_index, Addressing, COORDINATOR_INDEX};
use vercelify_mesh::cert::NodeKind;
use vercelify_mesh::keys::PublicKey;
use vercelify_mesh::proto::{valid_endpoint, valid_node_name, NodeRecord};

pub const MAP_VERSION: &str = "mesh.map_version";
pub const EPOCH: &str = "mesh.epoch";
/// Enrollment requests older than this are refused (replays).
pub const REQUEST_MAX_AGE_SECS: i64 = 600;
const MAX_ENDPOINTS: usize = 8;

fn hash(secret: &str) -> String {
    let d = Sha256::digest(secret.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn kind_str(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Server => "server",
        NodeKind::Device => "device",
    }
}

fn kind_of(s: &str) -> NodeKind {
    if s == "device" {
        NodeKind::Device
    } else {
        NodeKind::Server
    }
}

/// A node as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub kind: NodeKind,
    pub role: String,
    pub public_key: String,
    pub ip_index: u16,
    pub os: String,
    pub arch: String,
    pub endpoints: Vec<String>,
    pub cert_expires_at: Option<String>,
    pub revoked_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub created_at: String,
}

fn row(r: &Row) -> rusqlite::Result<Node> {
    let eps: String = r.get("endpoints")?;
    Ok(Node {
        id: r.get("id")?,
        name: r.get("name")?,
        kind: kind_of(&r.get::<_, String>("kind")?),
        role: r.get("role")?,
        public_key: r.get("public_key")?,
        ip_index: r.get("ip_index")?,
        os: r.get("os")?,
        arch: r.get("arch")?,
        endpoints: serde_json::from_str(&eps).unwrap_or_default(),
        cert_expires_at: r.get("cert_expires_at")?,
        revoked_at: r.get("revoked_at")?,
        last_seen_at: r.get("last_seen_at")?,
        created_at: r.get("created_at")?,
    })
}

impl Node {
    pub fn record(&self) -> NodeRecord {
        NodeRecord {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind,
            role: self.role.clone(),
            public_key: self.public_key.clone(),
            ip_index: self.ip_index,
            endpoints: self.endpoints.clone(),
            extra_device_ports: vec![],
            revoked: self.revoked_at.is_some(),
        }
    }

    pub fn wire(&self, a: &Addressing) -> MeshNode {
        MeshNode {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: kind_str(self.kind).into(),
            role: self.role.clone(),
            ipv4: a.ipv4(self.ip_index).to_string(),
            ipv6: a.ipv6(self.ip_index).to_string(),
            public_key: self.public_key.clone(),
            endpoints: self.endpoints.clone(),
            os: self.os.clone(),
            arch: self.arch.clone(),
            coordinator: self.ip_index == COORDINATOR_INDEX,
            last_seen_at: self.last_seen_at.clone(),
            cert_expires_at: self.cert_expires_at.clone(),
            revoked_at: self.revoked_at.clone(),
            created_at: self.created_at.clone(),
            relay_connected: false,
            relayed_bytes: 0,
        }
    }
}

/// The current map version and epoch.
pub fn versions(c: &Connection) -> Result<(u64, u64)> {
    Ok((
        get_setting(c, MAP_VERSION)?.unwrap_or(0),
        get_setting(c, EPOCH)?.unwrap_or(1),
    ))
}

/// Something every map depends on changed.
pub fn bump(c: &Connection) -> Result<u64> {
    let v = versions(c)?.0 + 1;
    put_setting(c, MAP_VERSION, &v)?;
    Ok(v)
}

pub fn list(c: &Connection) -> Result<Vec<Node>> {
    let mut st = c.prepare("SELECT * FROM mesh_nodes ORDER BY ip_index")?;
    let rows = st.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(c: &Connection, id_or_name: &str) -> Result<Node> {
    c.query_row("SELECT * FROM mesh_nodes WHERE id = ?1 OR name = ?1", [id_or_name], row)
        .optional()?
        .ok_or_else(|| Error::not_found(format!("mesh node {id_or_name}")))
}

/// The node a token belongs to, unless revoked.
pub fn by_token(c: &Connection, token: &str) -> Result<Option<Node>> {
    if !token.starts_with("vfyn_") {
        return Ok(None);
    }
    Ok(c.query_row(
        "SELECT * FROM mesh_nodes WHERE token_hash = ?1 AND revoked_at IS NULL",
        [hash(token)],
        row,
    )
    .optional()?)
}

/// The node holding a WireGuard key, unless revoked (the relay
/// authenticates by key, design §13.7).
pub fn by_public_key(c: &Connection, public_key: &PublicKey) -> Result<Option<Node>> {
    Ok(c.query_row(
        "SELECT * FROM mesh_nodes WHERE public_key = ?1 AND revoked_at IS NULL",
        [public_key.to_base64()],
        row,
    )
    .optional()?)
}

/// A single-use invitation; returns the secret (shown once).
pub fn invite(c: &Connection, kind: NodeKind, role: &str, ttl_minutes: u32) -> Result<(String, String)> {
    if !["owner", "deployer", "viewer"].contains(&role) {
        return Err(Error::Invalid(format!("unknown role `{role}`")));
    }
    let ttl = ttl_minutes.clamp(1, 7 * 24 * 60);
    let secret = random_token(32);
    let expires =
        (chrono::Utc::now() + chrono::Duration::minutes(ttl as i64)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    c.execute(
        "INSERT INTO mesh_claims (hash, kind, role, expires_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![hash(&secret), kind_str(kind), role, expires, now()],
    )?;
    Ok((secret, expires))
}

/// The coordinator's own node: index 1, a server, owner. Updated when its
/// key changes (a reinstall).
pub fn ensure_coordinator(c: &Connection, name: &str, public_key: &PublicKey) -> Result<Node> {
    let pk = public_key.to_base64();
    let existing: Option<Node> = c
        .query_row("SELECT * FROM mesh_nodes WHERE ip_index = ?1", [COORDINATOR_INDEX], row)
        .optional()?;
    match existing {
        Some(n) if n.public_key == pk && n.name == name => Ok(n),
        Some(n) => {
            c.execute(
                "UPDATE mesh_nodes SET public_key = ?2, name = ?3, revoked_at = NULL WHERE id = ?1",
                params![n.id, pk, name],
            )?;
            bump(c)?;
            get(c, &n.id)
        }
        None => {
            // A stale row may already use this name (a node renamed later).
            c.execute(
                "DELETE FROM mesh_nodes WHERE name = ?1 OR public_key = ?2",
                params![name, pk],
            )?;
            let id = new_id("mn");
            c.execute(
                "INSERT INTO mesh_nodes (id, name, kind, public_key, ip_index, role, os, arch, token_hash, created_at)
                 VALUES (?1, ?2, 'server', ?3, ?4, 'owner', ?5, ?6, ?7, ?8)",
                params![
                    id,
                    name,
                    pk,
                    COORDINATOR_INDEX,
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    hash(&format!("vfyn_{}", random_token(32))),
                    now()
                ],
            )?;
            bump(c)?;
            get(c, &id)
        }
    }
}

/// A new token for a node (the coordinator's own daemon gets one at start).
pub fn issue_token(c: &Connection, id: &str) -> Result<String> {
    let token = format!("vfyn_{}", random_token(40));
    c.execute(
        "UPDATE mesh_nodes SET token_hash = ?2 WHERE id = ?1",
        params![id, hash(&token)],
    )?;
    Ok(token)
}

/// What enrollment needs from the request.
pub struct Enrollment<'a> {
    pub claim: &'a str,
    pub name: &'a str,
    pub kind: NodeKind,
    pub os: &'a str,
    pub arch: &'a str,
    pub public_key: &'a PublicKey,
    pub endpoints: &'a [String],
    pub ts: i64,
}

/// Redeem an invitation for a new node: check it, allocate an address,
/// store the node and return it with its token.
pub fn enroll(c: &Connection, e: &Enrollment) -> Result<(Node, String)> {
    let skew = (chrono::Utc::now().timestamp() - e.ts).abs();
    if skew > REQUEST_MAX_AGE_SECS {
        return Err(Error::Invalid(
            "the request is too old (or this machine's clock is wrong): check the time on both machines".into(),
        ));
    }
    let claim: Option<(String, String, String, Option<String>)> = c
        .query_row(
            "SELECT kind, role, expires_at, used_at FROM mesh_claims WHERE hash = ?1",
            [hash(e.claim)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((kind, role, expires, used)) = claim else {
        return Err(Error::Invalid("this join token isn't valid here".into()));
    };
    if used.is_some() {
        return Err(Error::Conflict(
            "this join token was used already: make a new one".into(),
        ));
    }
    if expires < now() {
        return Err(Error::Conflict("this join token expired: make a new one".into()));
    }
    if kind_of(&kind) != e.kind {
        return Err(Error::Invalid(format!("this token is for a {kind}")));
    }
    if !valid_node_name(e.name) {
        return Err(Error::Invalid(format!("`{}` is not a valid node name", e.name)));
    }
    let pk = e.public_key.to_base64();
    // A revoked node of the same name or key makes room; a live one doesn't.
    let clash: Option<Node> = c
        .query_row(
            "SELECT * FROM mesh_nodes WHERE name = ?1 OR public_key = ?2",
            params![e.name, pk],
            row,
        )
        .optional()?;
    if let Some(n) = clash {
        if n.revoked_at.is_none() {
            return Err(Error::Conflict(if n.public_key == pk {
                format!("this machine is already on the mesh as {}", n.name)
            } else {
                format!("a node named {} is on the mesh already: pick another name", n.name)
            }));
        }
        c.execute("DELETE FROM mesh_nodes WHERE id = ?1", [&n.id])?;
    }
    let used: Vec<u16> = {
        let mut st = c.prepare("SELECT ip_index FROM mesh_nodes")?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let index = next_free_index(&used).ok_or_else(|| Error::Conflict("the mesh is full".into()))?;
    let endpoints: Vec<String> = e
        .endpoints
        .iter()
        .filter(|s| valid_endpoint(s))
        .take(MAX_ENDPOINTS)
        .cloned()
        .collect();
    let id = new_id("mn");
    let token = format!("vfyn_{}", random_token(40));
    c.execute(
        "INSERT INTO mesh_nodes (id, name, kind, public_key, ip_index, role, os, arch, token_hash, endpoints,
                                 last_seen_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
        params![
            id,
            e.name,
            kind_str(e.kind),
            pk,
            index,
            if e.kind == NodeKind::Device {
                role.as_str()
            } else {
                "owner"
            },
            e.os.chars().take(32).collect::<String>(),
            e.arch.chars().take(32).collect::<String>(),
            hash(&token),
            serde_json::to_string(&endpoints)?,
            now()
        ],
    )?;
    c.execute(
        "UPDATE mesh_claims SET used_at = ?2, used_by = ?3 WHERE hash = ?1",
        params![hash(e.claim), now(), id],
    )?;
    bump(c)?;
    Ok((get(c, &id)?, token))
}

/// Where a node says it can be reached; bumps the map only on a change.
pub fn set_endpoints(c: &Connection, id: &str, endpoints: &[String]) -> Result<bool> {
    let mut eps: Vec<String> = endpoints
        .iter()
        .filter(|s| valid_endpoint(s))
        .take(MAX_ENDPOINTS)
        .cloned()
        .collect();
    eps.dedup();
    let n = get(c, id)?;
    c.execute(
        "UPDATE mesh_nodes SET last_seen_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    if n.endpoints == eps {
        return Ok(false);
    }
    c.execute(
        "UPDATE mesh_nodes SET endpoints = ?2 WHERE id = ?1",
        params![id, serde_json::to_string(&eps)?],
    )?;
    bump(c)?;
    Ok(true)
}

pub fn touch(c: &Connection, id: &str) -> Result<()> {
    c.execute(
        "UPDATE mesh_nodes SET last_seen_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

/// A rotated key (with its renewed certificate).
pub fn rekey(c: &Connection, id: &str, public_key: &PublicKey) -> Result<Node> {
    let pk = public_key.to_base64();
    let taken: Option<String> = c
        .query_row(
            "SELECT id FROM mesh_nodes WHERE public_key = ?1 AND id != ?2",
            params![pk, id],
            |r| r.get(0),
        )
        .optional()?;
    if taken.is_some() {
        return Err(Error::Conflict("that key belongs to another node".into()));
    }
    c.execute("UPDATE mesh_nodes SET public_key = ?2 WHERE id = ?1", params![id, pk])?;
    bump(c)?;
    get(c, id)
}

pub fn set_cert_expiry(c: &Connection, id: &str, unix: i64) -> Result<()> {
    let at = chrono::DateTime::from_timestamp(unix, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    c.execute(
        "UPDATE mesh_nodes SET cert_expires_at = ?2 WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

/// Take a node off the mesh: its token stops working and peers drop it
/// with the next map. The coordinator can't revoke itself.
pub fn revoke(c: &Connection, id_or_name: &str) -> Result<Node> {
    let n = get(c, id_or_name)?;
    if n.ip_index == COORDINATOR_INDEX {
        return Err(Error::Invalid("the coordinator can't be revoked".into()));
    }
    if n.revoked_at.is_none() {
        c.execute(
            "UPDATE mesh_nodes SET revoked_at = ?2 WHERE id = ?1",
            params![n.id, now()],
        )?;
        // Removing the platform's link to it keeps the worker list honest.
        c.execute("UPDATE nodes SET mesh_node_id = NULL WHERE mesh_node_id = ?1", [&n.id])?;
        bump(c)?;
    }
    get(c, &n.id)
}

/// Forget a revoked node entirely (frees its name and address).
pub fn delete(c: &Connection, id_or_name: &str) -> Result<()> {
    let n = get(c, id_or_name)?;
    if n.revoked_at.is_none() {
        return Err(Error::Conflict(format!("revoke {} before removing it", n.name)));
    }
    c.execute("DELETE FROM mesh_nodes WHERE id = ?1", [&n.id])?;
    Ok(())
}

/// Every node, as the map builder takes them.
pub fn records(c: &Connection) -> Result<Vec<NodeRecord>> {
    Ok(list(c)?.iter().map(Node::record).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use vercelify_mesh::keys::NodeKey;

    fn enrollment<'a>(claim: &'a str, name: &'a str, key: &'a PublicKey) -> Enrollment<'a> {
        Enrollment {
            claim,
            name,
            kind: NodeKind::Server,
            os: "linux",
            arch: "aarch64",
            public_key: key,
            endpoints: &[],
            ts: chrono::Utc::now().timestamp(),
        }
    }

    #[test]
    fn invite_enroll_revoke() {
        let s = Store::open_in_memory().unwrap();
        let ck = NodeKey::generate().public();
        let main = s.tx(|c| ensure_coordinator(c, "main", &ck)).unwrap();
        assert_eq!(main.ip_index, 1);
        assert_eq!(
            s.tx(|c| ensure_coordinator(c, "main", &ck)).unwrap().id,
            main.id,
            "idempotent"
        );

        let (claim, _) = s.tx(|c| invite(c, NodeKind::Server, "owner", 60)).unwrap();
        let pk = NodeKey::generate().public();
        let v0 = s.with(versions).unwrap().0;
        let eps = vec!["192.168.1.31:51820".to_string(), "junk".to_string()];
        let mut e = enrollment(&claim, "pi5", &pk);
        e.endpoints = &eps;
        let (pi, token) = s.tx(|c| enroll(c, &e)).unwrap();
        assert_eq!(pi.ip_index, 2);
        assert_eq!(pi.endpoints, vec!["192.168.1.31:51820"]);
        assert!(s.with(versions).unwrap().0 > v0, "the map changed");
        assert_eq!(s.with(|c| by_token(c, &token)).unwrap().unwrap().id, pi.id);
        assert!(s.with(|c| by_token(c, "vfyn_nope")).unwrap().is_none());
        assert_eq!(s.with(|c| by_public_key(c, &pk)).unwrap().unwrap().id, pi.id);
        assert!(s
            .with(|c| by_public_key(c, &NodeKey::generate().public()))
            .unwrap()
            .is_none());

        // An invitation works once.
        let other = NodeKey::generate().public();
        let err = s.tx(|c| enroll(c, &enrollment(&claim, "pi6", &other))).unwrap_err();
        assert!(err.to_string().contains("used already"), "{err}");
        let err = s.tx(|c| enroll(c, &enrollment("forged", "pi6", &other))).unwrap_err();
        assert!(err.to_string().contains("isn't valid"), "{err}");

        // A second machine can't take the name; the same machine can't join twice.
        let (c2, _) = s.tx(|c| invite(c, NodeKind::Server, "owner", 60)).unwrap();
        let err = s.tx(|c| enroll(c, &enrollment(&c2, "pi5", &other))).unwrap_err();
        assert!(err.to_string().contains("pick another name"), "{err}");
        let err = s.tx(|c| enroll(c, &enrollment(&c2, "other", &pk))).unwrap_err();
        assert!(err.to_string().contains("already on the mesh"), "{err}");

        // Revoked: the token stops working, the name and key are free again.
        assert!(s.tx(|c| revoke(c, "pi5")).unwrap().revoked_at.is_some());
        assert!(s.with(|c| by_token(c, &token)).unwrap().is_none());
        assert!(
            s.with(|c| by_public_key(c, &pk)).unwrap().is_none(),
            "no relay for a revoked key"
        );
        assert!(s.tx(|c| revoke(c, "main")).is_err(), "not the coordinator");
        let (again, _) = s.tx(|c| enroll(c, &enrollment(&c2, "pi5", &pk))).unwrap();
        assert_eq!(again.ip_index, 2, "the freed address is reused");

        // Endpoints change the map only when they change.
        let v = s.with(versions).unwrap().0;
        assert!(!s.tx(|c| set_endpoints(c, &again.id, &[])).unwrap());
        assert!(s
            .tx(|c| set_endpoints(c, &again.id, &["10.0.0.5:51820".into()]))
            .unwrap());
        assert_eq!(s.with(versions).unwrap().0, v + 1);

        // Rotation.
        let new = NodeKey::generate().public();
        assert_eq!(s.tx(|c| rekey(c, &again.id, &new)).unwrap().public_key, new.to_base64());
        assert!(s.tx(|c| rekey(c, &again.id, &ck)).is_err(), "not someone else's key");
    }

    #[test]
    fn invitations_expire_and_requests_age() {
        let s = Store::open_in_memory().unwrap();
        let (claim, _) = s.tx(|c| invite(c, NodeKind::Device, "viewer", 60)).unwrap();
        s.with(|c| {
            c.execute("UPDATE mesh_claims SET expires_at = '2000-01-01T00:00:00Z'", [])?;
            Ok(())
        })
        .unwrap();
        let pk = NodeKey::generate().public();
        let mut e = enrollment(&claim, "mac", &pk);
        e.kind = NodeKind::Device;
        assert!(s.tx(|c| enroll(c, &e)).unwrap_err().to_string().contains("expired"));

        let (claim, _) = s.tx(|c| invite(c, NodeKind::Device, "viewer", 60)).unwrap();
        let mut e = enrollment(&claim, "mac", &pk);
        assert!(s
            .tx(|c| enroll(c, &e))
            .unwrap_err()
            .to_string()
            .contains("for a device"));
        e.kind = NodeKind::Device;
        e.ts -= 3600;
        assert!(s.tx(|c| enroll(c, &e)).unwrap_err().to_string().contains("too old"));
        e.ts += 3600;
        let (mac, _) = s.tx(|c| enroll(c, &e)).unwrap();
        assert_eq!(mac.role, "viewer", "a device keeps the invitation's role");
        assert!(s.tx(|c| invite(c, NodeKind::Server, "root", 5)).is_err());
    }
}
