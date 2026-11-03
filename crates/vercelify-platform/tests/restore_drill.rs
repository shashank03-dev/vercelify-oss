use vercelify_platform::restore_drill::{validate_assertions, Assertion, Manifest};
fn manifest() -> Manifest {
    Manifest {
        version: 1,
        database_id: "db_source".into(),
        database_name: "source".into(),
        server_major: 17,
        filename: "source.dump".into(),
        format: "postgresql-custom".into(),
        backup_started_at: "2026-11-03T00:00:00Z".into(),
        bytes: 8192,
        database_bytes: 16 * 1024 * 1024,
        extensions: vec![("plpgsql".into(), "1.0".into())],
        objects: vec![],
    }
}
#[test]
fn an_isolated_drill_requires_an_exact_supported_manifest() {
    assert!(manifest().validate("db_source").is_ok());
    assert!(manifest().validate("db_other").is_err());
    for variant in 0..7 {
        let mut m = manifest();
        match variant {
            0 => m.version = 2,
            1 => m.server_major = 16,
            2 => m.filename = "../source.dump".into(),
            3 => m.bytes = 0,
            4 => m.format = "tar".into(),
            5 => m.backup_started_at = "unknown".into(),
            _ => m.extensions.push(("bad/extension".into(), "1".into())),
        }
        assert!(
            m.validate("db_source").is_err(),
            "unsupported manifest variant {variant}"
        );
    }
}
#[test]
fn assertion_inputs_are_bounded_and_do_not_accept_obvious_write_commands() {
    let good = Assertion {
        name: "migration-marker".into(),
        query: "SELECT version FROM migrations ORDER BY version DESC LIMIT 1".into(),
        expected: "20261103".into(),
    };
    assert!(validate_assertions(std::slice::from_ref(&good)).is_ok());
    assert!(validate_assertions(&vec![good.clone(); 21]).is_err());
    let mut bad = good;
    bad.query = "DELETE FROM migrations".into();
    assert!(validate_assertions(&[bad]).is_err());
}

#[test]
fn journal_requires_matching_backup_and_keeps_assertion_values_sealed() {
    use vercelify_core::platform::DatabaseCreate;
    use vercelify_platform::{databases, restore_drill as drill, secrets::MasterKey, Store};
    let s = Store::open_in_memory().unwrap();
    let key = MasterKey::from_bytes(&[3; 32]).unwrap();
    let request: DatabaseCreate =
        serde_json::from_value(serde_json::json!({"name":"source","kind":"postgresql"})).unwrap();
    let db = s.tx(|c| databases::create(c, &key, &request)).unwrap();
    let mut m = manifest();
    m.database_id = db.id.clone();
    s.tx(|c|{c.execute("INSERT INTO database_backups(id,database_id,status,bytes,snapshot,created_at) VALUES('bak_drill',?1,'succeeded',8192,?2,?3)",rusqlite::params![db.id,"a".repeat(64),m.backup_started_at])?;Ok(())}).unwrap();
    assert!(
        s.tx(|c| drill::enqueue(c, &key, &db.id, "bak_drill", &[])).is_err(),
        "legacy backups are not implicitly verified"
    );
    s.tx(|c| drill::attach_manifest(c, "bak_drill", &m)).unwrap();
    let assertions = vec![Assertion {
        name: "marker".into(),
        query: "SELECT 'private-known-marker'".into(),
        expected: "private-known-marker".into(),
    }];
    let job = s
        .tx(|c| drill::enqueue(c, &key, &db.id, "bak_drill", &assertions))
        .unwrap();
    assert_eq!(job.state, "queued");
    assert!(!serde_json::to_string(&job).unwrap().contains("private-known-marker"));
    let sealed: Vec<u8> = s
        .with(|c| {
            Ok(
                c.query_row("SELECT assertions FROM restore_drills WHERE id=?1", [&job.id], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap();
    assert!(!sealed.windows(20).any(|w| w == b"private-known-marker"));
    assert!(s
        .tx(|c| drill::enqueue(c, &key, &db.id, "bak_drill", &assertions))
        .is_err());
    assert!(s.tx(|c| drill::set_state(c, &job.id, "succeeded", "")).is_err());
}

#[test]
fn quarantine_is_journaled_hidden_and_cannot_be_exposed_or_repurposed() {
    use vercelify_core::platform::*;
    use vercelify_platform::{databases, restore_drill as drill, secrets::MasterKey, Store};
    let s = Store::open_in_memory().unwrap();
    let key = MasterKey::from_bytes(&[5; 32]).unwrap();
    let source = s
        .tx(|c| {
            databases::create(
                c,
                &key,
                &serde_json::from_value(serde_json::json!({"name":"source","kind":"postgresql"})).unwrap(),
            )
        })
        .unwrap();
    let mut m = manifest();
    m.database_id = source.id.clone();
    s.tx(|c|{c.execute("INSERT INTO database_backups(id,database_id,status,bytes,snapshot,created_at) VALUES('bak_service',?1,'succeeded',8192,?2,?3)",rusqlite::params![source.id,"b".repeat(64),m.backup_started_at])?;drill::attach_manifest(c,"bak_service",&m)}).unwrap();
    let job = s
        .tx(|c| drill::enqueue(c, &key, &source.id, "bak_service", &[]))
        .unwrap();
    let service = s.tx(|c| drill::prepare(c, &key, &job.id)).unwrap();
    assert_ne!(service.database_id, source.id);
    assert_eq!(
        s.tx(|c| drill::prepare(c, &key, &job.id)).unwrap().database_id,
        service.database_id
    );
    assert!(s
        .with(|c| databases::list(c, &key, None, false))
        .unwrap()
        .iter()
        .all(|d| d.id != service.database_id));
    assert!(s
        .tx(|c| databases::patch(
            c,
            &key,
            &service.database_id,
            &DatabasePatch {
                public_port: Some(15432),
                ..Default::default()
            }
        ))
        .is_err());
    assert!(s.tx(|c| databases::delete(c, &key, &service.database_id)).is_err());
    let destination = s
        .with(|c| databases::get(c, &key, &service.database_id, false))
        .unwrap();
    assert!(s
        .with(|c| databases::resolve_references(c, &key, &format!("${{{{db.{}.url}}}}", destination.name)))
        .is_err());
    s.tx(|c| drill::set_state(c, &job.id, "running", "")).unwrap();
    s.tx(|c| drill::begin_cleanup(c, &job.id, "fixture failure")).unwrap();
    assert!(s
        .tx(|c| drill::set_state(c, &job.id, "failed", "fixture failure"))
        .is_err());
    s.tx(|c| drill::finish_cleanup(c, &job.id)).unwrap();
    s.tx(|c| drill::set_state(c, &job.id, "failed", "fixture failure"))
        .unwrap();
    assert!(s.tx(|c| drill::set_state(c, &job.id, "running", "")).is_err());
    assert_eq!(s.with(|c| databases::list(c, &key, None, false)).unwrap().len(), 1);
}

#[test]
fn assertions_cannot_stack_statements_or_invoke_psql_metacommands() {
    for query in [
        "SELECT 1; COMMIT; DELETE FROM entries",
        "SELECT 1\n\\! touch /tmp/not-a-read-probe",
        "SELECT 'command'\n\\gexec",
    ] {
        assert!(validate_assertions(&[Assertion {
            name: "probe".into(),
            query: query.into(),
            expected: "1".into()
        }])
        .is_err());
    }
}

#[test]
fn schedules_catch_up_once_and_keep_missing_backups_overdue() {
    use vercelify_platform::{databases, restore_drill as d, secrets::MasterKey, Store};
    let s = Store::open_in_memory().unwrap();
    let key = MasterKey::from_bytes(&[42; 32]).unwrap();
    let db = s
        .tx(|c| {
            databases::create(
                c,
                &key,
                &serde_json::from_value(serde_json::json!({"name":"source","kind":"postgresql"})).unwrap(),
            )
        })
        .unwrap();
    let spec = d::ScheduleUpdate {
        enabled: true,
        schedule: "0 3 1 * *".into(),
        assertions: vec![Assertion {
            name: "marker".into(),
            query: "SELECT 'private-marker'".into(),
            expected: "private-marker".into(),
        }],
    };
    s.tx(|c| d::set_schedule(c, &key, &db.id, &spec)).unwrap();
    let due = "2026-11-01T03:00:00+00:00";
    assert!(s.tx(|c| d::enqueue_scheduled(c, &key, &db.id, due)).unwrap().is_none());
    let missed = s.with(|c| d::schedule(c, &key, &db.id)).unwrap();
    assert!(missed.error.contains("overdue") && missed.last_due.is_none());
    let sealed: Vec<u8> = s
        .with(|c| Ok(c.query_row("SELECT assertions FROM restore_drill_schedules", [], |r| r.get(0))?))
        .unwrap();
    assert!(!sealed.windows(14).any(|v| v == b"private-marker"));
    let mut m = manifest();
    m.database_id = db.id.clone();
    s.tx(|c| {c.execute("INSERT INTO database_backups(id,database_id,status,bytes,snapshot,created_at) VALUES('bak_schedule',?1,'succeeded',8192,?2,?3)",rusqlite::params![db.id,"b".repeat(64),m.backup_started_at])?;d::attach_manifest(c,"bak_schedule",&m)}).unwrap();
    let job = s.tx(|c| d::enqueue_scheduled(c, &key, &db.id, due)).unwrap().unwrap();
    assert!(s.tx(|c| d::enqueue_scheduled(c, &key, &db.id, due)).unwrap().is_none());
    s.tx(|c| d::set_state(c, &job.id, "failed", "fixture")).unwrap();
    assert!(s.tx(|c| d::enqueue_scheduled(c, &key, &db.id, due)).unwrap().is_none());
    let schedule = s.with(|c| d::schedule(c, &key, &db.id)).unwrap();
    assert_eq!(schedule.last_due.as_deref(), Some(due));
    assert_eq!(schedule.last_job.as_deref(), Some(job.id.as_str()));
    assert!(schedule.error.is_empty());
}
#[test]
fn provided_recovery_is_sealed_and_rejects_executable_or_file_credentials() {
    use std::collections::BTreeMap;
    use vercelify_platform::{restore_drill as d, secrets::MasterKey, Store};
    let s = Store::open_in_memory().unwrap();
    let key = MasterKey::from_bytes(&[43; 32]).unwrap();
    let good = d::Recovery {
        repository: "s3:https://example.invalid/bucket".into(),
        password: "private-recovery-secret".into(),
        access: BTreeMap::new(),
    };
    let job = s
        .tx(|c| d::enqueue_imported(c, &key, &manifest(), &"c".repeat(64), &[], &good))
        .unwrap();
    assert!(!format!("{good:?}").contains(&good.password));
    assert!(!serde_json::to_string(&job).unwrap().contains(&good.password));
    assert_eq!(
        s.with(|c| d::recovery(c, &key, &job.id)).unwrap().unwrap().password,
        good.password
    );
    let sealed: Vec<u8> = s
        .with(|c| Ok(c.query_row("SELECT recovery FROM restore_drills", [], |r| r.get(0))?))
        .unwrap();
    assert!(!sealed
        .windows(good.password.len())
        .any(|v| v == good.password.as_bytes()));
    for forbidden in [
        "RESTIC_PASSWORD_COMMAND",
        "RESTIC_PASSWORD_FILE",
        "AWS_SHARED_CREDENTIALS_FILE",
    ] {
        let mut bad = good.clone();
        bad.access.insert(forbidden.into(), "/private/credentials".into());
        assert!(bad.validate().is_err());
    }
    s.tx(|c| d::set_state(c, &job.id, "failed", "missing repo")).unwrap();
    assert!(s.with(|c| d::recovery(c, &key, &job.id)).unwrap().is_none());
}

#[test]
fn source_deletion_preserves_exact_cleanup_and_immutable_restore_identity() {
    use vercelify_platform::{databases, restore_drill as d, secrets::MasterKey, Store};
    let s = Store::open_in_memory().unwrap();
    let key = MasterKey::from_bytes(&[55; 32]).unwrap();
    let source = s
        .tx(|c| {
            databases::create(
                c,
                &key,
                &serde_json::from_value(serde_json::json!({"name":"source","kind":"postgresql"})).unwrap(),
            )
        })
        .unwrap();
    let mut m = manifest();
    m.database_id = source.id.clone();
    s.tx(|c|{c.execute("INSERT INTO database_backups(id,database_id,status,bytes,snapshot,created_at) VALUES('bak_deleted',?1,'succeeded',8192,?2,?3)",rusqlite::params![source.id,"a".repeat(64),m.backup_started_at])?;d::attach_manifest(c,"bak_deleted",&m)}).unwrap();
    let job = s.tx(|c| d::enqueue(c, &key, &source.id, "bak_deleted", &[])).unwrap();
    let destination = s.tx(|c| d::prepare(c, &key, &job.id)).unwrap();
    s.tx(|c| databases::delete(c, &key, &source.id)).unwrap();
    assert!(s.with(|c| d::get(c, &job.id)).unwrap().database_id.is_none());
    assert_eq!(s.with(|c| d::job_manifest(c, &job.id)).unwrap().database_id, source.id);
    assert_eq!(
        s.with(|c| d::service(c, &job.id)).unwrap().unwrap().database_id,
        destination.database_id
    );
    s.tx(|c| d::begin_cleanup(c, &job.id, "source deleted")).unwrap();
    s.tx(|c| d::finish_cleanup(c, &job.id)).unwrap();
    s.tx(|c| d::set_state(c, &job.id, "failed", "source deleted")).unwrap();
    assert!(s.with(|c| databases::list(c, &key, None, false)).unwrap().is_empty());
}
#[test]
fn independently_imported_jobs_share_a_bounded_global_queue() {
    use vercelify_platform::{restore_drill as d, secrets::MasterKey, Store};
    let s = Store::open_in_memory().unwrap();
    let key = MasterKey::from_bytes(&[56; 32]).unwrap();
    let recovery = d::Recovery {
        repository: "/fixture/repository".into(),
        password: "fixture-only".into(),
        access: Default::default(),
    };
    let mut first = String::new();
    for index in 0..20 {
        let mut m = manifest();
        m.database_id = format!("db_source{index}");
        let job = s
            .tx(|c| d::enqueue_imported(c, &key, &m, &"a".repeat(64), &[], &recovery))
            .unwrap();
        if index == 0 {
            first = job.id;
        }
    }
    let mut last = manifest();
    last.database_id = "db_source20".into();
    assert!(s
        .tx(|c| d::enqueue_imported(c, &key, &last, &"a".repeat(64), &[], &recovery))
        .is_err());
    s.tx(|c| d::set_state(c, &first, "failed", "fixture")).unwrap();
    assert!(s
        .tx(|c| d::enqueue_imported(c, &key, &last, &"a".repeat(64), &[], &recovery))
        .is_ok());
}
