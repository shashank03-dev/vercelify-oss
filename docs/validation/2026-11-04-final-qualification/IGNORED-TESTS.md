# Ignored Rust runtime coverage

All fourteen have a successful runtime execution. Historical logs remain separate.

| Test | Source | Prior successful logs | Fresh evidence |
| --- | --- | --- | --- |
| `into_a_container` | `agent/src/terminal.rs` | 0 | [passed](terminal.txt) |
| `isolated_restore_drills_verify_selected_snapshots_and_fail_safely` | `crates/vercelify-platform/tests/restore_drill_docker.rs` | 4 | [passed](restore-drills.txt) |
| `previews_have_no_production_credentials_network_or_public_bypass` | `crates/vercelify-platform/tests/preview_safety.rs` | 6 | [passed](preview-safety.txt) |
| `deploy_redeploy_rollback_recover` | `crates/vercelify-platform/tests/engine_docker.rs` | 5 | [passed](engine.txt) |
| `running_http_failure_crash_loop_and_failed_recovery_are_durable` | `crates/vercelify-platform/tests/deploy_guard_docker.rs` | 1 | [passed](deploy-guard.txt) |
| `databases_end_to_end` | `crates/vercelify-platform/tests/databases_docker.rs` | 2 | [passed](databases.txt) |
| `isolated_postgres_roles_migrations_and_owned_recovery` | `crates/vercelify-platform/tests/preview_db_docker.rs` | 4 | [passed](postgres-previews.txt) |
| `idle_builder_retains_cache_and_restarts_for_repeated_builds` | `crates/vercelify-platform/tests/builder_idle.rs` | 3 | [passed](builder-idle.txt) |
| `container_lifecycle` | `crates/vercelify-platform/src/docker.rs` | 0 | [passed](docker.txt) |
| `push_list_delete` | `crates/vercelify-platform/src/registry.rs` | 0 | [passed](registry.txt) |
| `compose_resolves_preview_volumes` | `crates/vercelify-platform/src/compose.rs` | 0 | [passed](compose-config.txt) |
| `compose_up_and_down` | `crates/vercelify-platform/src/compose.rs` | 0 | [passed](compose-runtime.txt) |
| `builds_fixture_apps_with_docker` | `crates/vercelify-platform/src/build.rs` | 0 | [passed](build-fixtures.txt) |
| `a_virtual_display_appears_and_goes` | `cockpit/src-tauri/src/vdisplay.rs` | 0 | [passed](mac-virtual-display.txt) |
