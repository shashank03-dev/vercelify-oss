//! The service catalogue: self-hosted tools as ready-made compose apps,
//! replacing Coolify's one-click services (plan §4.13). Each template is a
//! compose file whose secrets (`${VAR}`) are generated when the app is
//! created and kept as the app's secret variables; `VERCELIFY_URL` is the
//! app's own address. Picked for machines with 8 GB of memory.

use crate::secrets::{random_token, MasterKey};
use crate::{apps, env, Error, Result};
use rusqlite::Connection;
use vercelify_core::platform::{
    App, AppCreate, AppKind, AppRuntime, AppSource, EnvVarInput, EnvVarsPut, HealthCheck, ServiceTemplate,
    TemplateCreate, VarScope,
};

pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    pub website: &'static str,
    pub min_memory_mb: u32,
    /// The compose service the app's domains route to, and its port.
    pub service: &'static str,
    pub port: u16,
    pub health_path: Option<&'static str>,
    /// Generated on creation: (variable, length).
    pub secrets: &'static [(&'static str, usize)],
    pub compose: &'static str,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "umami",
        name: "Umami",
        description: "Privacy-friendly website analytics, a light alternative to Google Analytics.",
        category: "analytics",
        website: "https://umami.is",
        min_memory_mb: 512,
        service: "umami",
        port: 3000,
        health_path: Some("/api/heartbeat"),
        secrets: &[("POSTGRES_PASSWORD", 32), ("APP_SECRET", 48)],
        compose: r#"services:
  umami:
    image: ghcr.io/umami-software/umami:postgresql-v2.15.1
    environment:
      DATABASE_URL: postgresql://umami:${POSTGRES_PASSWORD}@db:5432/umami
      APP_SECRET: ${APP_SECRET}
    depends_on:
      db:
        condition: service_healthy
    restart: unless-stopped
  db:
    image: postgres:17-alpine
    environment:
      POSTGRES_DB: umami
      POSTGRES_USER: umami
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}
    volumes:
      - db:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U umami -d umami"]
      interval: 5s
      retries: 20
    restart: unless-stopped
volumes:
  db:
"#,
    },
    Template {
        id: "plausible",
        name: "Plausible",
        description: "Simple, cookie-free analytics with a fast dashboard. Runs ClickHouse: needs about 2 GB of memory.",
        category: "analytics",
        website: "https://plausible.io",
        min_memory_mb: 2048,
        service: "plausible",
        port: 8000,
        health_path: Some("/api/health"),
        secrets: &[("POSTGRES_PASSWORD", 32), ("SECRET_KEY_BASE", 64), ("TOTP_VAULT_KEY_SEED", 32)],
        compose: r#"services:
  plausible:
    image: ghcr.io/plausible/community-edition:v2.1.5
    command: sh -c "/entrypoint.sh db createdb && /entrypoint.sh db migrate && /entrypoint.sh run"
    environment:
      BASE_URL: ${VERCELIFY_URL}
      SECRET_KEY_BASE: ${SECRET_KEY_BASE}
      TOTP_VAULT_KEY: ${TOTP_VAULT_KEY_SEED}
      DATABASE_URL: postgres://plausible:${POSTGRES_PASSWORD}@db:5432/plausible
      CLICKHOUSE_DATABASE_URL: http://events:8123/plausible
      DISABLE_REGISTRATION: invite_only
    depends_on:
      db:
        condition: service_healthy
      events:
        condition: service_healthy
    restart: unless-stopped
  db:
    image: postgres:17-alpine
    environment:
      POSTGRES_DB: plausible
      POSTGRES_USER: plausible
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}
    volumes:
      - db:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U plausible -d plausible"]
      interval: 5s
      retries: 20
    restart: unless-stopped
  events:
    image: clickhouse/clickhouse-server:24.8-alpine
    environment:
      CLICKHOUSE_SKIP_USER_SETUP: "1"
    volumes:
      - events:/var/lib/clickhouse
    ulimits:
      nofile:
        soft: 262144
        hard: 262144
    healthcheck:
      test: ["CMD-SHELL", "wget --no-verbose --tries=1 -O - http://127.0.0.1:8123/ping || exit 1"]
      interval: 5s
      retries: 30
    restart: unless-stopped
volumes:
  db:
  events:
"#,
    },
    Template {
        id: "minio",
        name: "MinIO",
        description: "S3-compatible object storage for uploads and backups. The domain opens its console; apps reach the S3 API at minio:9000.",
        category: "storage",
        website: "https://min.io",
        min_memory_mb: 512,
        service: "minio",
        port: 9001,
        health_path: None,
        secrets: &[("MINIO_ROOT_PASSWORD", 32)],
        compose: r#"services:
  minio:
    image: minio/minio:RELEASE.2025-04-22T22-12-26Z
    command: server /data --console-address :9001
    environment:
      MINIO_ROOT_USER: admin
      MINIO_ROOT_PASSWORD: ${MINIO_ROOT_PASSWORD}
      MINIO_BROWSER_REDIRECT_URL: ${VERCELIFY_URL}
    volumes:
      - data:/data
    restart: unless-stopped
volumes:
  data:
"#,
    },
    Template {
        id: "unleash",
        name: "Unleash",
        description: "Feature flags and gradual rollouts for your apps.",
        category: "flags",
        website: "https://www.getunleash.io",
        min_memory_mb: 512,
        service: "unleash",
        port: 4242,
        health_path: Some("/health"),
        secrets: &[("POSTGRES_PASSWORD", 32)],
        compose: r#"services:
  unleash:
    image: unleashorg/unleash-server:6.9
    environment:
      DATABASE_URL: postgres://unleash:${POSTGRES_PASSWORD}@db:5432/unleash
      DATABASE_SSL: "false"
      UNLEASH_URL: ${VERCELIFY_URL}
    depends_on:
      db:
        condition: service_healthy
    restart: unless-stopped
  db:
    image: postgres:17-alpine
    environment:
      POSTGRES_DB: unleash
      POSTGRES_USER: unleash
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}
    volumes:
      - db:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U unleash -d unleash"]
      interval: 5s
      retries: 20
    restart: unless-stopped
volumes:
  db:
"#,
    },
    Template {
        id: "uptime-kuma",
        name: "Uptime Kuma",
        description: "Status pages and uptime monitoring for any website or service.",
        category: "monitoring",
        website: "https://uptime.kuma.pet",
        min_memory_mb: 256,
        service: "kuma",
        port: 3001,
        health_path: None,
        secrets: &[],
        compose: r#"services:
  kuma:
    image: louislam/uptime-kuma:1.23.16
    volumes:
      - data:/app/data
    restart: unless-stopped
volumes:
  data:
"#,
    },
    Template {
        id: "vaultwarden",
        name: "Vaultwarden",
        description: "A password manager server compatible with the Bitwarden apps. Keep it on HTTPS.",
        category: "security",
        website: "https://github.com/dani-garcia/vaultwarden",
        min_memory_mb: 128,
        service: "vaultwarden",
        port: 80,
        health_path: Some("/alive"),
        secrets: &[("ADMIN_TOKEN", 48)],
        compose: r#"services:
  vaultwarden:
    image: vaultwarden/server:1.33.2-alpine
    environment:
      DOMAIN: ${VERCELIFY_URL}
      ADMIN_TOKEN: ${ADMIN_TOKEN}
      SIGNUPS_ALLOWED: "true"
    volumes:
      - data:/data
    restart: unless-stopped
volumes:
  data:
"#,
    },
];

pub fn list() -> Vec<ServiceTemplate> {
    TEMPLATES.iter().map(info).collect()
}

fn info(t: &Template) -> ServiceTemplate {
    ServiceTemplate {
        id: t.id.into(),
        name: t.name.into(),
        description: t.description.into(),
        category: t.category.into(),
        website: t.website.into(),
        min_memory_mb: t.min_memory_mb,
        compose: t.compose.into(),
    }
}

pub fn find(id: &str) -> Result<&'static Template> {
    TEMPLATES
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| Error::not_found(format!("template {id}")))
}

/// Create the app of a template, with its secrets generated (and kept as
/// secret variables of the app). The caller deploys it.
pub fn create(c: &Connection, key: &MasterKey, id: &str, req: &TemplateCreate) -> Result<App> {
    let t = find(id)?;
    let app = apps::create(
        c,
        &AppCreate {
            name: req.name.clone().unwrap_or_else(|| t.id.to_string()),
            kind: AppKind::Compose,
            environment_id: req.environment_id.clone(),
            project_id: req.project_id.clone(),
            source: AppSource {
                compose_content: Some(t.compose.to_string()),
                ..AppSource::default()
            },
            build: None,
            runtime: Some(AppRuntime {
                port: Some(t.port),
                service: Some(t.service.to_string()),
                health: t.health_path.map(|p| HealthCheck {
                    path: Some(p.to_string()),
                    command: None,
                }),
                // First starts pull images and run migrations.
                health_timeout_secs: 600,
                ..AppRuntime::default()
            }),
            previews: None,
            node_id: None,
            domains: vec![],
        },
    )?;
    if !t.secrets.is_empty() {
        env::put(
            c,
            key,
            VarScope::App,
            &app.id,
            &EnvVarsPut {
                vars: t
                    .secrets
                    .iter()
                    .map(|(k, n)| EnvVarInput {
                        key: (*k).to_string(),
                        value: random_token(*n),
                        build: None,
                        preview_only: None,
                        secret: Some(true),
                    })
                    .collect(),
                replace: false,
            },
        )?;
    }
    apps::get(c, &app.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    #[test]
    fn every_template_is_well_formed() {
        let mut ids = std::collections::HashSet::new();
        for t in TEMPLATES {
            assert!(ids.insert(t.id), "duplicate {}", t.id);
            assert!(crate::ids::valid_name(t.id), "{}", t.id);
            assert!(t.compose.starts_with("services:\n"), "{}", t.id);
            assert!(
                t.compose.contains(&format!("\n  {}:\n", t.service)),
                "{}: no service {}",
                t.id,
                t.service
            );
            // Every ${VAR} is a generated secret or a system variable.
            for var in t.compose.split("${").skip(1).filter_map(|r| r.split('}').next()) {
                assert!(
                    var == "VERCELIFY_URL" || t.secrets.iter().any(|(k, _)| *k == var),
                    "{}: ${{{var}}} is never set",
                    t.id
                );
            }
            // Images are pinned to a version, never `latest`.
            for line in t.compose.lines().filter(|l| l.trim_start().starts_with("image:")) {
                let image = line.trim_start().trim_start_matches("image:").trim();
                let tag = image.rsplit_once(':').map(|(_, t)| t).unwrap_or("");
                assert!(!tag.is_empty() && !tag.contains("latest"), "{}: {image}", t.id);
            }
        }
    }

    #[test]
    fn create_generates_secrets() {
        let s = Store::open_in_memory().unwrap();
        let key = MasterKey::from_bytes(&[1u8; 32]).unwrap();
        let a = s.tx(|c| create(c, &key, "umami", &TemplateCreate::default())).unwrap();
        assert_eq!(a.name, "umami");
        assert_eq!(a.kind, AppKind::Compose);
        assert!(a.source.repository.is_none());
        assert!(a.source.compose_content.as_deref().unwrap().contains("umami-software"));
        assert_eq!(a.runtime.service.as_deref(), Some("umami"));
        assert_eq!(a.runtime.port, Some(3000));
        let vars = s.with(|c| env::list(c, &key, VarScope::App, &a.id, true)).unwrap();
        let pw = vars.iter().find(|v| v.key == "POSTGRES_PASSWORD").unwrap();
        assert!(pw.secret);
        assert_eq!(pw.value.as_deref().unwrap().len(), 32);
        // A second one needs its own name.
        assert!(s.tx(|c| create(c, &key, "umami", &TemplateCreate::default())).is_err());
        let b = s
            .tx(|c| {
                create(
                    c,
                    &key,
                    "umami",
                    &TemplateCreate {
                        name: Some("stats".into()),
                        ..TemplateCreate::default()
                    },
                )
            })
            .unwrap();
        let other = s.with(|c| env::list(c, &key, VarScope::App, &b.id, true)).unwrap();
        assert_ne!(
            other.iter().find(|v| v.key == "POSTGRES_PASSWORD").unwrap().value,
            pw.value,
            "secrets differ per app"
        );
        assert!(s.tx(|c| create(c, &key, "nope", &TemplateCreate::default())).is_err());
    }
}
