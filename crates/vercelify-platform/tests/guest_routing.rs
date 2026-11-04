use vercelify_platform::proxy::{self, Route, RouteHost};
#[test]
fn bridge_alias_requires_auth_before_every_backend_and_does_not_relax_mesh_hosts() {
    let normal = RouteHost {
        host: "pr-7-app.main.vfy".into(),
        https: false,
        redirect_to: None,
        allow_from: vec!["10.77.0.0/16".into()],
        guest_bridge: false,
    };
    let bridge = RouteHost {
        host: "shr-test.guests.example.test".into(),
        https: false,
        redirect_to: None,
        allow_from: vec![],
        guest_bridge: true,
    };
    let rule = proxy::host_rule(&bridge);
    assert!(rule.contains("HeaderRegexp(`X-Vfy-Bridge`, `.+`)"));
    let text = proxy::render(&Route {
        key: "app-pr-7".into(),
        hosts: vec![normal, bridge],
        upstreams: vec!["http://app:3000".into()],
        basic_auth: vec![],
        dns_challenge: false,
    });
    assert!(text.contains("vfy-app-pr-7-0-private"));
    let alias = text
        .split("vfy-app-pr-7-1:")
        .nth(1)
        .unwrap()
        .split("  services:")
        .next()
        .unwrap();
    assert!(alias.contains("vfy-guest-bridge") && alias.contains("vfy-guest-strip"));
}
