//! Databases against a real Docker daemon and a real restic repository:
//! create, health, backup, restore, tier moves, public ports, removal.
//! Opt in with `VERCELIFY_DOCKER_TESTS=1`.

use std::sync::Arc;
use std::time::Duration;
use vercelify_core::platform::*;
use vercelify_core::StorageTier;
use vercelify_platform::build::{Builder, Driver};
use vercelify_platform::databases;
use vercelify_platform::dbengine::Restic;
use vercelify_platform::deploy::Deployer;
use vercelify_platform::docker::Docker;
use vercelify_platform::engine::Engine;
use vercelify_platform::gate::BuildGate;
use vercelify_platform::logs::LogHub;
use vercelify_platform::proxy::{ProxyConfig, TRAEFIK_IMAGE};
use vercelify_platform::secrets::MasterKey;
use vercelify_platform::Store;

fn sh(cmd: &str) -> String {
    let o = std::process::Command::new("sh").arg("-c").arg(cmd).output().unwrap();
    assert!(o.status.success(), "{cmd}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// Pull from Google's Docker Hub mirror (no rate limit), tag as Docker Hub.
fn image(name: &str) {
    if sh(&format!(
        "docker image inspect {name} >/dev/null 2>&1 && echo yes || true"
    )) == "yes"
    {
        return;
    }
    let mirror = if name.contains('/') {
        format!("mirror.gcr.io/{name}")
    } else {
        format!("mirror.gcr.io/library/{name}")
    };
    sh(&format!(
        "docker pull -q {mirror} >/dev/null && docker tag {mirror} {name}"
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs Docker: VERCELIFY_DOCKER_TESTS=1 cargo test … -- --include-ignored"]
async fn databases_end_to_end() {
    if std::env::var("VERCELIFY_DOCKER_TESTS").as_deref() != Ok("1") {
        return;
    }
    // It runs an engine on this machine's Docker: never on a real server.
    assert!(
        !std::path::Path::new("/etc/vercelify").exists()
            || std::env::var("VERCELIFY_TESTS_ON_SERVER").as_deref() == Ok("1"),
        "this machine is a Vercelify server: this test would change its containers (VERCELIFY_TESTS_ON_SERVER=1 runs it anyway)"
    );
    image("postgres:17-alpine");
    image("valkey/valkey:8-alpine");
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("platform.db")).unwrap());
    store
        .tx(|c| vercelify_platform::nodes::ensure_main(c, "test", "http://127.0.0.1:7420", std::env::consts::ARCH))
        .unwrap();
    let key = Arc::new(MasterKey::from_bytes(&[6u8; 32]).unwrap());
    let docker = Docker::default();
    let engine = Engine::new(
        store.clone(),
        key.clone(),
        Builder {
            docker: "docker".into(),
            git: "git".into(),
            work_dir: dir.path().join("work"),
            driver: Driver::Docker,
            memory: None,
            timeout: Duration::from_secs(60),
        },
        Deployer {
            docker: docker.clone(),
            proxy: ProxyConfig {
                dir: dir.path().join("proxy"),
                http_port: 18080,
                https_port: 18443,
                metrics_port: 18082,
                acme_email: None,
                dns_challenge: false,
                image: TRAEFIK_IMAGE.into(),
            },
            hdd_root: dir.path().join("hdd"),
            cgroup_parent: None,
            health_interval: Duration::from_millis(300),
        },
        BuildGate::new(1, dir.path().join("pg.state")),
        LogHub::new(dir.path().join("logs")),
    );
    let repo = dir.path().join("restic");
    let restic = Restic {
        bin: "restic".into(),
        env: [
            ("RESTIC_PASSWORD".to_string(), "test-password".to_string()),
            ("BACKUP_REPOS".to_string(), repo.to_string_lossy().into_owned()),
        ]
        .into(),
    };
    sh(&format!(
        "RESTIC_PASSWORD=test-password restic init -q -r {}",
        repo.display()
    ));
    let docker_root = std::path::PathBuf::from(sh("docker info -f '{{.DockerRootDir}}'")).join("volumes");

    let create = |name: &str, kind: DatabaseKind| {
        let req = DatabaseCreate {
            name: name.into(),
            kind,
            version: None,
            environment_id: None,
            project_id: None,
            public_port: None,
            tier: None,
            link_app: None,
        };
        store.tx(|c| databases::create(c, &key, &req)).unwrap()
    };
    for n in ["vfy-db-pgtest", "vfy-db-cachetest"] {
        let _ = docker.remove(n).await;
    }
    let _ = docker.volume_remove("vfy-db-pgtest-data").await;
    let _ = docker.volume_remove("vfy-db-cachetest-data").await;

    // PostgreSQL.
    let pg = create("pgtest", DatabaseKind::Postgresql);
    // Emulate an existing pre-v5 database: its data must keep the old name.
    store
        .tx(|c| {
            c.execute("UPDATE volumes SET storage_name = NULL WHERE owner_id = ?1", [&pg.id])?;
            Ok(())
        })
        .unwrap();
    let (first, second) = tokio::join!(engine.ensure_database(&pg.id), engine.ensure_database(&pg.id));
    first.expect("postgres starts");
    second.expect("concurrent ensure is idempotent");
    let sql_file = dir.path().join("q.sql");
    let psql = |sql: &str| {
        std::fs::write(&sql_file, sql).unwrap();
        sh(&format!(
            "docker exec -i vfy-db-pgtest sh -c 'psql -q -tA -v ON_ERROR_STOP=1 -U \"$POSTGRES_USER\" -d \"$POSTGRES_DB\"' < {}",
            sql_file.display()
        ))
    };
    psql("CREATE TABLE notes (t text); INSERT INTO notes VALUES ('kept');");
    let b = engine.backup_database(&pg.id, &restic).await.expect("backup");
    assert_eq!(b.status, "succeeded");
    assert!(!b.snapshot.is_empty() && b.bytes > 0, "{b:?}");
    psql("DROP TABLE notes;");
    engine
        .restore_database(&pg.id, &b.snapshot, &restic)
        .await
        .expect("restore");
    assert_eq!(psql("SELECT t FROM notes;"), "kept");
    assert_eq!(
        store
            .with(|c| vercelify_platform::dbengine::backups(c, &pg.id))
            .unwrap()
            .len(),
        2,
        "restore saves current data before replacement"
    );

    // To the HDD and back, with the data.
    engine
        .move_database(&pg.id, StorageTier::Hdd, &docker_root)
        .await
        .expect("to hdd");
    assert!(dir.path().join("hdd/vfy-db-pgtest-data/PG_VERSION").exists());
    assert_eq!(psql("SELECT t FROM notes;"), "kept");
    engine
        .move_database(&pg.id, StorageTier::Ssd, &docker_root)
        .await
        .expect("to ssd");
    assert_eq!(psql("SELECT t FROM notes;"), "kept");

    // A public port recreates the container on the same data.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    store
        .tx(|c| {
            databases::patch(
                c,
                &key,
                &pg.id,
                &DatabasePatch {
                    public_port: Some(port),
                    ..DatabasePatch::default()
                },
            )
        })
        .unwrap();
    assert!(engine.reconcile_databases().await.is_empty());
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok(),
        "public port open"
    );
    assert_eq!(psql("SELECT t FROM notes;"), "kept");
    assert!(
        engine.reconcile_databases().await.is_empty(),
        "nothing changes on a second pass"
    );

    // Valkey: snapshot backups.
    let cache = create("cachetest", DatabaseKind::Redis);
    engine.ensure_database(&cache.id).await.expect("valkey starts");
    let cli = |args: &str| {
        sh(&format!(
            "docker exec vfy-db-cachetest sh -c 'REDISCLI_AUTH=\"$VFY_PASSWORD\" valkey-cli {args}'"
        ))
    };
    assert_eq!(cli("SET greeting hello"), "OK");
    let b = engine.backup_database(&cache.id, &restic).await.expect("valkey backup");
    assert_eq!(cli("FLUSHALL"), "OK");
    engine
        .restore_database(&cache.id, &b.snapshot, &restic)
        .await
        .expect("valkey restore");
    assert_eq!(cli("GET greeting"), "hello");
    let unauth = sh("docker exec vfy-db-cachetest valkey-cli GET greeting 2>&1 || true");
    assert!(unauth.contains("NOAUTH"), "a password is required: {unauth}");

    // A missing snapshot must fail before the live data is touched.
    assert!(engine.restore_database(&cache.id, "00000000", &restic).await.is_err());
    assert_eq!(cli("GET greeting"), "hello", "failed restore must preserve live data");

    // Removal (with the data).
    for d in [&pg, &cache] {
        let vol = store
            .with(|c| vercelify_platform::volumes::list(c, vercelify_core::platform::VolumeOwner::Database, &d.id))
            .unwrap()
            .remove(0);
        let name = vercelify_platform::volumes::physical_name(&vol, &format!("db-{}", d.name), None);
        let d = store.tx(|c| databases::delete(c, &key, &d.id)).unwrap();
        engine.remove_database(&d, true, None, &name).await.unwrap();
        assert!(!docker_root.join(&name).exists(), "purge removes the actual volume");
        assert!(docker.inspect(&d.container).await.unwrap().is_none());
    }
}
