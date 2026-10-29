//! "vfy pack": turn a checked-out repository into a build plan (design §6.3).
//!
//! Order: the app's explicit pack → a Dockerfile → a compose file (compose
//! apps) → language detection, which writes a multi-stage Dockerfile. Only
//! files are read; nothing from the repository runs on the host. The
//! generated Dockerfile is printed in the build log so it can be copied into
//! the repository and adjusted.
//!
//! Build-time variables reach `RUN` steps as a BuildKit secret (`/run/secrets/env`,
//! shell-quoted `export` lines written by us), so they never end up in an
//! image layer or in `docker history`.

use crate::{validate, Error, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use vercelify_core::platform::{AppBuild, AppKind, AppSource, BuildPack};

#[derive(Debug, Clone, PartialEq)]
pub enum PlanKind {
    /// The repository's own Dockerfile (path relative to the context).
    Dockerfile(String),
    /// A Dockerfile we wrote.
    Generated(String),
    /// A compose file (path relative to the repository root).
    Compose(String),
    /// Nothing to build.
    Image(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuildPlan {
    pub kind: PlanKind,
    /// Build context relative to the repository root ("." for the root).
    pub context: String,
    /// Port the app listens on, unless the app sets one.
    pub port: u16,
    /// What was detected, for the log and the UI ("Next.js (pnpm)").
    pub framework: String,
    pub notes: Vec<String>,
}

/// The build settings of `vercel.json` (`installCommand`, `buildCommand`,
/// `outputDirectory`, `framework`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VercelBuild {
    pub install_command: Option<String>,
    pub build_command: Option<String>,
    pub output_directory: Option<String>,
    pub framework: Option<String>,
}

impl VercelBuild {
    pub fn read(dir: &Path) -> Self {
        let Some(v) = read_json(&dir.join("vercel.json")) else {
            return Self::default();
        };
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Self {
            install_command: s("installCommand"),
            build_command: s("buildCommand"),
            output_directory: s("outputDirectory"),
            framework: s("framework"),
        }
    }
}

const NODE_MAJORS: [u32; 4] = [18, 20, 22, 24];
const DEFAULT_NODE: u32 = 22;
const CADDY: &str = "caddy:2.8-alpine";

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn exists(dir: &Path, f: &str) -> bool {
    dir.join(f).exists()
}

/// Commands from settings end up in a `RUN` line: one line, no NUL.
fn one_line(what: &str, cmd: &str) -> Result<String> {
    let c = cmd.trim();
    if c.is_empty() || c.contains(['\n', '\r', '\0']) {
        return Err(Error::Invalid(format!("{what} must be a single line")));
    }
    Ok(c.to_string())
}

/// Wrap a build step so build-time variables are loaded first.
fn with_env(cmd: &str) -> String {
    format!(
        "RUN --mount=type=secret,id=env,required=false sh -c 'if [ -f /run/secrets/env ]; then . /run/secrets/env; fi; {}'",
        cmd.replace('\'', r"'\''")
    )
}

/// Shell-quoted `export` lines for the build secret.
pub fn build_env_file(vars: &[(String, String)]) -> String {
    vars.iter()
        .map(|(k, v)| format!("export {k}='{}'\n", v.replace('\'', r"'\''")))
        .collect()
}

pub fn plan(root: &Path, kind: AppKind, source: &AppSource, build: &AppBuild) -> Result<BuildPlan> {
    let context = build
        .context
        .clone()
        .filter(|c| !c.is_empty() && c != ".")
        .unwrap_or_else(|| ".".into());
    if context != "." {
        validate::repo_path("context", &context)?;
    }
    let dir: PathBuf = if context == "." {
        root.to_path_buf()
    } else {
        root.join(&context)
    };
    if !dir.is_dir() {
        return Err(Error::Invalid(format!(
            "the build context {context} doesn't exist in the repository"
        )));
    }
    if kind == AppKind::Image {
        let image = source
            .image
            .clone()
            .ok_or_else(|| Error::Invalid("an image app needs an image".into()))?;
        return Ok(BuildPlan {
            kind: PlanKind::Image(image),
            context,
            port: 80,
            framework: "image".into(),
            notes: vec![],
        });
    }
    if kind == AppKind::Compose {
        let file = source
            .compose_file
            .clone()
            .unwrap_or_else(|| "docker-compose.yml".into());
        validate::repo_path("compose file", &file)?;
        if !root.join(&file).is_file() {
            return Err(Error::Invalid(format!("{file} is not in the repository")));
        }
        return Ok(BuildPlan {
            kind: PlanKind::Compose(file),
            context,
            port: 80,
            framework: "Docker Compose".into(),
            notes: vec![],
        });
    }
    let vercel = VercelBuild::read(&dir);
    let mut b = build.clone();
    // The app's own settings win over vercel.json.
    b.install_command = b.install_command.or(vercel.install_command);
    b.build_command = b.build_command.or(vercel.build_command);
    b.output_directory = b.output_directory.or(vercel.output_directory);

    let dockerfile = b.dockerfile.clone().unwrap_or_else(|| "Dockerfile".into());
    validate::repo_path("dockerfile", &dockerfile)?;
    let has_dockerfile = dir.join(&dockerfile).is_file();
    let mut p = match b.pack {
        BuildPack::Dockerfile => {
            if !has_dockerfile {
                return Err(Error::Invalid(format!("{dockerfile} is not in {context}")));
            }
            dockerfile_plan(&dir, &dockerfile)
        }
        BuildPack::Auto if has_dockerfile => dockerfile_plan(&dir, &dockerfile),
        BuildPack::Auto => detect(&dir, &b, vercel.framework.as_deref())?,
        BuildPack::Static => static_plan(&dir, &b, "Static site")?,
        BuildPack::Node => node(&dir, &b, vercel.framework.as_deref())?,
        BuildPack::Python => python(&dir)?,
        BuildPack::Go => go(&dir)?,
        BuildPack::Rust => rust(&dir)?,
        BuildPack::Deno => deno(&dir)?,
        BuildPack::Bun => bun(&dir, &b)?,
        BuildPack::Ruby => ruby(&dir)?,
        BuildPack::Php => php(&dir)?,
    };
    p.context = context;
    Ok(p)
}

fn dockerfile_plan(dir: &Path, path: &str) -> BuildPlan {
    // EXPOSE in the Dockerfile is the best hint for the port.
    let port = read(&dir.join(path))
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("EXPOSE ").or_else(|| l.strip_prefix("expose "))?;
            rest.split_whitespace().next()?.split('/').next()?.parse::<u16>().ok()
        })
        .next()
        .unwrap_or(3000);
    BuildPlan {
        kind: PlanKind::Dockerfile(path.into()),
        context: ".".into(),
        port,
        framework: "Dockerfile".into(),
        notes: vec![],
    }
}

fn detect(dir: &Path, b: &AppBuild, vercel_framework: Option<&str>) -> Result<BuildPlan> {
    let has_pkg = exists(dir, "package.json");
    if has_pkg {
        let pkg = read_json(&dir.join("package.json")).unwrap_or(Value::Null);
        let bun_lock = exists(dir, "bun.lockb") || exists(dir, "bun.lock");
        if bun_lock && node_framework(&pkg, dir).is_none() && !has_dep(&pkg, "vite") {
            return bun(dir, b);
        }
        return node(dir, b, vercel_framework);
    }
    if exists(dir, "deno.json") || exists(dir, "deno.jsonc") {
        return deno(dir);
    }
    if ["requirements.txt", "pyproject.toml", "Pipfile", "uv.lock"]
        .iter()
        .any(|f| exists(dir, f))
    {
        return python(dir);
    }
    if exists(dir, "go.mod") {
        return go(dir);
    }
    if exists(dir, "Cargo.toml") {
        return rust(dir);
    }
    if exists(dir, "Gemfile") {
        return ruby(dir);
    }
    if exists(dir, "composer.json") || exists(dir, "index.php") {
        return php(dir);
    }
    if exists(dir, "index.html") || b.output_directory.is_some() {
        return static_plan(dir, b, "Static site");
    }
    Err(Error::Invalid(
        "couldn't tell how to build this repository: no Dockerfile, package.json, requirements.txt/pyproject.toml, \
         go.mod, Cargo.toml, deno.json, Gemfile, composer.json or index.html. Add a Dockerfile, or pick a build pack."
            .into(),
    ))
}

// ----- Node -------------------------------------------------------------------

fn has_dep(pkg: &Value, name: &str) -> bool {
    ["dependencies", "devDependencies"]
        .iter()
        .any(|k| pkg.get(k).and_then(|d| d.get(name)).is_some())
}

fn script<'a>(pkg: &'a Value, name: &str) -> Option<&'a str> {
    pkg.get("scripts")?.get(name)?.as_str()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Pm {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

fn package_manager(dir: &Path, pkg: &Value) -> Pm {
    let declared = pkg.get("packageManager").and_then(Value::as_str).unwrap_or("");
    if declared.starts_with("pnpm") || exists(dir, "pnpm-lock.yaml") {
        Pm::Pnpm
    } else if declared.starts_with("yarn") || exists(dir, "yarn.lock") {
        Pm::Yarn
    } else if declared.starts_with("bun") || exists(dir, "bun.lockb") || exists(dir, "bun.lock") {
        Pm::Bun
    } else {
        Pm::Npm
    }
}

impl Pm {
    fn name(self) -> &'static str {
        match self {
            Pm::Npm => "npm",
            Pm::Pnpm => "pnpm",
            Pm::Yarn => "yarn",
            Pm::Bun => "bun",
        }
    }

    fn install(self, dir: &Path) -> String {
        match self {
            Pm::Npm if exists(dir, "package-lock.json") || exists(dir, "npm-shrinkwrap.json") => "npm ci".into(),
            Pm::Npm => "npm install".into(),
            // Frozen installs fail outright without a lockfile (a repo that
            // only declares `packageManager`): install from package.json.
            Pm::Pnpm if exists(dir, "pnpm-lock.yaml") => "pnpm install --frozen-lockfile".into(),
            Pm::Pnpm => "pnpm install".into(),
            Pm::Yarn if !exists(dir, "yarn.lock") => "yarn install".into(),
            Pm::Yarn if exists(dir, ".yarnrc.yml") => "yarn install --immutable".into(),
            Pm::Yarn => "yarn install --frozen-lockfile".into(),
            Pm::Bun if exists(dir, "bun.lockb") || exists(dir, "bun.lock") => "bun install --frozen-lockfile".into(),
            Pm::Bun => "bun install".into(),
        }
    }

    fn run(self, script: &str) -> String {
        match self {
            Pm::Npm => format!("npm run {script}"),
            Pm::Pnpm => format!("pnpm run {script}"),
            Pm::Yarn => format!("yarn run {script}"),
            Pm::Bun => format!("bun run {script}"),
        }
    }

    fn cache(self) -> &'static str {
        match self {
            Pm::Npm => "/root/.npm",
            Pm::Pnpm => "/root/.local/share/pnpm/store",
            Pm::Yarn => "/usr/local/share/.cache/yarn",
            Pm::Bun => "/root/.bun/install/cache",
        }
    }

    /// Lockfiles to copy before installing (dependency layer cache).
    fn lockfiles(self) -> &'static [&'static str] {
        match self {
            Pm::Npm => &["package-lock.json", "npm-shrinkwrap.json"],
            Pm::Pnpm => &["pnpm-lock.yaml", "pnpm-workspace.yaml"],
            Pm::Yarn => &["yarn.lock", ".yarnrc.yml"],
            Pm::Bun => &["bun.lockb", "bun.lock"],
        }
    }
}

/// Node major from `engines.node`, `.nvmrc` or `.node-version`.
fn node_major(dir: &Path, pkg: &Value) -> u32 {
    let wanted = [read(&dir.join(".nvmrc")), read(&dir.join(".node-version"))]
        .into_iter()
        .chain(
            pkg.get("engines")
                .and_then(|e| e.get("node"))
                .and_then(Value::as_str)
                .map(String::from),
        )
        .find(|s| !s.trim().is_empty());
    let Some(w) = wanted else { return DEFAULT_NODE };
    let w = w.trim();
    if w.starts_with("lts") || w == "node" || w == "latest" {
        return DEFAULT_NODE;
    }
    // ">=18.17", "^20", "20.x", "v20.11.0", "18 || 20": the highest supported
    // major that the first number allows.
    let first: Option<u32> = w
        .split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())
        .and_then(|s| s.parse().ok());
    // An upper bound (">=18 <21", ">=18 <=20"): stay under it.
    let below = |m: u32| match w.split_once('<') {
        Some((_, rest)) => {
            let inclusive = rest.starts_with('=');
            let n: Option<u32> = rest
                .split(|c: char| !c.is_ascii_digit())
                .find(|s| !s.is_empty())
                .and_then(|s| s.parse().ok());
            n.is_none_or(|n| if inclusive { m <= n } else { m < n })
        }
        None => true,
    };
    match first {
        Some(n) if w.contains(">=") || w.starts_with('>') => NODE_MAJORS
            .iter()
            .copied()
            .filter(|m| *m >= n && below(*m))
            .max()
            .unwrap_or(DEFAULT_NODE),
        Some(n) => NODE_MAJORS.iter().copied().find(|m| *m == n).unwrap_or_else(|| {
            NODE_MAJORS
                .iter()
                .copied()
                .filter(|m| *m >= n)
                .min()
                .unwrap_or(DEFAULT_NODE)
        }),
        None => DEFAULT_NODE,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum NodeFw {
    Next { standalone: bool },
    Nuxt,
    SvelteKitNode,
    SvelteKitStatic,
    Remix,
    ReactRouter,
    AstroServer,
    AstroStatic,
    Vite,
    Cra,
    Gatsby,
    Angular,
}

fn file_contains(dir: &Path, names: &[&str], needle: &str) -> bool {
    names.iter().any(|n| read(&dir.join(n)).contains(needle))
}

fn node_framework(pkg: &Value, dir: &Path) -> Option<NodeFw> {
    if has_dep(pkg, "next") {
        let configs = ["next.config.js", "next.config.mjs", "next.config.ts", "next.config.cjs"];
        let standalone = configs.iter().any(|c| {
            let s = read(&dir.join(c));
            s.contains("output: 'standalone'")
                || s.contains("output: \"standalone\"")
                || s.contains("output:'standalone'")
        });
        return Some(NodeFw::Next { standalone });
    }
    if has_dep(pkg, "nuxt") {
        return Some(NodeFw::Nuxt);
    }
    if has_dep(pkg, "@sveltejs/kit") {
        return Some(if has_dep(pkg, "@sveltejs/adapter-static") {
            NodeFw::SvelteKitStatic
        } else {
            NodeFw::SvelteKitNode
        });
    }
    if has_dep(pkg, "@react-router/dev") {
        return Some(NodeFw::ReactRouter);
    }
    if pkg
        .get("dependencies")
        .and_then(Value::as_object)
        .is_some_and(|d| d.keys().any(|k| k.starts_with("@remix-run/")))
    {
        return Some(NodeFw::Remix);
    }
    if has_dep(pkg, "astro") {
        let cfgs = ["astro.config.mjs", "astro.config.ts", "astro.config.js"];
        let server = has_dep(pkg, "@astrojs/node")
            || file_contains(dir, &cfgs, "output: 'server'")
            || file_contains(dir, &cfgs, "output: \"server\"");
        return Some(if server {
            NodeFw::AstroServer
        } else {
            NodeFw::AstroStatic
        });
    }
    if has_dep(pkg, "gatsby") {
        return Some(NodeFw::Gatsby);
    }
    if has_dep(pkg, "@angular/core") && !has_dep(pkg, "@angular/ssr") {
        return Some(NodeFw::Angular);
    }
    if has_dep(pkg, "react-scripts") {
        return Some(NodeFw::Cra);
    }
    if has_dep(pkg, "vite") && script(pkg, "start").is_none() {
        return Some(NodeFw::Vite);
    }
    None
}

fn node(dir: &Path, b: &AppBuild, vercel_framework: Option<&str>) -> Result<BuildPlan> {
    let pkg = read_json(&dir.join("package.json"))
        .ok_or_else(|| Error::Invalid("package.json is missing or not valid JSON".into()))?;
    let pm = package_manager(dir, &pkg);
    let major = node_major(dir, &pkg);
    let fw = node_framework(&pkg, dir);
    let mut notes = vec![];
    let install = match &b.install_command {
        Some(c) => one_line("install command", c)?,
        None => pm.install(dir),
    };
    let build_cmd = match &b.build_command {
        Some(c) => Some(one_line("build command", c)?),
        None => script(&pkg, "build").map(|_| pm.run("build")),
    };
    let workspaces = pkg.get("workspaces").is_some() || exists(dir, "pnpm-workspace.yaml");

    // Static frameworks: build, then serve the output with the static stage.
    let static_out = match fw {
        Some(NodeFw::Vite) | Some(NodeFw::AstroStatic) | Some(NodeFw::SvelteKitStatic) => Some("dist"),
        Some(NodeFw::Cra) => Some("build"),
        Some(NodeFw::Gatsby) => Some("public"),
        Some(NodeFw::Angular) => Some("dist"),
        _ => None,
    }
    .map(|d| {
        if fw == Some(NodeFw::SvelteKitStatic) {
            "build"
        } else {
            d
        }
    });
    let static_out = b
        .output_directory
        .clone()
        .or_else(|| static_out.map(String::from))
        .filter(|_| static_out.is_some() || script(&pkg, "start").is_none() && b.output_directory.is_some());
    let label = |name: &str| format!("{name} ({}, Node {major})", pm.name());

    let mut df = String::from(
        "# syntax=docker/dockerfile:1.7\n# Generated by Vercelify (vfy pack). Commit it as Dockerfile to customise.\n",
    );
    df.push_str(&format!("FROM node:{major}-slim AS build\nWORKDIR /app\n"));
    df.push_str("ENV NEXT_TELEMETRY_DISABLED=1 ASTRO_TELEMETRY_DISABLED=1 NUXT_TELEMETRY_DISABLED=1 CI=1\n");
    if matches!(pm, Pm::Pnpm | Pm::Yarn) {
        // No "download pnpm? [Y/n]" question can stop a build.
        df.push_str("ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0\nRUN corepack enable\n");
    }
    if pm == Pm::Bun {
        df.push_str("RUN npm install -g bun@1\n");
    }
    if workspaces || b.install_command.is_some() {
        // Workspaces need every package.json before installing.
        df.push_str("COPY . .\n");
    } else {
        let mut files = vec!["package.json".to_string()];
        files.extend(pm.lockfiles().iter().filter(|f| exists(dir, f)).map(|f| f.to_string()));
        if exists(dir, ".npmrc") {
            files.push(".npmrc".into());
        }
        df.push_str(&format!("COPY {} ./\n", files.join(" ")));
    }
    df.push_str(&format!(
        "RUN --mount=type=cache,target={} --mount=type=secret,id=env,required=false sh -c 'if [ -f /run/secrets/env ]; then . /run/secrets/env; fi; {}'\n",
        pm.cache(),
        install.replace('\'', r"'\''")
    ));
    df.push_str("COPY . .\n");
    if let Some(c) = &build_cmd {
        df.push_str(&with_env(c));
        df.push('\n');
    } else if static_out.is_none() {
        notes.push("package.json has no build script: nothing is built".into());
    }

    if let Some(out) = static_out {
        validate::repo_path("output directory", &out)?;
        let name = match fw {
            Some(NodeFw::Vite) => "Vite",
            Some(NodeFw::AstroStatic) => "Astro (static)",
            Some(NodeFw::SvelteKitStatic) => "SvelteKit (static)",
            Some(NodeFw::Cra) => "Create React App",
            Some(NodeFw::Gatsby) => "Gatsby",
            Some(NodeFw::Angular) => "Angular",
            _ => "Static site",
        };
        df.push_str(&static_stage(&format!("/app/{out}"), Some("build")));
        return Ok(BuildPlan {
            kind: PlanKind::Generated(df),
            context: ".".into(),
            port: 8080,
            framework: label(name),
            notes,
        });
    }

    let (name, port, cmd): (&str, u16, String) = match fw {
        Some(NodeFw::Next { standalone: true }) => ("Next.js (standalone)", 3000, r#"["node", "server.js"]"#.into()),
        Some(NodeFw::Next { standalone: false }) => (
            "Next.js",
            3000,
            json_cmd(&start_or(&pkg, pm, "node_modules/.bin/next start -H 0.0.0.0")),
        ),
        Some(NodeFw::Nuxt) => ("Nuxt", 3000, r#"["node", ".output/server/index.mjs"]"#.into()),
        Some(NodeFw::SvelteKitNode) => ("SvelteKit", 3000, r#"["node", "build"]"#.into()),
        Some(NodeFw::Remix) => (
            "Remix",
            3000,
            json_cmd(&start_or(
                &pkg,
                pm,
                "node_modules/.bin/remix-serve ./build/server/index.js",
            )),
        ),
        Some(NodeFw::ReactRouter) => (
            "React Router",
            3000,
            json_cmd(&start_or(
                &pkg,
                pm,
                "node_modules/.bin/react-router-serve ./build/server/index.js",
            )),
        ),
        Some(NodeFw::AstroServer) => ("Astro (server)", 4321, r#"["node", "./dist/server/entry.mjs"]"#.into()),
        _ => {
            let start = if script(&pkg, "start").is_some() {
                pm.run("start")
            } else if let Some(main) = pkg.get("main").and_then(Value::as_str) {
                validate::repo_path("main", main)?;
                format!("node {main}")
            } else if exists(dir, "server.js") {
                "node server.js".into()
            } else if exists(dir, "index.js") {
                "node index.js".into()
            } else {
                return Err(Error::Invalid(
                    "package.json has no start script and no main file; add \"start\" to scripts".into(),
                ));
            };
            ("Node.js", 3000, json_cmd(&start))
        }
    };
    if vercel_framework.is_some_and(|f| f != "nextjs") && matches!(fw, Some(NodeFw::Next { .. })) {
        notes.push("vercel.json names another framework; detected Next.js from package.json".into());
    }

    df.push_str(&format!("\nFROM node:{major}-slim\nWORKDIR /app\n"));
    df.push_str(&format!(
        "ENV NODE_ENV=production PORT={port} HOST=0.0.0.0 HOSTNAME=0.0.0.0 NEXT_TELEMETRY_DISABLED=1\n"
    ));
    if matches!(fw, Some(NodeFw::Next { standalone: true })) {
        df.push_str("COPY --from=build --chown=node:node /app/.next/standalone ./\n");
        df.push_str("COPY --from=build --chown=node:node /app/.next/static ./.next/static\n");
        if exists(dir, "public") {
            df.push_str("COPY --from=build --chown=node:node /app/public ./public\n");
        }
    } else {
        if matches!(pm, Pm::Pnpm | Pm::Yarn) {
            df.push_str("RUN corepack enable\n");
        }
        if pm == Pm::Bun {
            df.push_str("RUN npm install -g bun@1\n");
        }
        df.push_str("COPY --from=build --chown=node:node /app ./\n");
    }
    df.push_str(&format!("USER node\nEXPOSE {port}\nCMD {cmd}\n"));
    Ok(BuildPlan {
        kind: PlanKind::Generated(df),
        context: ".".into(),
        port,
        framework: label(name),
        notes,
    })
}

fn start_or(pkg: &Value, pm: Pm, fallback: &str) -> String {
    if script(pkg, "start").is_some() {
        pm.run("start")
    } else {
        fallback.into()
    }
}

/// `npm run start` → `["npm", "run", "start"]` (exec form: signals reach the app).
fn json_cmd(cmd: &str) -> String {
    exec_form(&cmd.split_whitespace().collect::<Vec<_>>())
}

/// `["a", "b"]`, as Dockerfiles are usually written.
fn exec_form(parts: &[&str]) -> String {
    let quoted: Vec<String> = parts
        .iter()
        .map(|p| serde_json::to_string(p).expect("serializable"))
        .collect();
    format!("[{}]", quoted.join(", "))
}

// ----- static ----------------------------------------------------------------

/// The static file server stage: Caddy with clean URLs, a 404 page if there
/// is one, compression and long caching for hashed assets.
fn static_stage(from_path: &str, from_stage: Option<&str>) -> String {
    let copy = match from_stage {
        Some(stage) => format!("COPY --from={stage} {from_path} /srv"),
        None => format!("COPY {from_path} /srv"),
    };
    format!(
        "\nFROM {CADDY}\n{copy}\nCOPY <<'CADDY' /etc/caddy/Caddyfile\n\
         {{\n\tadmin off\n\tauto_https off\n}}\n\
         :8080 {{\n\troot * /srv\n\tencode zstd gzip\n\
         \t@assets path /assets/* /_astro/* /static/* /_app/immutable/*\n\
         \theader @assets Cache-Control \"public, max-age=31536000, immutable\"\n\
         \ttry_files {{path}} {{path}}.html {{path}}/index.html\n\
         \tfile_server\n\
         \thandle_errors {{\n\t\t@404 expression {{err.status_code}} == 404\n\t\thandle @404 {{\n\t\t\trewrite * /404.html\n\t\t\tfile_server\n\t\t}}\n\t}}\n}}\nCADDY\n\
         EXPOSE 8080\nCMD [\"caddy\", \"run\", \"--config\", \"/etc/caddy/Caddyfile\"]\n"
    )
}

fn static_plan(dir: &Path, b: &AppBuild, name: &str) -> Result<BuildPlan> {
    let out = b.output_directory.clone().unwrap_or_else(|| ".".into());
    if out != "." {
        validate::repo_path("output directory", &out)?;
    }
    let mut df = String::from("# syntax=docker/dockerfile:1.7\n# Generated by Vercelify (vfy pack).\n");
    let stage = if let Some(cmd) = &b.build_command {
        df.push_str("FROM node:22-slim AS build\nWORKDIR /app\nCOPY . .\n");
        if let Some(i) = &b.install_command {
            df.push_str(&with_env(&one_line("install command", i)?));
            df.push('\n');
        }
        df.push_str(&with_env(&one_line("build command", cmd)?));
        df.push('\n');
        Some("build")
    } else {
        if out != "." && !dir.join(&out).is_dir() {
            return Err(Error::Invalid(format!(
                "the output directory {out} is not in the repository"
            )));
        }
        None
    };
    let from = match (stage, out.as_str()) {
        (Some(_), ".") => "/app".to_string(),
        (Some(_), o) => format!("/app/{o}"),
        (None, ".") => ".".to_string(),
        (None, o) => o.to_string(),
    };
    df.push_str(&static_stage(&from, stage));
    Ok(BuildPlan {
        kind: PlanKind::Generated(df),
        context: ".".into(),
        port: 8080,
        framework: name.into(),
        notes: vec![],
    })
}

// ----- other languages -----------------------------------------------------------

/// A `web:` line of a Procfile, the cross-language way to say how to start.
fn procfile_web(dir: &Path) -> Option<String> {
    read(&dir.join("Procfile"))
        .lines()
        .find_map(|l| l.strip_prefix("web:").map(|c| c.trim().to_string()))
        .filter(|c| !c.is_empty() && !c.contains(['\n', '\0']))
}

fn shell_cmd(cmd: &str) -> String {
    exec_form(&["sh", "-c", &format!("exec {cmd}")])
}

/// Find `<var> = FastAPI(` / `Flask(` in the usual entry files: (module, var).
fn python_app(dir: &Path, ctor: &str) -> Option<(String, String)> {
    for f in [
        "main.py",
        "app.py",
        "server.py",
        "app/main.py",
        "src/main.py",
        "api/main.py",
        "wsgi.py",
        "asgi.py",
    ] {
        let src = read(&dir.join(f));
        for line in src.lines() {
            let l = line.trim();
            if let Some((var, rhs)) = l.split_once('=') {
                let var = var.trim();
                if rhs.trim_start().starts_with(&format!("{ctor}("))
                    && !var.is_empty()
                    && var.chars().all(|c| c.is_alphanumeric() || c == '_')
                {
                    let module = f.trim_end_matches(".py").replace('/', ".");
                    return Some((module, var.to_string()));
                }
            }
        }
    }
    None
}

fn python(dir: &Path) -> Result<BuildPlan> {
    let reqs = read(&dir.join("requirements.txt")).to_ascii_lowercase();
    let pyproject = read(&dir.join("pyproject.toml")).to_ascii_lowercase();
    let deps = format!("{reqs}\n{pyproject}");
    let dep = |n: &str| deps.contains(n);
    let version = read(&dir.join(".python-version"));
    let version = version.trim().split('.').take(2).collect::<Vec<_>>().join(".");
    let version = if ["3.10", "3.11", "3.12", "3.13"].contains(&version.as_str()) {
        version
    } else {
        "3.12".into()
    };
    let install = if exists(dir, "uv.lock") {
        "pip install uv && uv export --frozen --no-hashes -o /tmp/req.txt && pip install -r /tmp/req.txt && pip install --no-deps ."
    } else if exists(dir, "requirements.txt") {
        "pip install -r requirements.txt"
    } else if exists(dir, "poetry.lock") {
        "pip install poetry poetry-plugin-export && poetry export --without-hashes -o /tmp/req.txt && pip install -r /tmp/req.txt"
    } else if exists(dir, "Pipfile") {
        "pip install pipenv && pipenv requirements > /tmp/req.txt && pip install -r /tmp/req.txt"
    } else {
        "pip install ."
    };
    let django = exists(dir, "manage.py") && dep("django");
    let (name, start, extra): (&str, String, &str) = if let Some(cmd) = procfile_web(dir) {
        ("Python (Procfile)", cmd, "")
    } else if django {
        let project = std::fs::read_dir(dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .find(|e| e.path().join("wsgi.py").is_file())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .ok_or_else(|| Error::Invalid("Django project without a <project>/wsgi.py".into()))?;
        (
            "Django",
            format!("gunicorn {project}.wsgi:application --bind 0.0.0.0:$PORT --workers 2"),
            "gunicorn",
        )
    } else if let Some((m, v)) = python_app(dir, "FastAPI") {
        (
            "FastAPI",
            format!("uvicorn {m}:{v} --host 0.0.0.0 --port $PORT"),
            "uvicorn",
        )
    } else if let Some((m, v)) = python_app(dir, "Flask") {
        (
            "Flask",
            format!("gunicorn {m}:{v} --bind 0.0.0.0:$PORT --workers 2"),
            "gunicorn",
        )
    } else if exists(dir, "main.py") {
        ("Python", "python main.py".into(), "")
    } else {
        return Err(Error::Invalid(
            "couldn't find how to start this Python app: add a Procfile with `web: <command>`".into(),
        ));
    };
    let mut df = format!(
        "# syntax=docker/dockerfile:1.7\n# Generated by Vercelify (vfy pack).\nFROM python:{version}-slim\n\
         ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 PIP_DISABLE_PIP_VERSION_CHECK=1 PORT=8000\nWORKDIR /app\nCOPY . .\n\
         RUN --mount=type=cache,target=/root/.cache/pip --mount=type=secret,id=env,required=false sh -c 'if [ -f /run/secrets/env ]; then . /run/secrets/env; fi; {install}'\n"
    );
    if !extra.is_empty() && !dep(extra) {
        df.push_str(&format!(
            "RUN --mount=type=cache,target=/root/.cache/pip pip install {extra}\n"
        ));
    }
    if django {
        df.push_str(&with_env("python manage.py collectstatic --noinput || true"));
        df.push('\n');
    }
    df.push_str(&format!(
        "RUN useradd --create-home --uid 10001 app && chown -R app /app\nUSER app\nEXPOSE 8000\nCMD {}\n",
        shell_cmd(&start)
    ));
    Ok(BuildPlan {
        kind: PlanKind::Generated(df),
        context: ".".into(),
        port: 8000,
        framework: name.into(),
        notes: vec![],
    })
}

fn go(dir: &Path) -> Result<BuildPlan> {
    let gomod = read(&dir.join("go.mod"));
    let version = gomod
        .lines()
        .find_map(|l| l.strip_prefix("go "))
        .map(|v| v.trim().split('.').take(2).collect::<Vec<_>>().join("."))
        .filter(|v| v.starts_with("1.") && v.len() <= 5)
        .unwrap_or_else(|| "1.23".into());
    // `package main` at the root, or a single directory under cmd/.
    let target = if read(&dir.join("main.go")).contains("package main") {
        ".".to_string()
    } else {
        let cmds: Vec<String> = std::fs::read_dir(dir.join("cmd"))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        match cmds.as_slice() {
            [one] => format!("./cmd/{one}"),
            [] => ".".into(),
            many => {
                return Err(Error::Invalid(format!(
                    "several commands under cmd/ ({}); add a Dockerfile that builds the right one",
                    many.join(", ")
                )))
            }
        }
    };
    let df = format!(
        "# syntax=docker/dockerfile:1.7\n# Generated by Vercelify (vfy pack).\nFROM golang:{version} AS build\nWORKDIR /src\n\
         COPY go.mod go.sum* ./\nRUN --mount=type=cache,target=/go/pkg/mod go mod download\nCOPY . .\n\
         RUN --mount=type=cache,target=/go/pkg/mod --mount=type=cache,target=/root/.cache/go-build \
         CGO_ENABLED=0 go build -trimpath -ldflags='-s -w' -o /out/app {target}\n\n\
         FROM gcr.io/distroless/static-debian12:nonroot\nCOPY --from=build /out/app /app\nENV PORT=8080\nEXPOSE 8080\nENTRYPOINT [\"/app\"]\n"
    );
    Ok(BuildPlan {
        kind: PlanKind::Generated(df),
        context: ".".into(),
        port: 8080,
        framework: format!("Go {version}"),
        notes: vec![],
    })
}

fn rust(dir: &Path) -> Result<BuildPlan> {
    let cargo = read(&dir.join("Cargo.toml"));
    // [[bin]] name wins, else [package] name.
    let mut section = "";
    let mut bin = None;
    let mut package = None;
    for line in cargo.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            section = l;
            continue;
        }
        if let Some(v) = l.strip_prefix("name").map(str::trim).and_then(|r| r.strip_prefix('=')) {
            let v = v.trim().trim_matches('"').to_string();
            match section {
                "[[bin]]" if bin.is_none() => bin = Some(v),
                "[package]" => package = Some(v),
                _ => {}
            }
        }
    }
    let name = bin
        .or(package)
        .filter(|n| n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .ok_or_else(|| Error::Invalid("Cargo.toml has no package name (a workspace? add a Dockerfile)".into()))?;
    let df = format!(
        "# syntax=docker/dockerfile:1.7\n# Generated by Vercelify (vfy pack).\nFROM rust:1-slim-bookworm AS build\nWORKDIR /src\nCOPY . .\n\
         RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/src/target \
         cargo build --release --locked --bin {name} || cargo build --release --bin {name}; \
         cp target/release/{name} /app\n\n\
         FROM debian:12-slim\nRUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*\n\
         COPY --from=build /app /usr/local/bin/app\nRUN useradd --uid 10001 app\nUSER app\nENV PORT=8080\nEXPOSE 8080\nCMD [\"/usr/local/bin/app\"]\n"
    );
    Ok(BuildPlan {
        kind: PlanKind::Generated(df),
        context: ".".into(),
        port: 8080,
        framework: "Rust".into(),
        notes: vec![],
    })
}

fn deno(dir: &Path) -> Result<BuildPlan> {
    let cfg = read_json(&dir.join("deno.json")).unwrap_or(Value::Null);
    let start = if cfg.get("tasks").and_then(|t| t.get("start")).is_some() {
        "deno task start".to_string()
    } else {
        let entry = ["main.ts", "server.ts", "mod.ts", "main.js"]
            .into_iter()
            .find(|f| exists(dir, f))
            .ok_or_else(|| Error::Invalid("deno.json has no start task and there is no main.ts".into()))?;
        format!("deno run --allow-net --allow-env --allow-read {entry}")
    };
    let df = format!(
        "# syntax=docker/dockerfile:1.7\n# Generated by Vercelify (vfy pack).\nFROM denoland/deno:2.1.4\nWORKDIR /app\nCOPY . .\n\
         RUN deno install || true\nUSER deno\nENV PORT=8000\nEXPOSE 8000\nCMD {}\n",
        shell_cmd(&start)
    );
    Ok(BuildPlan {
        kind: PlanKind::Generated(df),
        context: ".".into(),
        port: 8000,
        framework: "Deno".into(),
        notes: vec![],
    })
}

fn bun(dir: &Path, b: &AppBuild) -> Result<BuildPlan> {
    let pkg = read_json(&dir.join("package.json")).unwrap_or(Value::Null);
    let start = if script(&pkg, "start").is_some() {
        "bun run start".to_string()
    } else {
        let entry = ["index.ts", "src/index.ts", "server.ts", "index.js"]
            .into_iter()
            .find(|f| exists(dir, f))
            .ok_or_else(|| Error::Invalid("no start script and no index.ts".into()))?;
        format!("bun run {entry}")
    };
    let mut df = String::from(
        "# syntax=docker/dockerfile:1.7\n# Generated by Vercelify (vfy pack).\nFROM oven/bun:1.1 \nWORKDIR /app\nCOPY . .\n\
         RUN --mount=type=cache,target=/root/.bun/install/cache bun install --frozen-lockfile || bun install\n",
    );
    if let Some(c) = b
        .build_command
        .as_deref()
        .or(script(&pkg, "build").map(|_| "bun run build"))
    {
        df.push_str(&with_env(&one_line("build command", c)?));
        df.push('\n');
    }
    df.push_str(&format!(
        "USER bun\nENV NODE_ENV=production PORT=3000\nEXPOSE 3000\nCMD {}\n",
        json_cmd(&start)
    ));
    Ok(BuildPlan {
        kind: PlanKind::Generated(df),
        context: ".".into(),
        port: 3000,
        framework: "Bun".into(),
        notes: vec![],
    })
}

fn ruby(dir: &Path) -> Result<BuildPlan> {
    let gemfile = read(&dir.join("Gemfile"));
    let rails = gemfile.contains("\"rails\"") || gemfile.contains("'rails'");
    let version = read(&dir.join(".ruby-version"));
    let version = version.trim().trim_start_matches("ruby-");
    let version = if version.starts_with("3.") && version.len() <= 8 {
        version.split('.').take(2).collect::<Vec<_>>().join(".")
    } else {
        "3.3".into()
    };
    let start = procfile_web(dir).unwrap_or_else(|| {
        if rails {
            "bundle exec rails server -b 0.0.0.0 -p $PORT".into()
        } else if exists(dir, "config.ru") {
            "bundle exec rackup -o 0.0.0.0 -p $PORT".into()
        } else {
            "bundle exec ruby app.rb -o 0.0.0.0 -p $PORT".into()
        }
    });
    let mut df = format!(
        "# syntax=docker/dockerfile:1.7\n# Generated by Vercelify (vfy pack).\nFROM ruby:{version}-slim\n\
         RUN apt-get update && apt-get install -y --no-install-recommends build-essential libpq-dev libyaml-dev git && rm -rf /var/lib/apt/lists/*\n\
         ENV BUNDLE_WITHOUT=development:test BUNDLE_DEPLOYMENT=1 RAILS_ENV=production RACK_ENV=production RAILS_LOG_TO_STDOUT=1 PORT=3000\n\
         WORKDIR /app\nCOPY Gemfile Gemfile.lock* ./\nRUN --mount=type=cache,target=/usr/local/bundle/cache bundle install\nCOPY . .\n"
    );
    if rails {
        df.push_str(&with_env(
            "SECRET_KEY_BASE_DUMMY=1 bundle exec rails assets:precompile || true",
        ));
        df.push('\n');
    }
    df.push_str(&format!(
        "RUN useradd --uid 10001 app && chown -R app /app\nUSER app\nEXPOSE 3000\nCMD {}\n",
        shell_cmd(&start)
    ));
    Ok(BuildPlan {
        kind: PlanKind::Generated(df),
        context: ".".into(),
        port: 3000,
        framework: if rails { "Rails".into() } else { "Ruby".into() },
        notes: vec![],
    })
}

fn php(dir: &Path) -> Result<BuildPlan> {
    let laravel = exists(dir, "artisan");
    let root = if laravel || exists(dir, "public/index.php") {
        "/app/public"
    } else {
        "/app"
    };
    let mut df = String::from("# syntax=docker/dockerfile:1.7\n# Generated by Vercelify (vfy pack).\n");
    if exists(dir, "composer.json") {
        df.push_str(
            "FROM composer:2 AS vendor\nWORKDIR /app\nCOPY composer.json composer.lock* ./\n\
             RUN --mount=type=cache,target=/tmp/cache composer install --no-dev --no-scripts --prefer-dist --no-interaction --ignore-platform-reqs\n\n",
        );
    }
    df.push_str(&format!(
        "FROM dunglas/frankenphp:1.3-php8.3\nRUN install-php-extensions pdo_mysql pdo_pgsql intl zip opcache\n\
         ENV SERVER_NAME=:8080 SERVER_ROOT={root} PORT=8080\nWORKDIR /app\nCOPY . .\n"
    ));
    if exists(dir, "composer.json") {
        df.push_str("COPY --from=vendor /app/vendor ./vendor\n");
    }
    if laravel {
        df.push_str(
            "RUN chown -R www-data:www-data storage bootstrap/cache && php artisan package:discover --ansi || true\n",
        );
    }
    df.push_str("EXPOSE 8080\n");
    Ok(BuildPlan {
        kind: PlanKind::Generated(df),
        context: ".".into(),
        port: 8080,
        framework: if laravel { "Laravel".into() } else { "PHP".into() },
        notes: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (p, c) in files {
            let path = d.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, c).unwrap();
        }
        d
    }

    fn auto(d: &tempfile::TempDir) -> BuildPlan {
        plan(d.path(), AppKind::Git, &AppSource::default(), &AppBuild::default()).unwrap()
    }

    fn dockerfile(p: &BuildPlan) -> &str {
        match &p.kind {
            PlanKind::Generated(s) => s,
            other => panic!("not generated: {other:?}"),
        }
    }

    #[test]
    fn a_dockerfile_wins() {
        let d = repo(&[("Dockerfile", "FROM nginx\nEXPOSE 8081/tcp\n"), ("package.json", "{}")]);
        let p = auto(&d);
        assert_eq!(p.kind, PlanKind::Dockerfile("Dockerfile".into()));
        assert_eq!(p.port, 8081);
    }

    #[test]
    fn nextjs_with_pnpm_and_standalone() {
        let d = repo(&[
            (
                "package.json",
                r#"{"dependencies":{"next":"15"},"scripts":{"build":"next build","start":"next start"},"engines":{"node":">=20"}}"#,
            ),
            ("pnpm-lock.yaml", ""),
            ("next.config.mjs", "export default { output: 'standalone' }"),
            ("public/x.png", ""),
        ]);
        let p = auto(&d);
        let df = dockerfile(&p);
        assert_eq!(p.framework, "Next.js (standalone) (pnpm, Node 24)");
        assert_eq!(p.port, 3000);
        assert!(df.contains("FROM node:24-slim AS build"));
        assert!(df.contains("RUN corepack enable"));
        assert!(df.contains("COPY package.json pnpm-lock.yaml ./"));
        assert!(df.contains("pnpm install --frozen-lockfile"));
        assert!(df.contains("pnpm run build"));
        assert!(df.contains("/app/.next/standalone"));
        assert!(df.contains("/app/public ./public"));
        assert!(df.contains(r#"CMD ["node", "server.js"]"#));
        assert!(df.contains("USER node"));
        assert!(
            df.contains("--mount=type=secret,id=env"),
            "build-time variables as a secret"
        );
    }

    #[test]
    fn node_versions() {
        let v = |engines: &str, nvmrc: Option<&str>| {
            let pkg = format!(r#"{{"engines":{{"node":"{engines}"}}}}"#);
            let mut files = vec![("package.json", pkg.as_str())];
            if let Some(n) = nvmrc {
                files.push((".nvmrc", n));
            }
            let d = repo(&files);
            node_major(d.path(), &read_json(&d.path().join("package.json")).unwrap())
        };
        assert_eq!(v("20.x", None), 20);
        assert_eq!(v("^18.17", None), 18);
        assert_eq!(v(">=18", None), 24);
        assert_eq!(v(">=18 <21", None), 20, "under the upper bound");
        assert_eq!(v(">=18 <=22", None), 22);
        assert_eq!(v("", Some("v20.11.0\n")), 20);
        assert_eq!(v("", Some("lts/*")), 22);
        assert_eq!(v("16", None), 18, "older than supported: the oldest supported");
        assert_eq!(v("", None), 22);
    }

    #[test]
    fn static_frameworks() {
        let d = repo(&[
            (
                "package.json",
                r#"{"devDependencies":{"vite":"5"},"scripts":{"build":"vite build"}}"#,
            ),
            ("package-lock.json", "{}"),
        ]);
        let p = auto(&d);
        let df = dockerfile(&p);
        assert!(p.framework.starts_with("Vite (npm"));
        assert_eq!(p.port, 8080);
        assert!(df.contains("npm ci"));
        assert!(df.contains("COPY --from=build /app/dist /srv"));
        assert!(df.contains("FROM caddy:"));
        assert!(df.contains("try_files {path} {path}.html {path}/index.html"));

        let d = repo(&[("index.html", "<h1>hi</h1>")]);
        let p = auto(&d);
        assert_eq!(p.framework, "Static site");
        assert!(dockerfile(&p).contains("COPY . /srv"));

        // vercel.json outputDirectory and buildCommand are honoured.
        let d = repo(&[
            ("package.json", r#"{"devDependencies":{"vite":"5"}}"#),
            (
                "vercel.json",
                r#"{"buildCommand":"npm run gen","outputDirectory":"public"}"#,
            ),
        ]);
        let df = dockerfile(&auto(&d)).to_string();
        assert!(df.contains("npm run gen"));
        assert!(df.contains("/app/public /srv"));
    }

    #[test]
    fn plain_node_and_errors() {
        let d = repo(&[("package.json", r#"{"main":"src/server.js"}"#)]);
        let p = auto(&d);
        assert!(dockerfile(&p).contains(r#"CMD ["node", "src/server.js"]"#));
        assert!(p.notes.iter().any(|n| n.contains("no build script")));
        let d = repo(&[("package.json", r#"{"name":"x"}"#)]);
        assert!(plan(d.path(), AppKind::Git, &AppSource::default(), &AppBuild::default()).is_err());
        let d = repo(&[("README.md", "")]);
        let e = plan(d.path(), AppKind::Git, &AppSource::default(), &AppBuild::default()).unwrap_err();
        assert!(e.message().contains("couldn't tell how to build"), "{e}");
    }

    #[test]
    fn a_declared_package_manager_without_a_lockfile_still_installs() {
        let pkg = |pm: &str| format!(r#"{{"packageManager":"{pm}","scripts":{{"start":"node s.js"}}}}"#);
        for (pm, want) in [
            ("pnpm@9.1.0", "pnpm install'"),
            ("yarn@4.1.0", "yarn install'"),
            ("bun@1.1.0", "bun install'"),
        ] {
            let pkg = pkg(pm);
            let d = repo(&[("package.json", pkg.as_str())]);
            let df = dockerfile(&auto(&d)).to_string();
            assert!(df.contains(want), "{pm}: {df}");
            assert!(!df.contains("frozen") && !df.contains("immutable"), "{pm}: {df}");
        }
        let d = repo(&[
            ("package.json", r#"{"packageManager":"pnpm@9","scripts":{"start":"x"}}"#),
            ("pnpm-lock.yaml", ""),
        ]);
        let df = dockerfile(&auto(&d)).to_string();
        assert!(df.contains("pnpm install --frozen-lockfile") && df.contains("COREPACK_ENABLE_DOWNLOAD_PROMPT=0"));
    }

    #[test]
    fn workspaces_copy_everything_first() {
        let d = repo(&[
            (
                "package.json",
                r#"{"workspaces":["apps/*"],"scripts":{"start":"node a.js"}}"#,
            ),
            ("package-lock.json", "{}"),
        ]);
        let df = dockerfile(&auto(&d)).to_string();
        let first_copy = df.lines().find(|l| l.starts_with("COPY")).unwrap();
        assert_eq!(first_copy, "COPY . .");
    }

    #[test]
    fn python_frameworks() {
        let d = repo(&[
            ("requirements.txt", "fastapi\nuvicorn\n"),
            ("app/main.py", "from fastapi import FastAPI\napi = FastAPI()\n"),
            (".python-version", "3.11.4\n"),
        ]);
        let p = auto(&d);
        let df = dockerfile(&p);
        assert_eq!(p.framework, "FastAPI");
        assert!(df.contains("FROM python:3.11-slim"));
        assert!(df.contains("uvicorn app.main:api --host 0.0.0.0 --port $PORT"));
        assert!(!df.contains("pip install uvicorn"), "already a dependency");

        let d = repo(&[
            ("requirements.txt", "Django==5\n"),
            ("manage.py", ""),
            ("mysite/wsgi.py", ""),
        ]);
        let df = dockerfile(&auto(&d)).to_string();
        assert!(df.contains("gunicorn mysite.wsgi:application"));
        assert!(df.contains("pip install gunicorn"), "added when missing");
        assert!(df.contains("collectstatic"));

        let d = repo(&[
            ("requirements.txt", "flask"),
            ("Procfile", "web: python serve.py --port $PORT\n"),
        ]);
        assert!(dockerfile(&auto(&d)).contains("exec python serve.py --port $PORT"));
    }

    #[test]
    fn go_rust_deno_bun_ruby_php() {
        let d = repo(&[("go.mod", "module x\n\ngo 1.22.3\n"), ("main.go", "package main\n")]);
        let p = auto(&d);
        assert_eq!(p.framework, "Go 1.22");
        assert!(dockerfile(&p).contains("go build -trimpath -ldflags='-s -w' -o /out/app ."));
        let d = repo(&[("go.mod", "module x\n"), ("cmd/api/main.go", "package main")]);
        assert!(dockerfile(&auto(&d)).contains("./cmd/api"));
        let d = repo(&[("go.mod", "module x\n"), ("cmd/a/x.go", ""), ("cmd/b/x.go", "")]);
        assert!(plan(d.path(), AppKind::Git, &AppSource::default(), &AppBuild::default()).is_err());

        let d = repo(&[(
            "Cargo.toml",
            "[package]\nname = \"api-server\"\n\n[dependencies]\nname = \"x\"\n",
        )]);
        assert!(dockerfile(&auto(&d)).contains("--bin api-server"));

        let d = repo(&[("deno.json", r#"{"tasks":{"start":"deno run -A main.ts"}}"#)]);
        assert!(dockerfile(&auto(&d)).contains("exec deno task start"));

        let d = repo(&[
            ("package.json", r#"{"scripts":{"start":"bun index.ts"}}"#),
            ("bun.lockb", ""),
        ]);
        assert_eq!(auto(&d).framework, "Bun");

        let d = repo(&[("Gemfile", "gem 'rails'\n"), ("Gemfile.lock", "")]);
        let p = auto(&d);
        assert_eq!(p.framework, "Rails");
        assert!(dockerfile(&p).contains("rails server -b 0.0.0.0"));

        let d = repo(&[("composer.json", "{}"), ("artisan", ""), ("public/index.php", "")]);
        let p = auto(&d);
        assert_eq!(p.framework, "Laravel");
        assert!(dockerfile(&p).contains("SERVER_ROOT=/app/public"));
    }

    #[test]
    fn explicit_packs_contexts_images_and_compose() {
        let d = repo(&[("apps/web/index.html", "x"), ("docker-compose.yml", "services: {}")]);
        let b = AppBuild {
            context: Some("apps/web".into()),
            ..AppBuild::default()
        };
        let p = plan(d.path(), AppKind::Git, &AppSource::default(), &b).unwrap();
        assert_eq!(p.context, "apps/web");
        let bad = AppBuild {
            context: Some("../x".into()),
            ..AppBuild::default()
        };
        assert!(plan(d.path(), AppKind::Git, &AppSource::default(), &bad).is_err());
        let missing = AppBuild {
            pack: BuildPack::Dockerfile,
            ..AppBuild::default()
        };
        assert!(plan(d.path(), AppKind::Git, &AppSource::default(), &missing).is_err());

        let compose = plan(d.path(), AppKind::Compose, &AppSource::default(), &AppBuild::default()).unwrap();
        assert_eq!(compose.kind, PlanKind::Compose("docker-compose.yml".into()));
        let img = plan(
            d.path(),
            AppKind::Image,
            &AppSource {
                image: Some("nginx:1.27".into()),
                ..AppSource::default()
            },
            &AppBuild::default(),
        )
        .unwrap();
        assert_eq!(img.kind, PlanKind::Image("nginx:1.27".into()));

        let multi = AppBuild {
            build_command: Some("a\nb".into()),
            pack: BuildPack::Static,
            ..AppBuild::default()
        };
        assert!(plan(d.path(), AppKind::Git, &AppSource::default(), &multi).is_err());
    }

    #[test]
    fn build_env_file_is_shell_safe() {
        let f = build_env_file(&[("A".into(), "it's $HOME `x`".into())]);
        assert_eq!(f, "export A='it'\\''s $HOME `x`'\n");
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{f} printf %s \"$A\""))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "it's $HOME `x`");
    }
}
