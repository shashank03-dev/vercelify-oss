//! The bits of GitHub's REST API that auto-deploy needs: open pull requests,
//! commit statuses and one comment per pull request with its preview URL.
//!
//! Requests go through curl (TLS without linking a TLS stack into the
//! agent); the token travels in a private curl config file, never on the
//! command line.

use crate::config::Config;
use crate::watch::write_private;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub head_sha: String,
    /// Opened from a fork: someone else's code, never built automatically.
    pub from_fork: bool,
}

/// `owner/repo` of a github.com clone URL.
pub fn repo_slug(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (parts.next().is_none() && ok(owner) && ok(repo)).then(|| format!("{owner}/{repo}"))
}

#[derive(Deserialize)]
struct RawPr {
    number: u64,
    #[serde(default)]
    title: String,
    head: RawHead,
    base: RawHead,
}

#[derive(Deserialize)]
struct RawHead {
    sha: String,
    #[serde(default)]
    repo: Option<RawRepo>,
}

#[derive(Deserialize)]
struct RawRepo {
    full_name: String,
}

pub fn parse_pulls(json: &str) -> Result<Vec<PullRequest>, String> {
    let raw: Vec<RawPr> = serde_json::from_str(json).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    Ok(raw
        .into_iter()
        .map(|p| PullRequest {
            from_fork: match (&p.head.repo, &p.base.repo) {
                (Some(h), Some(b)) => h.full_name != b.full_name,
                // A deleted fork: treat as foreign.
                _ => true,
            },
            number: p.number,
            title: p.title.chars().filter(|c| !c.is_control()).take(120).collect(),
            head_sha: p.head.sha,
        })
        .collect())
}

/// One API call: (HTTP status, body).
pub async fn api(
    cfg: &Config,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&serde_json::Value>,
) -> Result<(u16, String), String> {
    use tokio::io::AsyncWriteExt;
    let mut conf = format!(
        "url = \"{}{}\"\nrequest = \"{method}\"\nheader = \"Accept: application/vnd.github+json\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\nuser-agent = \"vercelify-agent\"\n",
        cfg.github_api.trim_end_matches('/'),
        path.replace('"', "")
    );
    if let Some(t) = token {
        conf.push_str(&format!(
            "header = \"Authorization: Bearer {}\"\n",
            t.replace(['"', '\n'], "")
        ));
    }
    if body.is_some() {
        conf.push_str("header = \"Content-Type: application/json\"\ndata-binary = \"@-\"\n");
    }
    let mut random = [0u8; 6];
    let _ = getrandom::fill(&mut random);
    let file = cfg.run_dir.join(format!(
        "gh-{}.curl",
        random.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    write_private(&file, conf.as_bytes()).map_err(|e| format!("writing {}: {e}", file.display()))?;
    let child = tokio::process::Command::new(&cfg.curl_bin)
        .args(["-sS", "-m", "20", "-w", "\n%{http_code}", "-K"])
        .arg(&file)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let result = async {
        let mut child = child.map_err(|e| format!("running curl: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            if let Some(b) = body {
                let _ = stdin.write_all(b.to_string().as_bytes()).await;
            }
        }
        let out = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
            .await
            .map_err(|_| "GitHub did not answer in time".to_string())?
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_str()));
        let code: u16 = code.trim().parse().unwrap_or(0);
        if code == 0 {
            return Err(format!(
                "cannot reach GitHub: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok((code, body.to_string()))
    }
    .await;
    let _ = std::fs::remove_file(&file);
    result
}

fn error_of(code: u16, body: &str) -> String {
    let msg = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("message")?.as_str().map(String::from))
        .unwrap_or_default();
    match code {
        401 => "GitHub rejected the token".into(),
        403 if msg.contains("rate limit") => "GitHub rate limit reached — add a token to check more often".into(),
        403 | 404 => format!("GitHub: {msg} (does the token have access to this repository?)"),
        _ => format!("GitHub HTTP {code}: {msg}"),
    }
}

pub async fn open_pulls(cfg: &Config, slug: &str, token: Option<&str>) -> Result<Vec<PullRequest>, String> {
    let (code, body) = api(
        cfg,
        "GET",
        &format!("/repos/{slug}/pulls?state=open&per_page=50&sort=updated&direction=desc"),
        token,
        None,
    )
    .await?;
    if code != 200 {
        return Err(error_of(code, &body));
    }
    parse_pulls(&body)
}

/// A commit status as GitHub shows it next to a commit or pull request.
pub struct CommitStatus<'a> {
    pub sha: &'a str,
    /// pending | success | failure | error
    pub state: &'a str,
    pub target_url: Option<&'a str>,
    pub description: &'a str,
    pub context: &'a str,
}

pub async fn set_status(cfg: &Config, slug: &str, token: &str, st: CommitStatus<'_>) -> Result<(), String> {
    let (sha, target_url) = (st.sha, st.target_url);
    let mut body = serde_json::json!({ "state": st.state, "description": st.description, "context": st.context });
    if let Some(u) = target_url {
        body["target_url"] = u.into();
    }
    let (code, resp) = api(
        cfg,
        "POST",
        &format!("/repos/{slug}/statuses/{sha}"),
        Some(token),
        Some(&body),
    )
    .await?;
    if code == 201 {
        Ok(())
    } else {
        Err(error_of(code, &resp))
    }
}

/// Create the preview comment, or update it when `existing` is set. Returns its id.
pub async fn upsert_comment(
    cfg: &Config,
    slug: &str,
    pr: u64,
    token: &str,
    existing: Option<u64>,
    text: &str,
) -> Result<u64, String> {
    let body = serde_json::json!({ "body": text });
    let (code, resp) = match existing {
        Some(id) => {
            api(
                cfg,
                "PATCH",
                &format!("/repos/{slug}/issues/comments/{id}"),
                Some(token),
                Some(&body),
            )
            .await?
        }
        None => {
            api(
                cfg,
                "POST",
                &format!("/repos/{slug}/issues/{pr}/comments"),
                Some(token),
                Some(&body),
            )
            .await?
        }
    };
    if code == 200 || code == 201 {
        serde_json::from_str::<serde_json::Value>(&resp)
            .ok()
            .and_then(|v| v.get("id")?.as_u64())
            .ok_or_else(|| "GitHub returned no comment id".into())
    } else if code == 404 && existing.is_some() {
        // Someone deleted our comment: write a new one.
        Box::pin(upsert_comment(cfg, slug, pr, token, None, text)).await
    } else {
        Err(error_of(code, &resp))
    }
}

pub fn preview_comment(app: &str, url: &str, sha: &str) -> String {
    format!(
        "**Vercelify preview** for `{app}` is live: {url}\n\nBuilt from {} on your own server. Updates on every push; removed when the pull request closes.",
        &sha[..sha.len().min(7)]
    )
}

/// Path inside the repository for a file of an app built from `base_dir`
/// (the app's build context, Vercel's "root directory").
pub fn repo_path(base_dir: &str, file: &str) -> Option<String> {
    let base = base_dir.trim().trim_matches('/');
    if base.split('/').any(|s| s == ".." || s == ".") || base.contains(['?', '#', '"', ' ', '\\']) {
        return None;
    }
    Some(if base.is_empty() {
        file.to_string()
    } else {
        format!("{base}/{file}")
    })
}

/// A file's text from the contents API body. None: not a file.
pub fn parse_content(body: &str) -> Result<Option<String>, String> {
    use base64::Engine;
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("GitHub: {e}"))?;
    if v.get("type").and_then(|t| t.as_str()) != Some("file") {
        return Ok(None);
    }
    if v.get("encoding").and_then(|e| e.as_str()) != Some("base64") {
        return Err("the file is too large to read through GitHub's API".into());
    }
    let b64: String = v
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("GitHub sent a broken file: {e}"))?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "the file is not UTF-8 text".into())
}

/// A file at a commit or branch (empty: the default branch). None: it
/// doesn't exist there, or the repository can't be read (see `readable`).
pub async fn file_at(
    cfg: &Config,
    slug: &str,
    path: &str,
    git_ref: &str,
    token: Option<&str>,
) -> Result<Option<String>, String> {
    if !git_ref.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)) {
        return Err(format!("invalid ref {git_ref}"));
    }
    let query = if git_ref.is_empty() {
        String::new()
    } else {
        format!("?ref={git_ref}")
    };
    let (code, body) = api(
        cfg,
        "GET",
        &format!("/repos/{slug}/contents/{path}{query}"),
        token,
        None,
    )
    .await?;
    match code {
        200 => parse_content(&body),
        404 => Ok(None),
        _ => Err(error_of(code, &body)),
    }
}

/// The repository can be read with this token (GitHub answers 404 for
/// both a missing file and a repository it won't show).
pub async fn readable(cfg: &Config, slug: &str, token: Option<&str>) -> Result<(), String> {
    let (code, body) = api(cfg, "GET", &format!("/repos/{slug}"), token, None).await?;
    if code == 200 {
        Ok(())
    } else {
        Err(error_of(code, &body))
    }
}

/// What changed between two commits.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Changes {
    pub files: Vec<String>,
    pub messages: Vec<String>,
    /// GitHub lists at most 300 files: the list may be incomplete.
    pub truncated: bool,
}

pub fn parse_compare(body: &str) -> Result<Changes, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("GitHub: {e}"))?;
    let mut files = Vec::new();
    let list = v.get("files").and_then(|f| f.as_array()).cloned().unwrap_or_default();
    for f in &list {
        for k in ["filename", "previous_filename"] {
            if let Some(n) = f.get(k).and_then(|n| n.as_str()) {
                files.push(n.to_string());
            }
        }
    }
    let messages = v
        .get("commits")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| c.pointer("/commit/message")?.as_str().map(String::from))
        .collect();
    Ok(Changes {
        truncated: list.len() >= 300 || v.get("files").is_none(),
        files,
        messages,
    })
}

pub async fn compare(cfg: &Config, slug: &str, base: &str, head: &str, token: Option<&str>) -> Result<Changes, String> {
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit());
    if !ok(base) || !ok(head) {
        return Err("invalid commit".into());
    }
    let (code, body) = api(
        cfg,
        "GET",
        &format!("/repos/{slug}/compare/{base}...{head}"),
        token,
        None,
    )
    .await?;
    if code != 200 {
        return Err(error_of(code, &body));
    }
    parse_compare(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(repo_slug("https://github.com/you/app.git").as_deref(), Some("you/app"));
        assert_eq!(repo_slug("https://github.com/you/app").as_deref(), Some("you/app"));
        assert_eq!(repo_slug("https://gitlab.com/you/app"), None);
        assert_eq!(repo_slug("https://github.com/you/app/tree/x"), None);
        assert_eq!(repo_slug("https://github.com/you/a b"), None);
    }

    #[test]
    fn pulls() {
        let j = r#"[
          {"number":7,"title":"Add login","head":{"sha":"abc","repo":{"full_name":"you/app"}},"base":{"sha":"x","repo":{"full_name":"you/app"}}},
          {"number":8,"title":"evil","head":{"sha":"def","repo":{"full_name":"stranger/app"}},"base":{"sha":"x","repo":{"full_name":"you/app"}}},
          {"number":9,"title":"gone fork","head":{"sha":"fff","repo":null},"base":{"sha":"x","repo":{"full_name":"you/app"}}}
        ]"#;
        let p = parse_pulls(j).unwrap();
        assert_eq!(p.len(), 3);
        assert!(!p[0].from_fork);
        assert!(p[1].from_fork);
        assert!(p[2].from_fork);
        assert!(parse_pulls("{}").is_err());
    }

    #[test]
    fn contents_and_paths() {
        assert_eq!(repo_path("", "vercel.json").as_deref(), Some("vercel.json"));
        assert_eq!(
            repo_path("/apps/web/", "vercel.json").as_deref(),
            Some("apps/web/vercel.json")
        );
        assert_eq!(repo_path("../x", "vercel.json"), None);
        let body = r#"{"type":"file","encoding":"base64","content":"eyJjbGVhblVy\nbHMiOnRydWV9\n"}"#;
        assert_eq!(parse_content(body).unwrap().as_deref(), Some(r#"{"cleanUrls":true}"#));
        assert_eq!(parse_content(r#"[{"type":"file"}]"#).unwrap(), None);
        assert!(parse_content(r#"{"type":"file","encoding":"none","content":""}"#).is_err());
    }

    #[test]
    fn compare_lists_files_and_messages() {
        let body = r#"{"commits":[{"commit":{"message":"docs: typo [skip deploy]"}}],
          "files":[{"filename":"docs/a.md"},{"filename":"apps/web/x.ts","previous_filename":"apps/api/x.ts"}]}"#;
        let c = parse_compare(body).unwrap();
        assert_eq!(c.files, vec!["docs/a.md", "apps/web/x.ts", "apps/api/x.ts"]);
        assert_eq!(c.messages, vec!["docs: typo [skip deploy]"]);
        assert!(!c.truncated);
        assert!(parse_compare(r#"{"commits":[]}"#).unwrap().truncated);
    }

    #[test]
    fn comment_text() {
        let c = preview_comment("blog", "https://pr-7-blog.example.com", "abcdef1234");
        assert!(c.contains("https://pr-7-blog.example.com") && c.contains("abcdef1"));
    }
}
