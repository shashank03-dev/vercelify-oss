//! Deploy hooks (design §11): `POST /v1/hooks/deploy/<secret>` deploys an
//! app, for CI or any service that can call a URL. The secret (256 bits) is
//! shown once and stored only as a SHA-256 hash.

use crate::ids::new_id;
use crate::secrets::random_token;
use crate::{now, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use vercelify_core::platform::DeployHook;

pub const PREFIX: &str = "vfyh_";
pub const MAX_PER_APP: i64 = 10;

fn hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn list(c: &Connection, app_id: &str) -> Result<Vec<DeployHook>> {
    let mut st =
        c.prepare("SELECT id, created_at, last_used_at FROM deploy_hooks WHERE app_id = ?1 ORDER BY created_at")?;
    let rows = st.query_map([app_id], |r| {
        Ok(DeployHook {
            id: r.get(0)?,
            created_at: r.get(1)?,
            last_used_at: r.get(2)?,
            secret: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn create(c: &Connection, app_id: &str) -> Result<DeployHook> {
    let n: i64 = c.query_row("SELECT count(*) FROM deploy_hooks WHERE app_id = ?1", [app_id], |r| {
        r.get(0)
    })?;
    if n >= MAX_PER_APP {
        return Err(Error::Invalid(format!("at most {MAX_PER_APP} deploy hooks per app")));
    }
    let secret = format!("{PREFIX}{}", random_token(43));
    let id = new_id("hook");
    let t = now();
    c.execute(
        "INSERT INTO deploy_hooks (id, app_id, secret_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, app_id, hash(&secret), t],
    )?;
    Ok(DeployHook {
        id,
        created_at: t,
        last_used_at: None,
        secret: Some(secret),
    })
}

pub fn delete(c: &Connection, app_id: &str, id: &str) -> Result<()> {
    if c.execute(
        "DELETE FROM deploy_hooks WHERE app_id = ?1 AND id = ?2",
        params![app_id, id],
    )? == 0
    {
        return Err(Error::not_found(format!("deploy hook {id}")));
    }
    Ok(())
}

/// The (hook id, app id) a secret belongs to; marks it used.
pub fn resolve(c: &Connection, secret: &str) -> Result<Option<(String, String)>> {
    if !secret.starts_with(PREFIX) || secret.len() > 128 {
        return Ok(None);
    }
    let found: Option<(String, String)> = c
        .query_row(
            "SELECT id, app_id FROM deploy_hooks WHERE secret_hash = ?1",
            [hash(secret)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((id, _)) = &found {
        c.execute(
            "UPDATE deploy_hooks SET last_used_at = ?2 WHERE id = ?1",
            params![id, now()],
        )?;
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{apps, Store};

    #[test]
    fn hooks() {
        let s = Store::open_in_memory().unwrap();
        let a = s.tx(|c| apps::create(c, &apps::tests::git_app("blog"))).unwrap();
        let h = s.tx(|c| create(c, &a.id)).unwrap();
        let secret = h.secret.clone().unwrap();
        assert!(secret.starts_with(PREFIX) && secret.len() == PREFIX.len() + 43);
        let listed = s.with(|c| list(c, &a.id)).unwrap();
        assert_eq!(listed[0].secret, None, "never shown again");
        let raw: String = s
            .with(|c| Ok(c.query_row("SELECT secret_hash FROM deploy_hooks", [], |r| r.get(0))?))
            .unwrap();
        assert_ne!(raw, secret);
        assert_eq!(
            s.with(|c| resolve(c, &secret)).unwrap(),
            Some((h.id.clone(), a.id.clone()))
        );
        assert!(s.with(|c| list(c, &a.id)).unwrap()[0].last_used_at.is_some());
        assert_eq!(s.with(|c| resolve(c, "vfyh_wrong")).unwrap(), None);
        assert_eq!(s.with(|c| resolve(c, "nope")).unwrap(), None);
        s.tx(|c| delete(c, &a.id, &h.id)).unwrap();
        assert_eq!(s.with(|c| resolve(c, &secret)).unwrap(), None);
    }
}
