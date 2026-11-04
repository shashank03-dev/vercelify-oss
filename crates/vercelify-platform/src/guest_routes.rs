//! Reserved public ingress and the bridge-only backend authentication chain.
use crate::proxy::{GUEST_AUTH, GUEST_CAPABILITY, GUEST_STRIP, RESOLVER};
use std::fmt::Write as _;
fn q(value: &str) -> String {
    serde_json::to_string(value).expect("serializable")
}
pub fn render(domains: &[String], hosts: &[String], wake: &str, public_port: u16, bridge_user: &str) -> String {
    let mut text=format!("# Written by vercelify-agent. Guest bridge secrets never enter this file.\nhttp:\n  middlewares:\n    {GUEST_CAPABILITY}:\n      basicAuth:\n        users: [{}]\n        realm: \"Vercelify internal guest bridge\"\n        removeHeader: true\n    {GUEST_AUTH}:\n      forwardAuth:\n        address: {}\n        trustForwardHeader: false\n        maxResponseBodySize: 4096\n        authRequestHeaders: [X-Vfy-Bridge, X-Vfy-Session, X-Vfy-Target, X-Vfy-App-Authorization]\n        authResponseHeaders: [Authorization]\n    {GUEST_STRIP}:\n      headers:\n        customRequestHeaders:\n          X-Vfy-Bridge: \"\"\n          X-Vfy-Session: \"\"\n          X-Vfy-Target: \"\"\n          X-Vfy-App-Authorization: \"\"\n          X-Forwarded-Proto: \"https\"\n          X-Forwarded-Port: {}\n    vercelify-guest-https:\n      redirectScheme:\n        scheme: https\n        port: {}\n        permanent: true\n  services:\n    vercelify-guest:\n      loadBalancer:\n        servers:\n          - url: {}\n",q(bridge_user),q(&format!("{wake}/__vfy/internal-check")),q(&public_port.to_string()),q(&public_port.to_string()),q(wake));
    if domains.is_empty() {
        return text;
    }
    let rules: Vec<_> = domains
        .iter()
        .map(|domain| format!("HostRegexp(`^[a-z0-9-]+\\.{}$`)", domain.replace('.', "\\.")))
        .collect();
    let rule = format!("({}) && !HeaderRegexp(`X-Vfy-Bridge`, `.+`)", rules.join(" || "));
    let _=write!(text,"  routers:\n    vercelify-guest-http:\n      rule: {}\n      entryPoints: [web]\n      priority: 60000\n      service: vercelify-guest\n      middlewares: [vercelify-guest-https]\n    vercelify-guest-secure:\n      rule: {}\n      entryPoints: [websecure]\n      priority: 60000\n      service: vercelify-guest\n      tls: {{}}\n",q(&rule),q(&rule));
    for (index, host) in hosts.iter().enumerate() {
        let rule = format!("Host(`{host}`) && !HeaderRegexp(`X-Vfy-Bridge`, `.+`)");
        let _=write!(text,"    vercelify-guest-cert-{index}:\n      rule: {}\n      entryPoints: [websecure]\n      priority: 60001\n      service: vercelify-guest\n      tls:\n        certResolver: {RESOLVER}\n",q(&rule));
    }
    text
}

/// The bridge credential is random high-entropy material; cost 4 bounds per-request
/// work. Only its bcrypt verifier enters the proxy file, never the raw credential.
pub fn bridge_user(secret: &str) -> crate::Result<String> {
    bcrypt::hash(secret, 4)
        .map(|hash| format!("bridge:{hash}"))
        .map_err(|_| crate::Error::Internal("guest bridge verifier unavailable".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_and_internal_paths_cannot_select_each_other_without_authority() {
        let text = render(
            &["guests.example.test".into()],
            &["shr-one.guests.example.test".into()],
            "http://host.docker.internal:7421",
            443,
            "bridge:$2b$04$fixture",
        );
        assert!(text.contains("priority: 60000"));
        assert!(text.contains("!HeaderRegexp(`X-Vfy-Bridge`, `.+`)"));
        assert!(text.contains("trustForwardHeader: false"));
        let yaml = yaml_rust2::YamlLoader::load_from_str(&text).unwrap();
        for name in ["X-Vfy-Bridge", "X-Vfy-Session", "X-Vfy-Target"] {
            assert_eq!(
                yaml[0]["http"]["middlewares"][GUEST_STRIP]["headers"]["customRequestHeaders"][name].as_str(),
                Some("")
            );
        }
        assert!(text.contains("authResponseHeaders: [Authorization]"));
        assert_eq!(
            yaml[0]["http"]["middlewares"][GUEST_CAPABILITY]["basicAuth"]["removeHeader"].as_bool(),
            Some(true)
        );
        assert!(text.contains("/__vfy/internal-check"));
    }
}
