use std::{fs, os::unix::fs::{symlink, PermissionsExt}, time::Duration, sync::{Arc, Barrier}};
use vercelify_platform::{compose, docker::Docker, proc::Cancel, detect};
use vercelify_core::platform::{AppBuild, AppKind, AppSource};
mod origin { include!("private_origin.rs"); }
#[tokio::main]
async fn main() {
 let t=tempfile::tempdir().unwrap(); let root=t.path();
 let dns_target="127.0.0.1.sslip.io"; let resolved:Vec<_>=tokio::net::lookup_host((dns_target,11434)).await.unwrap().map(|a|a.ip().to_string()).collect(); println!("rewrite_private_origin={} resolutions={:?}",origin::private_origin(&format!("http://{dns_target}:11434")),resolved);
 let repo=root.join("repo"); fs::create_dir_all(repo.join(".vercelify")).unwrap();
 let victim=root.join("host-file"); fs::write(&victim,"host data must survive").unwrap();
 symlink(&victim,repo.join(".vercelify/app.env")).unwrap();
 fs::write(repo.join("docker-compose.yml"),"services:\n  web:\n    image: alpine\n").unwrap();
 let stub=root.join("docker-stub");
 fs::write(&stub,"#!/bin/sh\ncase \"$*\" in *'config --format json'*) echo '{\"services\":{\"web\":{\"image\":\"alpine\"}}}';; esac\nexit 0\n").unwrap();
 fs::set_permissions(&stub,fs::Permissions::from_mode(0o755)).unwrap();
 let docker=Docker{bin:stub.to_string_lossy().into()};
 let env=vec![("AUDIT".into(),"synthetic-only".into())];
 let out=compose::up(compose::Up{docker:&docker,dir:&repo,file:"docker-compose.yml",project:"vfy-audit",service:None,env:&env,labels:compose::Labels{app_id:"app_audit",app_name:"audit",deployment_id:"dpl_audit",project_id:"prj_audit",pr:None},timeout:Duration::from_secs(5)},Cancel::never(),&mut |_,_|{}).await;
 println!("symlink_write={}",serde_json::json!({"up_succeeded":out.is_ok(),"host_file_after":fs::read_to_string(&victim).unwrap(),"repo_symlink_removed":!repo.join(".vercelify/app.env").exists()}));
 let outside=root.join("outside-context"); fs::create_dir(&outside).unwrap(); fs::write(outside.join("index.html"),"private host fixture").unwrap(); symlink(&outside,repo.join("context-link")).unwrap();
 let mut build=AppBuild::default(); build.context=Some("context-link".into());
 let plan=detect::plan(&repo,AppKind::Git,&AppSource::default(),&build);
 println!("symlink_context={}",serde_json::json!({"accepted":plan.is_ok(),"outside_checkout":!fs::canonicalize(repo.join("context-link")).unwrap().starts_with(&repo)}));
 let picked=compose::pick(&serde_json::json!({"services":{"db":{"expose":["5432"]},"web":{"expose":["3000"]}}}),None).unwrap(); println!("compose_default_routed={}",picked.1);
 let only_compose=root.join("compose-only"); fs::create_dir(&only_compose).unwrap(); fs::write(only_compose.join("docker-compose.yml"),"services:\n  web:\n    image: nginx\n").unwrap(); println!("git_auto_detects_compose={}",detect::plan(&only_compose,AppKind::Git,&AppSource::default(),&AppBuild::default()).is_ok());

 let unsafe_dir=root.join("unsafe-config"); fs::create_dir(&unsafe_dir).unwrap();
 let compose_text="services:\n  web:\n    image: alpine\n    privileged: true\n    ports: [\"3000:3000\"]\n    volumes: [\"./persist:/data\"]\n".replace("\\n","\n");
 fs::write(unsafe_dir.join("docker-compose.yml"),&compose_text).unwrap(); let empty_env=unsafe_dir.join("empty.env"); fs::write(&empty_env,"").unwrap();
 let overrides=compose::override_yaml(&["web".into()],"web",&empty_env,&compose::Labels{app_id:"app_fixture",app_name:"fixture",deployment_id:"dpl_fixture",project_id:"prj_fixture",pr:None});
 fs::write(unsafe_dir.join("override.yml"),&overrides).unwrap();
 let unsafe_config=std::process::Command::new("docker-compose").current_dir(&unsafe_dir).args(["-f","docker-compose.yml","-f","override.yml","config","--format","json"]).output().unwrap();
 let conf:serde_json::Value=serde_json::from_slice(&unsafe_config.stdout).unwrap();
 let req:vercelify_core::platform::AppCreate=serde_json::from_value(serde_json::json!({"name":"audit-unsafe","kind":"compose","source":{"compose_content":compose_text}})).unwrap();
 let store=vercelify_platform::Store::open_in_memory().unwrap();
 println!("unsafe_compose={}",serde_json::json!({"app_create_accepted":store.tx(|c| vercelify_platform::apps::create(c,&req)).is_ok(),"privileged_preserved":conf["services"]["web"]["privileged"],"published_ports_preserved":!conf["services"]["web"]["ports"].as_array().unwrap().is_empty(),"bind_source":conf["services"]["web"]["volumes"][0]["source"],"bind_inside_checkout":conf["services"]["web"]["volumes"][0]["source"].as_str().unwrap().starts_with(fs::canonicalize(&unsafe_dir).unwrap().to_str().unwrap())}));
 let fail_bin=root.join("docker-fails");fs::write(&fail_bin,"#!/bin/sh\necho 'volume is in use' >&2\nexit 1\n").unwrap();fs::set_permissions(&fail_bin,fs::Permissions::from_mode(0o755)).unwrap(); println!("failed_volume_removal_reported_success={}",Docker{bin:fail_bin.to_string_lossy().into()}.volume_remove("synthetic-only").await.is_ok());
 let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();let server=tokio::spawn(async move {use tokio::io::{AsyncReadExt,AsyncWriteExt}; loop{let(mut sock,_)=listener.accept().await.unwrap();let mut b=[0;2048];let _=sock.read(&mut b).await;let _=sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await;}});
 let health_bin=root.join("docker-health");fs::write(&health_bin,"#!/bin/sh\necho '{\"State\":{\"Running\":true},\"NetworkSettings\":{\"Networks\":{\"test\":{\"IPAddress\":\"127.0.0.1\"}}}}'\n").unwrap();fs::set_permissions(&health_bin,fs::Permissions::from_mode(0o755)).unwrap();let health_docker=Docker{bin:health_bin.to_string_lossy().into()};
 let healthy=vercelify_platform::health::wait_healthy(vercelify_platform::health::Wait{docker:&health_docker,container:"synthetic",network:"test",port,probe:vercelify_platform::health::Probe::Http{path:"/healthz".into()},timeout:Duration::from_secs(2),interval:Duration::from_millis(5)},&mut |_|{}).await.is_ok();server.abort();println!("http_404_passed_readiness={healthy}");
 let envfile=root.join("quoted.env"); fs::write(&envfile,compose::env_line("AUDIT_VALUE","it's ${AUDIT_EXPANSION}")).unwrap();
 let config=root.join("quote-compose.yml"); fs::write(&config,format!("services:\n  web:\n    image: alpine\n    env_file:\n      - {}\n",envfile.display())).unwrap();
 let o=std::process::Command::new("docker-compose").args(["-f"]).arg(&config).args(["config","--format","json"]).env("AUDIT_EXPANSION","changed-by-host-environment").output().unwrap();
 let v:serde_json::Value=serde_json::from_slice(&o.stdout).unwrap(); println!("compose_literal_value={}",v["services"]["web"]["environment"]["AUDIT_VALUE"]);
 let state=root.join("state.json"); let barrier=Arc::new(Barrier::new(8)); let errors=Arc::new(std::sync::atomic::AtomicUsize::new(0));
 let hs:Vec<_>=(0..8).map(|n|{let p=state.clone();let b=barrier.clone();let e=errors.clone();std::thread::spawn(move||{b.wait();for k in 0..100 {let data=format!("{{\"writer\":{n},\"iteration\":{k},\"data\":\"{}\"}}","x".repeat(1000+n));if vercelify_mesh::state::write_private(&p,data.as_bytes()).is_err(){e.fetch_add(1,std::sync::atomic::Ordering::SeqCst);}}})}).collect();for h in hs {h.join().unwrap();}
 println!("atomic_write_race={}",serde_json::json!({"errors":errors.load(std::sync::atomic::Ordering::SeqCst),"final_json_valid":serde_json::from_slice::<serde_json::Value>(&fs::read(state).unwrap()).is_ok()}));
}
