use std::{
    io::Write,
    process::{Command, Stdio},
};
use vercelify_core::platform::LocalBuildPlan;
#[test]
fn shared_pack_plans_a_real_checkout_without_running_repository_code() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "hello").unwrap();
    std::fs::write(dir.path().join("vercel.json"), r#"{"cleanUrls":true}"#).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vercelify-pack"))
        .args(["plan", dir.path().to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let plan: LocalBuildPlan = serde_json::from_slice(&output.stdout).unwrap();
    assert!(plan.generated);
    assert!(plan.dockerfile.contains("caddy"));
    assert!(plan.dockerfile.contains("ARG VERCELIFY_BUILD_JOB"));
    assert_eq!(plan.port, 8080);
    assert_eq!(
        plan.project_files,
        vec![("vercel.json".into(), r#"{"cleanUrls":true}"#.into())]
    );
}
