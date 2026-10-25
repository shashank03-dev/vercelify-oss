//! The image registry on the mesh (design §6.2 "push", §8.2): the main
//! server builds, pushes to `vfy-registry` (a pinned `registry:2` bound to
//! its loopback and mesh addresses), and workers pull from it.
//!
//! Every server names it `127.0.0.1:5055`: Docker trusts registries on the
//! loopback without TLS, so no server's `daemon.json` changes and Docker
//! never restarts for it. On the main server that is the registry itself;
//! on a worker the agent forwards the port to the main server over the
//! mesh (WireGuard already encrypts it).

use crate::docker::{ContainerSpec, Docker, Mount, MANAGED_LABEL};
use crate::proc::{self, Spec};
use crate::{Error, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use std::path::Path;
use std::time::Duration;

pub const CONTAINER: &str = "vfy-registry";
pub const IMAGE: &str = "registry:2";
pub const PORT: u16 = 5055;
/// What every server calls the registry.
pub const LOCAL: &str = "127.0.0.1:5055";

/// `vfy/blog:abc` → `127.0.0.1:5055/vfy/blog:abc`.
pub fn reference(image: &str) -> String {
    format!("{LOCAL}/{image}")
}

/// Docker's name for a machine architecture (`uname -m`).
pub fn docker_arch(arch: &str) -> &str {
    match arch {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        "armv7l" | "armv7" | "arm" => "arm/v7",
        other => other,
    }
}

/// The tag of a deployment's image built for `arch` on a `native` builder:
/// the native build keeps the plain tag, others get `-<arch>`.
pub fn arch_tag(image: &str, arch: &str, native: &str) -> String {
    let (a, n) = (docker_arch(arch), docker_arch(native));
    if a == n {
        image.to_string()
    } else {
        format!("{image}-{}", a.replace('/', ""))
    }
}

/// Start (or keep) the registry, published on 127.0.0.1 and `binds`
/// (the mesh address). Recreated when the addresses change.
pub async fn ensure(docker: &Docker, dir: &Path, binds: &[String]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut publish = vec![format!("127.0.0.1:{PORT}:5000")];
    for b in binds.iter().filter(|b| !b.is_empty() && b.as_str() != "127.0.0.1") {
        publish.push(format!("{b}:{PORT}:5000"));
    }
    let hash = format!("{}|{}", publish.join(","), dir.display());
    if let Some(i) = docker.inspect(CONTAINER).await? {
        if docker.label(CONTAINER, "vercelify.publish").await.unwrap_or_default() == hash {
            if !i.running {
                docker.start(CONTAINER).await?;
            }
            return Ok(());
        }
        docker.remove(CONTAINER).await?;
    }
    if !docker.image_exists(IMAGE).await? {
        docker.pull(IMAGE).await?;
    }
    docker
        .create(&ContainerSpec {
            name: CONTAINER.into(),
            image: IMAGE.into(),
            labels: [
                (MANAGED_LABEL.to_string(), "registry".to_string()),
                ("vercelify.kind".to_string(), "core".to_string()),
                ("vercelify.publish".to_string(), hash),
            ]
            .into(),
            env: vec![("REGISTRY_STORAGE_DELETE_ENABLED".into(), "true".into())],
            mounts: vec![Mount {
                source: dir.to_string_lossy().into(),
                target: "/var/lib/registry".into(),
                read_only: false,
            }],
            publish,
            memory_mb: Some(128),
            restart: "unless-stopped".into(),
            ..ContainerSpec::default()
        })
        .await?;
    docker.start(CONTAINER).await
}

/// Have `image` (from its own registry) here as `tag`, for `arch`: pulled
/// for that platform when it isn't this server's, or when it is missing.
pub async fn stage(
    docker: &Docker,
    image: &str,
    arch: &str,
    native: &str,
    tag: &str,
    log: &mut (dyn FnMut(&str) + Send),
) -> Result<()> {
    if docker.image_exists(tag).await? {
        return Ok(());
    }
    let foreign = docker_arch(arch) != docker_arch(native);
    let had_native = docker.image_exists(image).await?;
    if foreign || !had_native {
        log(&format!("pulling {image} for linux/{}", docker_arch(arch)));
        let args = proc::args(&[
            "pull",
            "--quiet",
            "--platform",
            &format!("linux/{}", docker_arch(arch)),
            image,
        ]);
        let o = docker.run(&args, Duration::from_secs(900)).await?;
        if !o.success() {
            return Err(Error::Unavailable(format!("docker pull {image}: {}", o.tail(3))));
        }
    }
    let o = docker
        .run(&proc::args(&["tag", image, tag]), Duration::from_secs(30))
        .await?;
    if !o.success() {
        return Err(Error::Internal(format!("docker tag {image}: {}", o.tail(2))));
    }
    // The foreign pull took the name: give it back to this server's own.
    if foreign && had_native {
        let args = proc::args(&[
            "pull",
            "--quiet",
            "--platform",
            &format!("linux/{}", docker_arch(native)),
            image,
        ]);
        let _ = docker.run(&args, Duration::from_secs(900)).await?;
    }
    Ok(())
}

/// Tag `image` for the registry and push it. Returns the registry reference.
pub async fn push(docker: &Docker, image: &str, log: &mut (dyn FnMut(&str) + Send)) -> Result<String> {
    let target = reference(image);
    let tag = proc::args(&["tag", image, &target]);
    let o = docker.run(&tag, Duration::from_secs(30)).await?;
    if !o.success() {
        return Err(Error::Internal(format!("docker tag {image}: {}", o.tail(2))));
    }
    log(&format!("pushing {target} for the other servers"));
    let args = proc::args(&["push", "--quiet", &target]);
    let o = proc::run(Spec {
        bin: &docker.bin,
        args: &args,
        env: &[],
        cwd: None,
        timeout: Duration::from_secs(1800),
    })
    .await?;
    if !o.success() {
        let tail = o.tail(3);
        return Err(Error::Unavailable(if tail.contains("connection refused") {
            format!("the image registry ({CONTAINER}) isn't running: {tail}")
        } else {
            format!("pushing {target}: {tail}")
        }));
    }
    // The local registry tag only costs a name; drop it.
    let _ = docker
        .run(&proc::args(&["image", "rm", &target]), Duration::from_secs(30))
        .await;
    Ok(target)
}

async fn http(method: Method, path: &str, accept: Option<&str>) -> Result<(u16, hyper::HeaderMap, Bytes)> {
    let fut = async {
        let tcp = tokio::net::TcpStream::connect(LOCAL)
            .await
            .map_err(|e| Error::Unavailable(format!("the image registry: {e}")))?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
            .await
            .map_err(|e| Error::Unavailable(e.to_string()))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, LOCAL);
        if let Some(a) = accept {
            b = b.header(hyper::header::ACCEPT, a);
        }
        let req = b
            .body(Full::new(Bytes::new()))
            .map_err(|e| Error::Internal(e.to_string()))?;
        let resp = sender
            .send_request(req)
            .await
            .map_err(|e| Error::Unavailable(e.to_string()))?;
        let (parts, body) = resp.into_parts();
        let bytes = body
            .collect()
            .await
            .map_err(|e| Error::Unavailable(e.to_string()))?
            .to_bytes();
        Ok((parts.status.as_u16(), parts.headers, bytes))
    };
    tokio::time::timeout(Duration::from_secs(30), fut)
        .await
        .map_err(|_| Error::Unavailable("the image registry didn't answer".into()))?
}

const MANIFESTS: &str = "application/vnd.docker.distribution.manifest.v2+json, \
application/vnd.oci.image.manifest.v1+json, application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json";

/// Wait until the registry answers (it was just started).
pub async fn wait_ready(timeout: Duration) -> Result<()> {
    let end = std::time::Instant::now() + timeout;
    loop {
        match http(Method::GET, "/v2/", None).await {
            Ok((200, _, _)) => return Ok(()),
            _ if std::time::Instant::now() > end => {
                return Err(Error::Unavailable(format!(
                    "the image registry ({CONTAINER}) doesn't answer"
                )))
            }
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
}

/// Tags of `vfy/<app>` in the registry.
pub async fn tags(app: &str) -> Result<Vec<String>> {
    let (status, _, body) = http(Method::GET, &format!("/v2/vfy/{app}/tags/list"), None).await?;
    if status == 404 {
        return Ok(vec![]);
    }
    let v: serde_json::Value = serde_json::from_slice(&body)?;
    Ok(v["tags"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
        .unwrap_or_default())
}

/// Delete a tag's manifest (its layers go with the next garbage collection).
pub async fn delete_tag(app: &str, tag: &str) -> Result<()> {
    let path = format!("/v2/vfy/{app}/manifests/{tag}");
    let (status, headers, _) = http(Method::HEAD, &path, Some(MANIFESTS)).await?;
    if status == 404 {
        return Ok(());
    }
    let digest = headers
        .get("docker-content-digest")
        .and_then(|d| d.to_str().ok())
        .ok_or_else(|| Error::Internal(format!("the registry gave no digest for {app}:{tag}")))?
        .to_string();
    let (status, _, body) = http(Method::DELETE, &format!("/v2/vfy/{app}/manifests/{digest}"), None).await?;
    if !(200..300).contains(&status) && status != 404 {
        return Err(Error::Internal(format!(
            "deleting {app}:{tag} from the registry: HTTP {status} {}",
            String::from_utf8_lossy(&body)
        )));
    }
    Ok(())
}

/// Drop registry tags of `app` that this server no longer keeps itself
/// (it keeps what it may roll back to), then nothing else: layers are
/// freed by [`collect_garbage`].
pub async fn prune(docker: &Docker, app: &str) -> Result<usize> {
    let mut n = 0;
    for tag in tags(app).await? {
        let local = format!("vfy/{app}:{tag}");
        if !docker.image_exists(&local).await? {
            delete_tag(app, &tag).await?;
            n += 1;
        }
    }
    Ok(n)
}

/// Free the layers no tag uses any more (daily; the registry is read-only
/// meanwhile for a moment, which a push in flight retries through).
pub async fn collect_garbage(docker: &Docker) -> Result<()> {
    let args = proc::args(&[
        "exec",
        CONTAINER,
        "bin/registry",
        "garbage-collect",
        "--delete-untagged",
        "/etc/docker/registry/config.yml",
    ]);
    let o = docker.run(&args, Duration::from_secs(600)).await?;
    if !o.success() {
        return Err(Error::Internal(format!("registry garbage collection: {}", o.tail(3))));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(reference("vfy/blog:abc"), "127.0.0.1:5055/vfy/blog:abc");
        assert_eq!(docker_arch("aarch64"), "arm64");
        assert_eq!(docker_arch("x86_64"), "amd64");
        assert_eq!(arch_tag("vfy/blog:abc", "x86_64", "amd64"), "vfy/blog:abc");
        assert_eq!(arch_tag("vfy/blog:abc", "aarch64", "x86_64"), "vfy/blog:abc-arm64");
        assert_eq!(arch_tag("vfy/blog:abc", "armv7l", "x86_64"), "vfy/blog:abc-armv7");
    }

    /// A real registry (opt in with VERCELIFY_DOCKER_TESTS=1).
    #[tokio::test]
    async fn push_list_delete() {
        if std::env::var("VERCELIFY_DOCKER_TESTS").as_deref() != Ok("1") {
            return;
        }
        let d = Docker::default();
        let dir = tempfile::tempdir().unwrap();
        ensure(&d, dir.path(), &[]).await.unwrap();
        ensure(&d, dir.path(), &[]).await.unwrap();
        for _ in 0..50 {
            if http(Method::GET, "/v2/", None).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let r = d
            .run(
                &proc::args(&["tag", "alpine:3.20", "vfy/regtest:one"]),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        assert!(r.success());
        push(&d, "vfy/regtest:one", &mut |_| {}).await.unwrap();
        assert_eq!(tags("regtest").await.unwrap(), ["one"]);
        d.remove_image("vfy/regtest:one").await.unwrap();
        assert_eq!(prune(&d, "regtest").await.unwrap(), 1);
        assert!(tags("regtest").await.unwrap().is_empty());
        collect_garbage(&d).await.unwrap();
        d.remove(CONTAINER).await.unwrap();
    }
}
