//! Restore destinations have no ingress, including from the platform proxy.
//! A separate label prevents preview-network recovery from joining them.
use crate::{
    docker::{Docker, MANAGED, MANAGED_LABEL},
    proc, Error, Result,
};
use serde_json::Value;
use std::time::Duration;
const LABEL: &str = "vercelify.restore-network";
const MODE: &str = "com.docker.network.bridge.gateway_mode_ipv4";
fn valid(v: &Value) -> bool {
    v["Driver"] == "bridge"
        && v["Internal"] == true
        && v["EnableIPv6"] == false
        && v["Options"][MODE] == "isolated"
        && v["Labels"][LABEL] == "true"
        && v["Labels"][MANAGED_LABEL] == MANAGED
        && v["Labels"]["vercelify.preview-network"].is_null()
}
pub async fn ensure(docker: &Docker, network: &str) -> Result<()> {
    if !network.starts_with("vfy-preview-drill-task_") {
        return Err(Error::Invalid("invalid restore network identity".into()));
    }
    let args = proc::args(&["network", "inspect", "--format", "{{json .}}", network]);
    let mut out = docker.run(&args, Duration::from_secs(30)).await?;
    if !out.success() {
        let created = docker
            .run(
                &proc::args(&[
                    "network",
                    "create",
                    "--internal",
                    "--driver",
                    "bridge",
                    "--opt",
                    &format!("{MODE}=isolated"),
                    "--label",
                    &format!("{MANAGED_LABEL}={MANAGED}"),
                    "--label",
                    &format!("{LABEL}=true"),
                    network,
                ]),
                Duration::from_secs(30),
            )
            .await?;
        if !created.success() && !created.tail(2).contains("already exists") {
            return Err(Error::Unavailable(
                "cannot create isolated restore network (Docker 28+ required)".into(),
            ));
        }
        out = docker.run(&args, Duration::from_secs(30)).await?;
    }
    if !out.success() || !valid(&serde_json::from_str(&out.stdout).unwrap_or_default()) {
        return Err(Error::Invalid("restore network is not an owned isolated bridge".into()));
    }
    let v: Value = serde_json::from_str(&out.stdout)?;
    if v["Containers"]
        .as_object()
        .is_some_and(|c| c.values().any(|v| v["Name"] == crate::proxy::CONTAINER))
    {
        return Err(Error::Invalid(
            "proxy attached to restore quarantine; refusing restore".into(),
        ));
    }
    Ok(())
}
pub async fn remove(docker: &Docker, network: &str) -> Result<()> {
    if !network.starts_with("vfy-preview-drill-task_") {
        return Err(Error::Invalid("invalid restore network identity".into()));
    }
    let out = docker
        .run(
            &proc::args(&["network", "inspect", "--format", "{{json .}}", network]),
            Duration::from_secs(30),
        )
        .await?;
    if !out.success() {
        if out.tail(2).contains("not found") || out.tail(2).contains("No such network") {
            return Ok(());
        }
        return Err(Error::Unavailable("restore network inspection failed".into()));
    }
    if !valid(&serde_json::from_str(&out.stdout).unwrap_or_default()) {
        return Err(Error::Invalid("refusing removal of unowned restore network".into()));
    }
    let out = docker
        .run(&proc::args(&["network", "rm", network]), Duration::from_secs(30))
        .await?;
    if !out.success() {
        return Err(Error::Unavailable("restore network cleanup pending".into()));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_bridges_cannot_be_adopted_as_preview_ingress() {
        let safe = serde_json::json!({"Driver":"bridge","Internal":true,"EnableIPv6":false,"Options":{MODE:"isolated"},"Labels":{LABEL:"true",MANAGED_LABEL:MANAGED}});
        assert!(valid(&safe));
        let mut ingress = safe.clone();
        ingress["Labels"]["vercelify.preview-network"] = "true".into();
        assert!(!valid(&ingress));
        for key in ["Internal", "Options", "Labels"] {
            let mut bad = safe.clone();
            bad.as_object_mut().unwrap().remove(key);
            assert!(!valid(&bad));
        }
    }
}
