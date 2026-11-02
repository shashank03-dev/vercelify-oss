//! Shared detector/build helper for explicitly selected developer computers/CI.
use serde::Deserialize;
use std::{
    io::Read,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use vercelify_core::platform::{AppBuild, AppKind, AppSource, LocalBuildPlan};
use vercelify_platform::{
    build::{self, Builder, DirBuild, Driver},
    detect::{self, PlanKind},
    logs::{LogHub, Redactor},
    proc, Error, Result,
};
#[derive(Deserialize)]
struct Variable {
    key: String,
    value: String,
    secret: bool,
}
#[derive(Deserialize)]
struct Request {
    build: AppBuild,
    variables: Vec<Variable>,
    app_name: String,
    deployment_id: String,
    platform: String,
}

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let outcome = run(&args).await;
    if let Err(e) = outcome {
        eprintln!("vercelify-pack: {e}");
        std::process::exit(1);
    }
}
async fn run(args: &[String]) -> Result<()> {
    if args.len() != 2 || !matches!(args[0].as_str(), "plan" | "build") {
        return Err(Error::Invalid(
            "usage: vercelify-pack plan|build CHECKOUT (JSON on stdin)".into(),
        ));
    }
    let mut input = String::new();
    std::io::stdin().take(2 * 1024 * 1024).read_to_string(&mut input)?;
    let dir = Path::new(&args[1]);
    if args[0] == "plan" {
        let config: AppBuild = serde_json::from_str(&input)?;
        let plan = detect::plan(dir, AppKind::Git, &AppSource::default(), &config)?;
        let context = detect::context_dir(dir, &plan)?;
        let (dockerfile, generated) = match &plan.kind {
            PlanKind::Generated(df) => (build::local_dockerfile(df), true),
            PlanKind::Dockerfile(path) => {
                let path = context.join(path).canonicalize()?;
                if !path.starts_with(context.canonicalize()?) {
                    return Err(Error::Invalid("Dockerfile escapes the checkout".into()));
                }
                (std::fs::read_to_string(path)?, false)
            }
            _ => {
                return Err(Error::Invalid(
                    "explicit local builds support Dockerfile and detected Git apps; Compose remains separate".into(),
                ))
            }
        };
        let plan = LocalBuildPlan {
            dockerfile,
            generated,
            context: plan.context,
            port: plan.port,
            framework: plan.framework,
            notes: plan.notes,
            project_files: build::read_project_files(&context),
        };
        println!("{}", serde_json::to_string(&plan)?);
        return Ok(());
    }
    let request: Request = serde_json::from_str(&input)?;
    if !matches!(request.platform.as_str(), "linux/arm64" | "linux/amd64") {
        return Err(Error::Invalid("explicit Linux target platform required".into()));
    }
    if request.app_name.is_empty()
        || !request.app_name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || !request.deployment_id.starts_with("dpl_")
        || !request
            .deployment_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(Error::Invalid("invalid app or deployment identifier".into()));
    }
    let work = tempfile::tempdir()?;
    let hub = LogHub::new(work.path().join("logs"));
    let mut log = hub.writer(&request.app_name, &request.deployment_id);
    log.redact(Redactor::new(
        request.variables.iter().filter(|v| v.secret).map(|v| v.value.as_str()),
    ));
    let env: Vec<_> = request
        .variables
        .iter()
        .map(|v| (v.key.clone(), v.value.clone()))
        .collect();
    let public: Vec<_> = request
        .variables
        .iter()
        .filter(|v| !v.secret)
        .map(|v| v.key.clone())
        .collect();
    let builder = Builder {
        docker: "docker".into(),
        git: "git".into(),
        work_dir: work.path().join("builds"),
        driver: Driver::Docker,
        memory: None,
        timeout: std::time::Duration::from_secs(45 * 60),
    };
    let (cancel_handle, cancel) = proc::cancel_pair();
    let signal = tokio::spawn(async move {
        #[cfg(unix)]
        if let Ok(mut terminate) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            tokio::select! {_ = tokio::signal::ctrl_c()=>{}, _ = terminate.recv()=>{}}
            cancel_handle.cancel();
            return;
        }
        let _ = tokio::signal::ctrl_c().await;
        cancel_handle.cancel();
    });
    let seen = Arc::new(AtomicU64::new(0));
    let progress = seen.clone();
    let mut receiver = hub.follow(&request.deployment_id).expect("writer registered");
    let relay = tokio::spawn(async move {
        while let Ok(line) = receiver.recv().await {
            eprintln!("{}", line.text);
            progress.store(line.seq + 1, Ordering::Release);
        }
    });
    let result = builder
        .build_dir_for(
            DirBuild {
                dir,
                app_name: &request.app_name,
                deployment_id: &request.deployment_id,
                build: &request.build,
                build_env: &env,
            },
            Some(request.platform),
            &public,
            true,
            cancel,
            &mut log,
        )
        .await;
    signal.abort();
    relay.abort();
    let _ = relay.await;
    let seen = seen.load(Ordering::Acquire);
    for line in hub.read(&request.app_name, &request.deployment_id, None) {
        if line.seq >= seen {
            eprintln!("{}", line.text);
        }
    }
    match result {
        Ok(built) => {
            println!("{}", serde_json::json!({"image":built.image}));
            Ok(())
        }
        Err(e) => Err(Error::Invalid(log.redact_text(e.message()))),
    }
}
