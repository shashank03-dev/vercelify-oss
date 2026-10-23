//! Storage-tier placement for Coolify resources.
//!
//! Coolify's API cannot change where a persistent volume lives, but its
//! database does: a `local_persistent_volumes` row with `host_path` set is
//! rendered as a bind mount on the next start. We read and update that row
//! through `docker exec coolify-db psql`, after copying the data ourselves.
//! Verified against Coolify 4.4.

use crate::config::Config;
use crate::error::AppError;
use crate::exec::run;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use vercelify_core::{CoolifyVolume, MoveVolumeResult, StorageTier};

const SQL_TIMEOUT: Duration = Duration::from_secs(20);

/// Coolify model class -> (table, movable)
fn table_for(model: &str) -> Option<(&'static str, bool)> {
    Some(match model.rsplit('\\').next()? {
        "Application" => ("applications", true),
        "StandalonePostgresql" => ("standalone_postgresqls", true),
        "StandaloneMysql" => ("standalone_mysqls", true),
        "StandaloneMariadb" => ("standalone_mariadbs", true),
        "StandaloneMongodb" => ("standalone_mongodbs", true),
        "StandaloneRedis" => ("standalone_redis", true),
        "StandaloneKeydb" => ("standalone_keydbs", true),
        "StandaloneDragonfly" => ("standalone_dragonflies", true),
        "StandaloneClickhouse" => ("standalone_clickhouses", true),
        "ServiceApplication" => ("service_applications", false),
        "ServiceDatabase" => ("service_databases", false),
        _ => return None,
    })
}

/// SQL string literal (standard_conforming_strings is on in PostgreSQL >= 9.1).
fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

async fn psql(cfg: &Config, sql: &str) -> Result<Vec<Vec<String>>, AppError> {
    let out = run(
        &cfg.docker_bin,
        &[
            "exec",
            &cfg.coolify_db_container,
            "psql",
            "-U",
            "coolify",
            "-d",
            "coolify",
            "-v",
            "ON_ERROR_STOP=1",
            "-At",
            "-F",
            "\t",
            "-c",
            sql,
        ],
        SQL_TIMEOUT,
    )
    .await?;
    if !out.success() {
        let msg = out.stderr.trim();
        return Err(if msg.contains("No such container") || msg.contains("is not running") {
            AppError::unavailable("Coolify database container is not running")
        } else {
            AppError::internal(format!("coolify-db query failed: {msg}"))
        });
    }
    Ok(out
        .stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.split('\t').map(String::from).collect())
        .collect())
}

/// Run a read-only query against Coolify's database (tab-separated rows).
pub async fn query(cfg: &Config, sql: &str) -> Result<Vec<Vec<String>>, AppError> {
    psql(cfg, sql).await
}

/// Mount names and sources of all running containers.
async fn mounts_in_use(cfg: &Config) -> Result<Vec<String>, AppError> {
    let ids = run(&cfg.docker_bin, &["ps", "-q"], SQL_TIMEOUT).await?;
    let ids: Vec<&str> = ids.stdout.split_whitespace().collect();
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let mut args = vec![
        "inspect",
        "--format",
        "{{range .Mounts}}{{.Name}}\t{{.Source}}\n{{end}}",
    ];
    args.extend(ids);
    let out = run(&cfg.docker_bin, &args, SQL_TIMEOUT).await?;
    Ok(out
        .stdout
        .lines()
        .flat_map(|l| l.split('\t').map(str::to_string).collect::<Vec<_>>())
        .filter(|s| !s.is_empty())
        .collect())
}

fn hdd_path(cfg: &Config, volume: &str) -> PathBuf {
    cfg.hdd_volume_root.join("coolify").join(volume)
}

fn ssd_path(cfg: &Config, volume: &str) -> PathBuf {
    cfg.docker_volumes_root.join(volume).join("_data")
}

fn tier_of(cfg: &Config, host_path: Option<&str>) -> StorageTier {
    match host_path {
        Some(p) if Path::new(p).starts_with(&cfg.storage_mount) || Path::new(p).starts_with(&cfg.hdd_volume_root) => {
            StorageTier::Hdd
        }
        // Any other bind mount (e.g. /data/...) is on the system disk.
        _ => StorageTier::Ssd,
    }
}

/// Docker volume names Coolify generates: letters, digits, `-`, `_`, `.`.
fn valid_volume_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 255
        && n.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub async fn list(cfg: &Config) -> Result<Vec<CoolifyVolume>, AppError> {
    let rows = psql(
        cfg,
        "select id, name, mount_path, coalesce(host_path, ''), resource_type, resource_id \
         from local_persistent_volumes order by id",
    )
    .await?;

    // Resolve resource names per table with one query each.
    let mut by_table: HashMap<&'static str, Vec<u64>> = HashMap::new();
    for r in &rows {
        let (Some(kind), Some(Ok(id))) = (r.get(4), r.get(5).map(|s| s.parse::<u64>())) else {
            continue;
        };
        if let Some((table, _)) = table_for(kind) {
            by_table.entry(table).or_default().push(id);
        }
    }
    let mut names: HashMap<(&'static str, u64), (String, String)> = HashMap::new();
    for (table, ids) in by_table {
        let list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        for r in psql(cfg, &format!("select id, name, uuid from {table} where id in ({list})")).await? {
            if let (Some(Ok(id)), Some(name), Some(uuid)) = (r.first().map(|s| s.parse()), r.get(1), r.get(2)) {
                names.insert((table, id), (name.clone(), uuid.clone()));
            }
        }
    }

    let in_use = mounts_in_use(cfg).await.unwrap_or_default();
    let mut out = Vec::new();
    for r in rows {
        if r.len() < 6 {
            continue;
        }
        let Ok(id) = r[0].parse::<u64>() else { continue };
        let host_path = Some(r[3].clone()).filter(|s| !s.is_empty());
        let resource_type = r[4].rsplit('\\').next().unwrap_or(&r[4]).to_string();
        let (table, movable) = table_for(&r[4]).unwrap_or(("", false));
        let (resource_name, resource_uuid) = r[5]
            .parse::<u64>()
            .ok()
            .and_then(|rid| names.get(&(table, rid)).cloned())
            .unwrap_or_default();
        let data_path = host_path
            .clone()
            .unwrap_or_else(|| ssd_path(cfg, &r[1]).to_string_lossy().into_owned());
        out.push(CoolifyVolume {
            id,
            tier: tier_of(cfg, host_path.as_deref()),
            in_use: in_use.iter().any(|m| *m == r[1] || *m == data_path),
            movable: movable && valid_volume_name(&r[1]),
            name: r[1].clone(),
            mount_path: r[2].clone(),
            host_path,
            data_path,
            resource_type,
            resource_name,
            resource_uuid,
        });
    }
    Ok(out)
}

async fn copy_tree(src: &Path, dst: &Path) -> Result<bool, AppError> {
    std::fs::create_dir_all(dst).map_err(|e| AppError::internal(format!("creating {}: {e}", dst.display())))?;
    if !src.is_dir() {
        return Ok(false);
    }
    let src_arg = format!("{}/", src.display());
    let dst_arg = format!("{}/", dst.display());
    let timeout = Duration::from_secs(6 * 3600);
    let out = match run(
        "rsync",
        &["-aHAX", "--numeric-ids", "--delete", &src_arg, &dst_arg],
        timeout,
    )
    .await
    {
        Err(crate::exec::ExecError::NotFound(_)) => {
            // Mirror like `rsync --delete`: stale files from an earlier copy
            // (e.g. old WAL segments) must not survive into the new location.
            clear_dir(dst)?;
            // `cp -a src/. dst/` preserves owners/modes when running as root.
            run("cp", &["-a", &format!("{}/.", src.display()), &dst_arg], timeout).await?
        }
        other => other?,
    };
    if !out.success() {
        return Err(AppError::internal(format!(
            "copying data failed: {}",
            out.stderr.trim()
        )));
    }
    Ok(true)
}

fn clear_dir(dir: &Path) -> Result<(), AppError> {
    let entries = std::fs::read_dir(dir).map_err(|e| AppError::internal(format!("reading {}: {e}", dir.display())))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let res = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        res.map_err(|e| AppError::internal(format!("clearing {}: {e}", path.display())))?;
    }
    Ok(())
}

pub async fn move_volume(cfg: &Config, id: u64, tier: StorageTier) -> Result<MoveVolumeResult, AppError> {
    let vol = list(cfg)
        .await?
        .into_iter()
        .find(|v| v.id == id)
        .ok_or_else(|| AppError::not_found(format!("Coolify volume {id} not found")))?;
    if !vol.movable {
        return Err(AppError::bad_request(format!(
            "{} volumes are defined by their compose file; edit the compose volumes instead",
            vol.resource_type
        )));
    }
    if vol.in_use {
        return Err(AppError::conflict(format!(
            "`{}` is running — stop it first (the cockpit does this for you)",
            if vol.resource_name.is_empty() {
                &vol.name
            } else {
                &vol.resource_name
            }
        )));
    }
    if vol.tier == tier {
        return Ok(MoveVolumeResult {
            previous_path: vol.data_path.clone(),
            volume: vol,
            copied: false,
            note: "already on that tier".into(),
        });
    }
    if tier == StorageTier::Hdd
        && cfg.require_storage_mount
        && !crate::volumes::is_mount_point(Path::new(&cfg.storage_mount))
    {
        return Err(AppError::conflict(format!("{} is not mounted", cfg.storage_mount)));
    }

    let src = PathBuf::from(&vol.data_path);
    let (dst, new_host_path) = match tier {
        StorageTier::Hdd => {
            let p = hdd_path(cfg, &vol.name);
            (p.clone(), Some(p.to_string_lossy().into_owned()))
        }
        StorageTier::Ssd => {
            crate::docker::volume_create(&cfg.docker_bin, &vol.name, &vol.resource_name.to_lowercase()).await?;
            (ssd_path(cfg, &vol.name), None)
        }
    };
    let copied = copy_tree(&src, &dst).await?;

    // A queued Coolify start may have fired while we were copying. Never
    // switch paths under a running container: its data would diverge.
    let in_use = mounts_in_use(cfg).await?;
    if in_use.iter().any(|m| *m == vol.name || *m == vol.data_path) {
        return Err(AppError::conflict(format!(
            "`{}` started while its data was being copied — nothing was changed; stop it and try again",
            vol.resource_name
        )));
    }

    let value = match &new_host_path {
        Some(p) => lit(p),
        None => "NULL".into(),
    };
    psql(
        cfg,
        &format!("update local_persistent_volumes set host_path = {value}, updated_at = now() where id = {id}"),
    )
    .await?;

    let volume = list(cfg)
        .await?
        .into_iter()
        .find(|v| v.id == id)
        .ok_or_else(|| AppError::internal("volume vanished after update"))?;
    Ok(MoveVolumeResult {
        note: format!(
            "Start `{}` to use the new location. The previous copy at {} was kept; delete it once you have verified the data.",
            volume.resource_name,
            src.display()
        ),
        previous_path: src.to_string_lossy().into_owned(),
        volume,
        copied,
    })
}

/// Coolify tables and columns Vercelify reads or writes. The installer keeps
/// the same list (`COOLIFY_REQUIRED_COLUMNS` in host/lib/coolify.sh).
pub const REQUIRED_COLUMNS: &[&str] = &[
    "servers.user",
    "servers.ip",
    "servers.private_key_id",
    "private_keys.uuid",
    "server_settings.wildcard_domain",
    "server_settings.server_id",
    "local_persistent_volumes.host_path",
    "local_persistent_volumes.name",
    "local_persistent_volumes.resource_type",
    "local_persistent_volumes.resource_id",
    "applications.uuid",
    "applications.fqdn",
    "applications.destination_id",
    // Read through the API by auto-deploy (skipped builds, vercel.json).
    "applications.watch_paths",
    "applications.base_directory",
    "standalone_dockers.server_id",
];

/// Required columns this Coolify database doesn't have (empty = compatible).
pub async fn schema_missing(cfg: &Config) -> Result<Vec<String>, AppError> {
    let rows = psql(
        cfg,
        "select table_name || '.' || column_name from information_schema.columns where table_schema = 'public'",
    )
    .await?;
    let have: std::collections::HashSet<String> = rows.into_iter().filter_map(|r| r.into_iter().next()).collect();
    Ok(REQUIRED_COLUMNS
        .iter()
        .filter(|c| !have.contains(**c))
        .map(|c| c.to_string())
        .collect())
}

/// Coolify's auto-update setting (None = unknown column / unreadable).
pub async fn auto_update_enabled(cfg: &Config) -> Option<bool> {
    let rows = psql(cfg, "select is_auto_update_enabled from instance_settings limit 1")
        .await
        .ok()?;
    match rows.first()?.first()?.as_str() {
        "t" => Some(true),
        "f" => Some(false),
        _ => None,
    }
}

/// Coolify's SSH key for the localhost server: authorising its public half
/// on a worker lets this Coolify deploy there.
pub async fn worker_key(cfg: &Config) -> Result<vercelify_core::CoolifyWorkerKey, AppError> {
    let rows = psql(
        cfg,
        "select pk.uuid from private_keys pk join servers s on s.private_key_id = pk.id where s.id = 0",
    )
    .await?;
    let uuid = rows
        .first()
        .and_then(|r| r.first())
        .filter(|u| !u.is_empty() && u.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .cloned()
        .ok_or_else(|| AppError::unavailable("Coolify has no SSH key for localhost yet"))?;
    let key_file = cfg.coolify_ssh_keys_dir.join(format!("ssh_key@{uuid}"));
    let out = run(
        &cfg.ssh_keygen_bin,
        &["-y", "-f", &key_file.to_string_lossy()],
        Duration::from_secs(10),
    )
    .await?;
    let public_key = out.stdout.trim().to_string();
    if !out.success() || !crate::ssh::valid_public_key(&public_key) {
        return Err(AppError::internal(format!(
            "could not read Coolify's public key from {}: {}",
            key_file.display(),
            out.stderr.trim()
        )));
    }
    Ok(vercelify_core::CoolifyWorkerKey {
        public_key: format!("{public_key} coolify"),
        private_key_uuid: uuid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_columns_match_the_installer() {
        let sh = include_str!("../../host/lib/coolify.sh");
        let start = sh.find("COOLIFY_REQUIRED_COLUMNS=\"").unwrap() + "COOLIFY_REQUIRED_COLUMNS=\"".len();
        let end = start + sh[start..].find('"').unwrap();
        let mut from_sh: Vec<&str> = sh[start..end].split_whitespace().collect();
        let mut here: Vec<&str> = REQUIRED_COLUMNS.to_vec();
        from_sh.sort();
        here.sort();
        assert_eq!(from_sh, here);
    }

    #[test]
    fn literal_escaping() {
        assert_eq!(
            lit("/mnt/storage/volumes/coolify/x"),
            "'/mnt/storage/volumes/coolify/x'"
        );
        assert_eq!(lit("a'b"), "'a''b'");
    }

    #[test]
    fn model_tables() {
        assert_eq!(
            table_for("App\\Models\\StandalonePostgresql"),
            Some(("standalone_postgresqls", true))
        );
        assert_eq!(
            table_for("App\\Models\\ServiceDatabase"),
            Some(("service_databases", false))
        );
        assert_eq!(table_for("App\\Models\\Nope"), None);
    }

    #[tokio::test]
    async fn copy_mirrors_without_stale_files() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("base")).unwrap();
        std::fs::write(src.path().join("base/1"), "new").unwrap();
        std::fs::write(dst.path().join("stale"), "old").unwrap();
        std::fs::create_dir_all(dst.path().join("olddir/x")).unwrap();
        assert!(copy_tree(src.path(), dst.path()).await.unwrap());
        assert_eq!(std::fs::read_to_string(dst.path().join("base/1")).unwrap(), "new");
        assert!(!dst.path().join("stale").exists());
        assert!(!dst.path().join("olddir").exists());
    }

    #[test]
    fn tiers() {
        let c = Config::default();
        assert_eq!(tier_of(&c, None), StorageTier::Ssd);
        assert_eq!(tier_of(&c, Some("/mnt/storage/volumes/coolify/x")), StorageTier::Hdd);
        assert_eq!(tier_of(&c, Some("/data/coolify/x")), StorageTier::Ssd);
        assert!(valid_volume_name("postgres-data-abc123"));
        assert!(!valid_volume_name("../x"));
    }
}
