//! Building an image for a deployment (design §6): fetch → plan → build.
//!
//! Builds run in BuildKit. With the `container` driver (the default on a
//! server) that is a dedicated `buildx_buildkit_vercelify0` container: the
//! power guard pauses it when the machine runs hot, which really freezes the
//! build, and it can be given a memory cap so a build can't push the apps
//! into swap. The `docker` driver uses the daemon's own BuildKit (no extra
//! container; used in tests and as a fallback).

use crate::detect::{self, BuildPlan, PlanKind};
use crate::git::{Git, GitAuth};
use crate::logs::LogWriter;
use crate::proc::{self, Cancel, Spec};
use crate::{Error, Result};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use vercelify_core::platform::{AppBuild, AppKind, AppSource};

pub const BUILDER: &str = "vercelify";
/// The builder's container, as the power guard sees it.
pub const BUILDER_CONTAINER: &str = "buildx_buildkit_vercelify0";
pub const BUILDKIT_IMAGE: &str = "moby/buildkit:v0.18.2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    /// Dedicated BuildKit container (pausable, memory-capped).
    Container,
    /// The Docker daemon's BuildKit.
    Docker,
}

impl Driver {
    pub fn parse(s: &str) -> Driver {
        if s == "docker" {
            Driver::Docker
        } else {
            Driver::Container
        }
    }
}

#[derive(Debug, Clone)]
pub struct Builder {
    pub docker: String,
    pub git: String,
    /// Where sources are checked out (`<dir>/<deployment>`), removed after.
    pub work_dir: PathBuf,
    pub driver: Driver,
    /// Memory cap of the BuildKit container, e.g. "3g".
    pub memory: Option<String>,
    pub timeout: Duration,
}

pub struct BuildRequest<'a> {
    pub app_name: &'a str,
    pub deployment_id: &'a str,
    pub kind: AppKind,
    pub source: &'a AppSource,
    pub build: &'a AppBuild,
    /// Commit or branch to build.
    pub rev: &'a str,
    pub auth: GitAuth,
    /// Variables marked "build" (resolved and decrypted).
    pub build_env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Built {
    /// Empty for compose apps (their images are built by `docker compose`).
    pub image: String,
    pub commit_sha: String,
    pub commit_message: String,
    pub plan: BuildPlan,
    /// Compose apps: the checkout, kept for the deployer, which removes it.
    pub checkout: Option<PathBuf>,
}

/// The tag of a deployment's image.
pub fn image_tag(app: &str, deployment: &str) -> String {
    format!(
        "vfy/{app}:{}",
        deployment.trim_start_matches("dpl_").to_ascii_lowercase()
    )
}

/// A file only root can read, removed when dropped.
struct PrivateFile(PathBuf);

impl PrivateFile {
    fn write(path: PathBuf, content: &[u8]) -> Result<Self> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(content)?;
        Ok(Self(path))
    }
}

impl Drop for PrivateFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Removes a work directory when the build ends, however it ends.
struct WorkDir {
    path: PathBuf,
    keep: bool,
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

pub struct DirBuild<'a> {
    pub dir: &'a Path,
    pub app_name: &'a str,
    pub deployment_id: &'a str,
    pub build: &'a AppBuild,
    pub build_env: &'a [(String, String)],
}

struct BuildxArgs<'a> {
    image: &'a str,
    dockerfile: &'a Path,
    context: &'a Path,
    labels: Vec<String>,
    env_file: &'a Path,
    /// Names only: values come from the environment.
    build_args: Vec<String>,
    env: Vec<(String, String)>,
    pull: bool,
}

impl Builder {
    fn spec<'a>(&'a self, args: &'a [String], timeout: Duration) -> Spec<'a> {
        Spec {
            bin: &self.docker,
            args,
            env: &[],
            cwd: None,
            timeout,
        }
    }

    /// Create the BuildKit container builder if it doesn't exist.
    pub async fn ensure_builder(&self, log: &mut LogWriter) -> Result<()> {
        if self.driver == Driver::Docker {
            return Ok(());
        }
        let inspect = proc::args(&["buildx", "inspect", BUILDER]);
        if proc::run(self.spec(&inspect, Duration::from_secs(30))).await?.success() {
            return Ok(());
        }
        log.info("creating the BuildKit builder (first build on this server)");
        let mut opts = format!("image={BUILDKIT_IMAGE}");
        if let Some(m) = &self.memory {
            opts.push_str(&format!(",memory={m}"));
        }
        let create = proc::args(&[
            "buildx",
            "create",
            "--name",
            BUILDER,
            "--driver",
            "docker-container",
            "--driver-opt",
            &opts,
            "--bootstrap",
        ]);
        let o = proc::run(self.spec(&create, Duration::from_secs(300))).await?;
        if !o.success() {
            return Err(Error::Unavailable(format!(
                "could not create the BuildKit builder: {}",
                o.tail(3)
            )));
        }
        Ok(())
    }

    async fn buildx(&self, a: BuildxArgs<'_>, cancel: Cancel, log: &mut LogWriter) -> Result<()> {
        self.ensure_builder(log).await?;
        let mut args = proc::args(&["buildx", "build"]);
        if self.driver == Driver::Container {
            args.extend(proc::args(&["--builder", BUILDER]));
        }
        args.extend(proc::args(&["--load", "--progress", "plain"]));
        if a.pull {
            args.push("--pull".into());
        }
        args.extend(proc::args(&[
            "--tag",
            a.image,
            "--file",
            &a.dockerfile.to_string_lossy(),
            "--secret",
            &format!("id=env,src={}", a.env_file.display()),
        ]));
        for l in &a.labels {
            args.extend(["--label".to_string(), l.clone()]);
        }
        for k in &a.build_args {
            args.extend(["--build-arg".to_string(), k.clone()]);
        }
        args.push(a.context.to_string_lossy().into_owned());
        log.info(&format!("building {}", a.image));
        let out = proc::stream(
            Spec {
                bin: &self.docker,
                args: &args,
                env: &a.env,
                cwd: None,
                timeout: self.timeout,
            },
            cancel,
            &mut |s, l| log.line(s, l),
        )
        .await?;
        if !out.success() {
            return Err(Error::Invalid(format!("the build failed:\n{}", out.tail(15))));
        }
        Ok(())
    }

    /// Write the plan's Dockerfile (if generated) and return its path.
    fn dockerfile_for(plan: &BuildPlan, context: &Path, work: &Path, log: &mut LogWriter) -> Result<PathBuf> {
        match &plan.kind {
            PlanKind::Dockerfile(p) => Ok(context.join(p)),
            PlanKind::Generated(df) => {
                log.info("generated Dockerfile:");
                for l in df.lines() {
                    log.line("info", &format!("  {l}"));
                }
                let p = work.join("Dockerfile.vercelify");
                std::fs::write(&p, df)?;
                Ok(p)
            }
            other => Err(Error::Internal(format!("no Dockerfile for {other:?}"))),
        }
    }

    pub async fn build(&self, req: BuildRequest<'_>, cancel: Cancel, log: &mut LogWriter) -> Result<Built> {
        let work = self.work_dir.join(req.deployment_id);
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        let mut cleanup = WorkDir {
            path: work.clone(),
            keep: false,
        };
        let src = work.join("src");

        // Image apps: nothing to fetch or build; the deployer pulls.
        if req.kind == AppKind::Image {
            let plan = detect::plan(&work, req.kind, req.source, req.build)?;
            let PlanKind::Image(image) = &plan.kind else {
                return Err(Error::Internal("image app without an image".into()));
            };
            return Ok(Built {
                image: image.clone(),
                commit_sha: String::new(),
                commit_message: String::new(),
                plan,
                checkout: None,
            });
        }

        let repo = req
            .source
            .repository
            .as_deref()
            .ok_or_else(|| Error::Invalid("the app has no repository".into()))?;
        let git = Git {
            bin: self.git.clone(),
            timeout: Duration::from_secs(600),
        };
        let fetched = git
            .fetch(repo, req.rev, &src, &req.auth, cancel.clone(), &mut |s, l| {
                log.line(s, l)
            })
            .await?;
        log.info(&format!(
            "commit {} {}",
            &fetched.sha[..fetched.sha.len().min(12)],
            fetched.message
        ));

        let plan = detect::plan(&src, req.kind, req.source, req.build)?;
        log.info(&format!("detected: {}", plan.framework));
        for n in &plan.notes {
            log.info(&format!("note: {n}"));
        }
        if let PlanKind::Compose(_) = plan.kind {
            cleanup.keep = true;
            return Ok(Built {
                image: String::new(),
                commit_sha: fetched.sha,
                commit_message: fetched.message,
                plan,
                checkout: Some(src),
            });
        }

        let context = if plan.context == "." {
            src.clone()
        } else {
            src.join(&plan.context)
        };
        let dockerfile = Self::dockerfile_for(&plan, &context, &work, log)?;
        let env_file = PrivateFile::write(
            work.join("build.env"),
            detect::build_env_file(&req.build_env).as_bytes(),
        )?;
        let image = image_tag(req.app_name, req.deployment_id);
        // A repository's own Dockerfile reads build-time variables as ARGs;
        // `--build-arg KEY` takes the value from the environment.
        let build_args = if matches!(plan.kind, PlanKind::Dockerfile(_)) {
            req.build_env.iter().map(|(k, _)| k.clone()).collect()
        } else {
            vec![]
        };
        self.buildx(
            BuildxArgs {
                image: &image,
                dockerfile: &dockerfile,
                context: &context,
                labels: vec![
                    format!("vercelify.app={}", req.app_name),
                    format!("vercelify.deployment={}", req.deployment_id),
                    format!("vercelify.commit={}", fetched.sha),
                ],
                env_file: &env_file.0,
                build_args,
                env: req.build_env.clone(),
                pull: true,
            },
            cancel,
            log,
        )
        .await?;
        Ok(Built {
            image,
            commit_sha: fetched.sha,
            commit_message: fetched.message,
            plan,
            checkout: None,
        })
    }

    /// Build from a directory already on disk (uploads from `vfy deploy`,
    /// tests). The directory is left as it is.
    pub async fn build_dir(&self, req: DirBuild<'_>, cancel: Cancel, log: &mut LogWriter) -> Result<Built> {
        let DirBuild {
            dir,
            app_name,
            deployment_id,
            build,
            build_env,
        } = req;
        let plan = detect::plan(dir, AppKind::Git, &AppSource::default(), build)?;
        log.info(&format!("detected: {}", plan.framework));
        let context = if plan.context == "." {
            dir.to_path_buf()
        } else {
            dir.join(&plan.context)
        };
        let work = self.work_dir.join(format!("{deployment_id}-dir"));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        let _cleanup = WorkDir {
            path: work.clone(),
            keep: false,
        };
        let dockerfile = Self::dockerfile_for(&plan, &context, &work, log)?;
        let env_file = PrivateFile::write(work.join("build.env"), detect::build_env_file(build_env).as_bytes())?;
        let image = image_tag(app_name, deployment_id);
        self.buildx(
            BuildxArgs {
                image: &image,
                dockerfile: &dockerfile,
                context: &context,
                labels: vec![
                    format!("vercelify.app={app_name}"),
                    format!("vercelify.deployment={deployment_id}"),
                ],
                env_file: &env_file.0,
                build_args: vec![],
                env: vec![],
                pull: false,
            },
            cancel,
            log,
        )
        .await?;
        Ok(Built {
            image,
            commit_sha: String::new(),
            commit_message: String::new(),
            plan,
            checkout: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::LogHub;

    #[test]
    fn tags() {
        assert_eq!(image_tag("blog", "dpl_7k2m9q4w1x8z3c5v"), "vfy/blog:7k2m9q4w1x8z3c5v");
        assert_eq!(Driver::parse("docker"), Driver::Docker);
        assert_eq!(Driver::parse(""), Driver::Container);
    }

    /// Real builds with the daemon's BuildKit. Opt in with
    /// `VERCELIFY_DOCKER_TESTS=1` (needs a running Docker daemon). Fixtures
    /// need no network inside the build except the base image pull.
    #[tokio::test]
    async fn builds_fixture_apps_with_docker() {
        if std::env::var("VERCELIFY_DOCKER_TESTS").as_deref() != Ok("1") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let logs = LogHub::new(dir.path().join("logs"));
        let b = Builder {
            docker: "docker".into(),
            git: "git".into(),
            work_dir: dir.path().join("work"),
            driver: Driver::Docker,
            memory: None,
            timeout: Duration::from_secs(900),
        };
        let fixtures: &[(&str, &[(&str, &str)])] = &[
            ("static", &[("index.html", "<h1>static ok</h1>"), ("about.html", "about")]),
            (
                "node",
                &[
                    (
                        "package.json",
                        r#"{"name":"n","scripts":{"build":"node build.js","start":"node server.js"}}"#,
                    ),
                    ("package-lock.json", r#"{"name":"n","lockfileVersion":3,"requires":true,"packages":{"":{"name":"n"}}}"#),
                    ("build.js", "require('fs').writeFileSync('built.txt', process.env.GREETING || 'none')"),
                    (
                        "server.js",
                        "require('http').createServer((q,s)=>s.end(require('fs').readFileSync('built.txt'))).listen(process.env.PORT||3000)",
                    ),
                ],
            ),
        ];
        for (name, files) in fixtures {
            let src = dir.path().join(name);
            for (p, c) in *files {
                std::fs::create_dir_all(src.join(p).parent().unwrap()).unwrap();
                std::fs::write(src.join(p), c).unwrap();
            }
            let dpl = format!("dpl_test{name}");
            let mut log = logs.writer(name, &dpl);
            let built = b
                .build_dir(
                    DirBuild {
                        dir: &src,
                        app_name: &format!("fixture-{name}"),
                        deployment_id: &dpl,
                        build: &AppBuild::default(),
                        build_env: &[("GREETING".into(), "hello from the build".into())],
                    },
                    Cancel::never(),
                    &mut log,
                )
                .await
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            drop(log);
            let inspect = std::process::Command::new("docker")
                .args([
                    "image",
                    "inspect",
                    "-f",
                    "{{index .Config.Labels \"vercelify.app\"}}",
                    &built.image,
                ])
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&inspect.stdout).trim(),
                format!("fixture-{name}")
            );
            if *name == "node" {
                // The build-time variable reached `npm run build` …
                let out = std::process::Command::new("docker")
                    .args(["run", "--rm", "--entrypoint", "cat", &built.image, "/app/built.txt"])
                    .output()
                    .unwrap();
                assert_eq!(String::from_utf8_lossy(&out.stdout), "hello from the build");
                // … but not the image's history or environment.
                let hist = std::process::Command::new("docker")
                    .args(["history", "--no-trunc", &built.image])
                    .output()
                    .unwrap();
                assert!(!String::from_utf8_lossy(&hist.stdout).contains("hello from the build"));
            }
            let _ = std::process::Command::new("docker")
                .args(["rmi", "-f", &built.image])
                .output();
        }
    }
}
