//! Where a deployment's containers run (design §8.3): one row per replica.
//! Deployments from before multi-node have only `deployments.container`;
//! [`of`] reads those as one instance on the main server.

use crate::ids::new_id;
use crate::{deployments, nodes, now, Result};
use rusqlite::{params, Connection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub node_id: String,
    pub container: String,
    pub replica: u8,
}

/// Record a deployment's instances (replacing any), the first also as
/// `deployments.container` / `node_id` for what reads only those.
pub fn set(c: &Connection, deployment_id: &str, list: &[Instance]) -> Result<()> {
    c.execute("DELETE FROM instances WHERE deployment_id = ?1", [deployment_id])?;
    let t = now();
    for i in list {
        c.execute(
            "INSERT INTO instances (id, deployment_id, node_id, container, replica, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![new_id("ins"), deployment_id, i.node_id, i.container, i.replica, t],
        )?;
    }
    if let Some(first) = list.first() {
        c.execute(
            "UPDATE deployments SET node_id = ?2 WHERE id = ?1",
            params![deployment_id, first.node_id],
        )?;
        deployments::set_container(c, deployment_id, &first.container, None)?;
    }
    Ok(())
}

/// A deployment's instances, replica order.
pub fn of(c: &Connection, deployment_id: &str) -> Result<Vec<Instance>> {
    let mut st = c.prepare(
        "SELECT node_id, container, replica FROM instances WHERE deployment_id = ?1 ORDER BY replica, container",
    )?;
    let rows: Vec<Instance> = st
        .query_map([deployment_id], |r| {
            Ok(Instance {
                node_id: r.get(0)?,
                container: r.get(1)?,
                replica: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    if !rows.is_empty() {
        return Ok(rows);
    }
    let container = deployments::container(c, deployment_id)?;
    if container.is_empty() {
        return Ok(vec![]);
    }
    Ok(vec![Instance {
        node_id: nodes::main(c)?.id,
        container,
        replica: 0,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    #[test]
    fn legacy_deployments_run_on_main() {
        let s = Store::open_in_memory().unwrap();
        let (main, dep) = s
            .tx(|c| {
                let m = nodes::ensure_main(c, "laptop", "http://127.0.0.1:7420", "x86_64")?;
                let app = crate::apps::create(c, &crate::apps::tests::git_app("blog"))?.id;
                let d = deployments::create(
                    c,
                    &deployments::NewDeployment {
                        app_id: &app,
                        pr_number: None,
                        branch: "main",
                        commit_sha: "",
                        commit_message: "",
                        trigger: vercelify_core::platform::DeploymentTrigger::Manual,
                        rollback_of: None,
                        image: "nginx:1",
                        node_id: None,
                    },
                )?;
                Ok((m.id, d.id))
            })
            .unwrap();
        assert!(s.with(|c| of(c, &dep)).unwrap().is_empty());
        s.tx(|c| deployments::set_container(c, &dep, "vfy-blog-1", None))
            .unwrap();
        assert_eq!(
            s.with(|c| of(c, &dep)).unwrap(),
            [Instance {
                node_id: main.clone(),
                container: "vfy-blog-1".into(),
                replica: 0
            }]
        );
        let two = [
            Instance {
                node_id: main.clone(),
                container: "vfy-blog-2".into(),
                replica: 0,
            },
            Instance {
                node_id: main.clone(),
                container: "vfy-blog-2-r1".into(),
                replica: 1,
            },
        ];
        s.tx(|c| set(c, &dep, &two)).unwrap();
        assert_eq!(s.with(|c| of(c, &dep)).unwrap(), two);
        assert_eq!(s.with(|c| deployments::container(c, &dep)).unwrap(), "vfy-blog-2");
    }
}
