//! The engine's view of its servers (design §8.2, §8.3): a runtime per
//! server, the instances of a deployment with where they run, and the
//! routes each server's proxy needs for them.
//!
//! Public traffic arrives at the main server's proxy (port forwards or the
//! tunnel point there). An app with instances elsewhere is routed by the
//! main server to those servers' proxies over the mesh (plain HTTP inside
//! WireGuard, the Host header kept); each of those routes the same hosts
//! to its local containers. TLS, redirects and Basic Auth stay on the main
//! server's route (Basic Auth is checked on both).

use crate::engine::Engine;
use crate::instances::{self, Instance};
use crate::proxy::{Route, RouteHost};
use crate::runtime::{NodeRuntime, Remote};
use crate::{nodes, Error, Result};
use std::collections::BTreeMap;
use vercelify_core::platform::PlatformNode;

/// An instance with its server.
#[derive(Debug, Clone)]
pub struct Placed {
    pub node: PlatformNode,
    pub instance: Instance,
}

impl Engine {
    /// The runtime of a server: Docker here, or the worker's agent.
    pub fn node_runtime(&self, n: &PlatformNode) -> Result<NodeRuntime> {
        if n.role == "main" {
            return Ok(NodeRuntime::Local(self.deployer.clone()));
        }
        let token = self
            .store
            .with(|c| nodes::runtime_token(c, &self.key, &n.id))?
            .ok_or_else(|| {
                Error::Unavailable(format!(
                    "{} hasn't registered with this server yet (is its agent running, on the mesh?)",
                    n.name
                ))
            })?;
        Ok(NodeRuntime::Remote(Remote {
            node: n.name.clone(),
            base: n.agent_url.clone(),
            token,
        }))
    }

    pub fn runtime_of(&self, node_id: &str) -> Result<NodeRuntime> {
        let n = self.store.with(|c| nodes::get(c, node_id))?;
        self.node_runtime(&n)
    }

    /// A deployment's instances with their servers.
    pub fn placed(&self, deployment_id: &str) -> Result<Vec<Placed>> {
        self.store.with(|c| {
            let mut out = vec![];
            let mut cache: BTreeMap<String, PlatformNode> = BTreeMap::new();
            for i in instances::of(c, deployment_id)? {
                let node = match cache.get(&i.node_id) {
                    Some(n) => n.clone(),
                    None => {
                        let n = nodes::get(c, &i.node_id)?;
                        cache.insert(n.id.clone(), n.clone());
                        n
                    }
                };
                out.push(Placed { node, instance: i });
            }
            Ok(out)
        })
    }
}

/// What a route needs besides where the instances run.
#[derive(Debug, Clone)]
pub struct RouteBase {
    pub key: String,
    pub hosts: Vec<RouteHost>,
    pub port: u16,
    pub basic_auth: Vec<String>,
    pub dns_challenge: bool,
}

/// The route each server needs (by node id). The main server's lists its
/// own containers and every other server with instances (online ones; all
/// of them only when none is, since a 502 says more than nothing).
pub fn routes(base: &RouteBase, placed: &[Placed], main_id: &str) -> BTreeMap<String, Route> {
    let mut out = BTreeMap::new();
    if base.hosts.is_empty() || placed.is_empty() {
        return out;
    }
    let mut local: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut others: BTreeMap<String, &PlatformNode> = BTreeMap::new();
    for p in placed {
        local
            .entry(p.node.id.clone())
            .or_default()
            .push(format!("http://{}:{}", p.instance.container, base.port));
        if p.node.role != "main" {
            others.insert(p.node.id.clone(), &p.node);
        }
    }
    let any_online = others.values().any(|n| n.online);
    let forward: Vec<String> = others
        .values()
        .filter(|n| n.online || !any_online)
        .map(|n| {
            let port = if n.status.http_port == 0 {
                80
            } else {
                n.status.http_port
            };
            format!("http://{}:{port}", n.mesh_ip)
        })
        .collect();
    let mut main_upstreams = local.get(main_id).cloned().unwrap_or_default();
    main_upstreams.extend(forward);
    out.insert(
        main_id.to_string(),
        Route {
            key: base.key.clone(),
            hosts: base.hosts.clone(),
            upstreams: main_upstreams,
            basic_auth: base.basic_auth.clone(),
            dns_challenge: base.dns_challenge,
        },
    );
    let plain: Vec<RouteHost> = base
        .hosts
        .iter()
        .filter(|h| h.redirect_to.is_none())
        .map(|h| RouteHost {
            host: h.host.clone(),
            https: false,
            redirect_to: None,
        })
        .collect();
    for id in others.keys() {
        out.insert(
            id.clone(),
            Route {
                key: base.key.clone(),
                hosts: plain.clone(),
                upstreams: local.get(id).cloned().unwrap_or_default(),
                basic_auth: base.basic_auth.clone(),
                dns_challenge: false,
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vercelify_core::platform::NodeStatus;

    pub fn node(id: &str, role: &str, ip: &str, online: bool) -> PlatformNode {
        PlatformNode {
            id: id.into(),
            name: id.into(),
            role: role.into(),
            agent_url: format!("http://{ip}:7420"),
            arch: "x86_64".into(),
            builds: false,
            last_seen_at: None,
            created_at: String::new(),
            mesh_ip: ip.into(),
            online,
            drained: false,
            status: NodeStatus {
                http_port: 80,
                ..NodeStatus::default()
            },
            instances: 0,
        }
    }

    fn placed(n: &PlatformNode, c: &str) -> Placed {
        Placed {
            node: n.clone(),
            instance: Instance {
                node_id: n.id.clone(),
                container: c.into(),
                replica: 0,
            },
        }
    }

    fn base() -> RouteBase {
        RouteBase {
            key: "blog".into(),
            hosts: vec![
                RouteHost {
                    host: "blog.example.com".into(),
                    https: true,
                    redirect_to: None,
                },
                RouteHost {
                    host: "www.blog.example.com".into(),
                    https: true,
                    redirect_to: Some("blog.example.com".into()),
                },
            ],
            port: 3000,
            basic_auth: vec!["u:h".into()],
            dns_challenge: false,
        }
    }

    #[test]
    fn main_forwards_to_the_other_servers() {
        let main = node("main", "main", "10.77.0.1", true);
        let pi = node("pi", "worker", "10.77.0.5", true);
        let p = vec![placed(&main, "vfy-blog-1"), placed(&pi, "vfy-blog-1-r1")];
        let r = routes(&base(), &p, "main");
        assert_eq!(r["main"].upstreams, ["http://vfy-blog-1:3000", "http://10.77.0.5:80"]);
        assert_eq!(r["main"].hosts.len(), 2);
        assert_eq!(r["pi"].upstreams, ["http://vfy-blog-1-r1:3000"]);
        assert_eq!(r["pi"].hosts.len(), 1, "redirects stay on the main server");
        assert!(!r["pi"].hosts[0].https, "TLS ends on the main server");
        assert_eq!(r["pi"].basic_auth, ["u:h"]);
    }

    #[test]
    fn offline_servers_leave_the_main_route() {
        let main = node("main", "main", "10.77.0.1", true);
        let up = node("a", "worker", "10.77.0.5", true);
        let down = node("b", "worker", "10.77.0.6", false);
        let r = routes(
            &base(),
            &[placed(&main, "m"), placed(&up, "x"), placed(&down, "y")],
            "main",
        );
        assert_eq!(r["main"].upstreams, ["http://m:3000", "http://10.77.0.5:80"]);
        // Only on a server that is down: keep it (a 502 beats nothing).
        let r = routes(&base(), &[placed(&down, "y")], "main");
        assert_eq!(r["main"].upstreams, ["http://10.77.0.6:80"]);
        assert!(routes(
            &RouteBase {
                hosts: vec![],
                ..base()
            },
            &[placed(&main, "m")],
            "main"
        )
        .is_empty());
    }
}
