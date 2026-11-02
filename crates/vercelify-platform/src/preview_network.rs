//! Preview-only, ingress-only networks. Docker 28+ isolated gateway mode
//! removes the host bridge address as well as the default route. A preview
//! never joins either the production project bridge or the shared proxy bridge.
use crate::docker::{Docker, MANAGED, MANAGED_LABEL};
use crate::proc;
use crate::{Error, Result};
use serde_json::Value;
use std::time::Duration;

pub const PREFIX: &str = "vfy-preview-";
const LABEL: &str = "vercelify.preview-network";
const MODE: &str = "com.docker.network.bridge.gateway_mode_ipv4";

pub fn name(app: &str, pr: u32) -> String {
    format!("{PREFIX}{app}-pr{pr}")
}

fn isolated(v: &Value) -> bool {
    v["Driver"] == "bridge"
        && v["Internal"] == true
        && v["EnableIPv6"] == false
        && v["Options"][MODE] == "isolated"
        && v["Labels"][LABEL] == "true"
        && v["Labels"][MANAGED_LABEL] == MANAGED
}

pub async fn ensure(docker: &Docker, network: &str) -> Result<()> {
    if !network.starts_with(PREFIX) {
        return Err(Error::Invalid("invalid preview network name".into()));
    }
    let args = proc::args(&["network", "inspect", "--format", "{{json .}}", network]);
    let mut inspected = docker.run(&args, Duration::from_secs(30)).await?;
    if !inspected.success() {
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
            return Err(Error::Invalid(format!(
                "cannot isolate preview network (Docker 28+ is required): {}",
                created.tail(2)
            )));
        }
        inspected = docker.run(&args, Duration::from_secs(30)).await?;
    }
    let value: Value = serde_json::from_str(inspected.stdout.trim()).unwrap_or_default();
    if !inspected.success() || !isolated(&value) {
        return Err(Error::Invalid(format!(
            "{network} is not an owned isolated preview network; refusing to start preview code"
        )));
    }
    connect_proxy(docker, network).await
}

pub async fn connect_proxy(docker: &Docker, network: &str) -> Result<()> {
    if docker
        .inspect(crate::proxy::CONTAINER)
        .await?
        .is_some_and(|i| i.ips.contains_key(network))
    {
        return Ok(());
    }
    let o = docker
        .run(
            &proc::args(&["network", "connect", network, crate::proxy::CONTAINER]),
            Duration::from_secs(30),
        )
        .await?;
    if o.success() || o.tail(2).contains("already exists") {
        Ok(())
    } else {
        Err(Error::Internal(format!("connect preview ingress: {}", o.tail(2))))
    }
}

/// A recreated proxy must rejoin existing private bridges before serving.
pub async fn reconnect(docker: &Docker) -> Result<()> {
    let o = docker
        .run(
            &proc::args(&[
                "network",
                "ls",
                "--filter",
                &format!("label={LABEL}=true"),
                "--format",
                "{{.Name}}",
            ]),
            Duration::from_secs(30),
        )
        .await?;
    if !o.success() {
        return Err(Error::Internal(format!("list preview networks: {}", o.tail(2))));
    }
    for network in o.stdout.lines().map(str::trim).filter(|n| !n.is_empty()) {
        // A close may remove the network after the list: never recreate it
        // during recovery, or a finished preview would leak a fresh bridge.
        let inspected = docker
            .run(
                &proc::args(&["network", "inspect", "--format", "{{json .}}", network]),
                Duration::from_secs(30),
            )
            .await?;
        if !inspected.success() {
            if inspected.tail(2).contains("not found") || inspected.tail(2).contains("No such network") {
                continue;
            }
            return Err(Error::Internal(format!(
                "inspect preview network: {}",
                inspected.tail(2)
            )));
        }
        if !isolated(&serde_json::from_str::<Value>(&inspected.stdout).unwrap_or_default()) {
            return Err(Error::Invalid(format!("refusing unsafe preview network {network}")));
        }
        connect_proxy(docker, network).await?;
    }
    Ok(())
}

/// Remove a failed attempt's bridge only if no workload uses it. A previous
/// healthy deployment of the same PR keeps its network during a failed rollout.
pub async fn remove_if_unused(docker: &Docker, network: &str) -> Result<()> {
    let inspected = docker
        .run(
            &proc::args(&["network", "inspect", "--format", "{{json .}}", network]),
            Duration::from_secs(30),
        )
        .await?;
    if !inspected.success() {
        return Ok(());
    }
    let v: Value = serde_json::from_str(&inspected.stdout)?;
    if v["Containers"]
        .as_object()
        .is_some_and(|c| c.values().all(|c| c["Name"] == crate::proxy::CONTAINER))
    {
        remove(docker, network).await?;
    }
    Ok(())
}

/// App deletion still knows the app ID after its SQL preview rows cascade.
pub async fn remove_for_app(docker: &Docker, app: &str) -> Result<()> {
    let prefix = format!("{PREFIX}{app}-pr");
    let out = docker
        .run(
            &proc::args(&[
                "network",
                "ls",
                "--filter",
                &format!("label={LABEL}=true"),
                "--format",
                "{{.Name}}",
            ]),
            Duration::from_secs(30),
        )
        .await?;
    if !out.success() {
        return Err(Error::Internal(format!("list preview networks: {}", out.tail(2))));
    }
    for net in out.stdout.lines().map(str::trim).filter(|n| n.starts_with(&prefix)) {
        remove(docker, net).await?;
    }
    Ok(())
}

pub async fn remove(docker: &Docker, network: &str) -> Result<()> {
    if !network.starts_with(PREFIX) {
        return Err(Error::Invalid("refusing to remove a non-preview network".into()));
    }
    let check = docker
        .run(
            &proc::args(&["network", "inspect", "--format", "{{json .}}", network]),
            Duration::from_secs(30),
        )
        .await?;
    if !check.success() {
        if check.tail(2).contains("not found") || check.tail(2).contains("No such network") {
            return Ok(());
        }
        return Err(Error::Internal(format!("inspect preview network: {}", check.tail(2))));
    }
    if !isolated(&serde_json::from_str::<Value>(&check.stdout).unwrap_or_default()) {
        return Err(Error::Invalid(
            "refusing to remove a network not owned by previews".into(),
        ));
    }
    let _ = docker
        .run(
            &proc::args(&["network", "disconnect", "--force", network, crate::proxy::CONTAINER]),
            Duration::from_secs(30),
        )
        .await?;
    let out = docker
        .run(&proc::args(&["network", "rm", network]), Duration::from_secs(30))
        .await?;
    if out.success() {
        Ok(())
    } else {
        Err(Error::Internal(format!("remove preview network: {}", out.tail(2))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolation_requires_every_boundary() {
        let safe = serde_json::json!({"Driver":"bridge","Internal":true,"EnableIPv6":false,
            "Options":{MODE:"isolated"},"Labels":{LABEL:"true",MANAGED_LABEL:MANAGED}});
        assert!(isolated(&safe));
        for key in ["Driver", "Internal", "EnableIPv6", "Options", "Labels"] {
            let mut broken = safe.clone();
            broken.as_object_mut().unwrap().remove(key);
            assert!(!isolated(&broken), "{key}");
        }
        let mut normal = safe.clone();
        normal["Options"][MODE] = "nat".into();
        assert!(!isolated(&normal));
        assert_ne!(name("app_1", 1), name("app_1", 2));
        assert_ne!(name("app_1", 1), name("app_2", 1));
    }
}
