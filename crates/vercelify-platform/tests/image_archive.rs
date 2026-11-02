//! Archive sanitization must not grant Docker permission to retag other apps.
use vercelify_platform::artifact;

fn entry(out: &mut Vec<u8>, name: &str, body: &[u8], kind: u8) {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..108].copy_from_slice(b"0000600\0");
    let size = format!("{:011o}\0", body.len());
    header[124..136].copy_from_slice(size.as_bytes());
    header[148..156].fill(b' ');
    header[156] = kind;
    header[257..263].copy_from_slice(b"ustar\0");
    let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    out.extend(header);
    out.extend(body);
    out.resize(out.len().div_ceil(512) * 512, 0);
}

#[test]
fn rejects_unauthorized_tags_links_truncation_and_digest_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let tag = "vfy/offhost:dpl_contract-1";
    let config = br#"{"architecture":"arm64","os":"linux","rootfs":{"type":"layers","diff_ids":[]}}"#;
    use sha2::{Digest, Sha256};
    let hex = format!("{:x}", Sha256::digest(config));
    let path = format!("blobs/sha256/{hex}");
    let manifest = serde_json::json!([{"Config":path,"RepoTags":[tag],"Layers":[]}]).to_string();
    let mut raw = vec![];
    entry(&mut raw, &path, config, b'0');
    entry(&mut raw, "manifest.json", manifest.as_bytes(), b'0');
    // This alternate OCI index must never make it into Docker's input.
    entry(&mut raw, "index.json", br#"{"manifests":[]}"#, b'0');
    raw.extend([0; 1024]);
    let input = dir.path().join("input.tar");
    std::fs::write(&input, &raw).unwrap();
    let output = dir.path().join("safe.tar");
    let identity = artifact::sanitize(&input, &output, tag, &format!("sha256:{hex}"), "linux/arm64").unwrap();
    let loaded = serde_json::json!({"Os":"linux","Architecture":"arm64","RootFS":{"Layers":[]},"Config":{}});
    assert!(artifact::matches_loaded(&identity, &loaded, "linux/arm64"));
    let mut wrong = loaded.clone();
    wrong["Config"]["Cmd"] = serde_json::json!(["wrong-command"]);
    assert!(!artifact::matches_loaded(&identity, &wrong, "linux/arm64"));
    let safe = std::fs::read(&output).unwrap();
    assert!(!safe.windows(10).any(|v| v == b"index.json"));
    assert!(artifact::sanitize(
        &input,
        &output,
        "vfy/other:production",
        &format!("sha256:{hex}"),
        "linux/arm64"
    )
    .is_err());
    assert!(artifact::sanitize(
        &input,
        &output,
        tag,
        &format!("sha256:{}", "0".repeat(64)),
        "linux/arm64"
    )
    .is_err());
    assert!(artifact::sanitize(&input, &output, tag, &format!("sha256:{hex}"), "linux/amd64").is_err());
    std::fs::write(&input, &raw[..raw.len() - 1100]).unwrap();
    assert!(artifact::sanitize(&input, &output, tag, &format!("sha256:{hex}"), "linux/arm64").is_err());
    let mut evil = vec![];
    entry(&mut evil, "manifest.json", manifest.as_bytes(), b'2');
    evil.extend([0; 1024]);
    std::fs::write(&input, evil).unwrap();
    assert!(artifact::sanitize(&input, &output, tag, &format!("sha256:{hex}"), "linux/arm64").is_err());
}
