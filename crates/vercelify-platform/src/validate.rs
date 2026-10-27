//! Input rules shared by every write path. Messages say what is accepted.

use crate::{Error, Result};

fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(msg.into()))
}

/// A hostname for a domain: lowercase labels, at least two of them, no
/// wildcards (wildcards are a server setting, not an app domain).
pub fn host(raw: &str) -> Result<String> {
    let h = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    let h = h
        .strip_prefix("https://")
        .or_else(|| h.strip_prefix("http://"))
        .unwrap_or(&h)
        .trim_end_matches('/')
        .to_string();
    let labels: Vec<&str> = h.split('.').collect();
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    if h.len() > 253 || labels.len() < 2 || !labels.iter().all(|l| label_ok(l)) {
        return invalid(format!("`{raw}` is not a domain name like app.example.com"));
    }
    if labels.last().is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit())) {
        return invalid(format!(
            "`{raw}` is an IP address; use a name (the sslip.io names work)"
        ));
    }
    Ok(h)
}

/// Environment variable names: letters, digits, underscore, not starting with
/// a digit (what a shell and `docker run --env-file` accept).
pub fn env_key(k: &str) -> Result<()> {
    let ok = !k.is_empty()
        && k.len() <= 256
        && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !k.as_bytes()[0].is_ascii_digit();
    if ok {
        Ok(())
    } else {
        invalid(format!(
            "`{k}` is not a variable name (letters, digits and _, not starting with a digit)"
        ))
    }
}

pub const MAX_ENV_VALUE: usize = 64 * 1024;

pub fn env_value(k: &str, v: &str) -> Result<()> {
    if v.len() > MAX_ENV_VALUE {
        return invalid(format!("{k}: value longer than 64 KiB"));
    }
    if v.contains('\0') {
        return invalid(format!("{k}: value contains a NUL byte"));
    }
    Ok(())
}

/// Repositories we can fetch with hardened git: https, or ssh in scp form
/// (`git@host:owner/repo.git`) or `ssh://` URL. No local paths, no `file://`,
/// no `ext::`, no option injection.
pub fn repository(r: &str) -> Result<String> {
    let r = r.trim();
    let ok_chars = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~/:@%+".contains(&b))
    };
    let valid = if let Some(rest) = r.strip_prefix("https://") {
        rest.contains('/') && !rest.starts_with('/') && ok_chars(rest)
    } else if let Some(rest) = r.strip_prefix("ssh://") {
        rest.contains('/') && ok_chars(rest)
    } else if let Some((user_host, path)) = r.split_once(':') {
        user_host.contains('@')
            && !user_host.starts_with('-')
            && !path.is_empty()
            && !path.starts_with('/')
            && ok_chars(user_host)
            && ok_chars(path)
    } else {
        false
    };
    if valid && !r.starts_with('-') && r.len() <= 512 {
        Ok(r.to_string())
    } else {
        invalid(format!(
            "`{r}` is not a repository URL (https://github.com/me/app or git@github.com:me/app.git)"
        ))
    }
}

/// Git ref names we pass to git: branch-like, no option injection or `..`.
pub fn branch(b: &str) -> Result<()> {
    let ok = !b.is_empty()
        && b.len() <= 200
        && !b.starts_with('-')
        && !b.starts_with('/')
        && !b.ends_with('/')
        && !b.ends_with(".lock")
        && !b.contains("..")
        && !b.contains("@{")
        && b.bytes().all(|c| c.is_ascii_alphanumeric() || b"-._/".contains(&c));
    if ok {
        Ok(())
    } else {
        invalid(format!("`{b}` is not a branch name"))
    }
}

pub fn commit_sha(s: &str) -> Result<()> {
    if (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        invalid(format!("`{s}` is not a commit hash"))
    }
}

/// Image references for `docker pull`: `[registry/]name[:tag][@sha256:…]`.
pub fn image(i: &str) -> Result<()> {
    let ok = !i.is_empty()
        && i.len() <= 255
        && !i.starts_with('-')
        && i.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._/:@".contains(&b))
        && i.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric());
    if ok {
        Ok(())
    } else {
        invalid(format!(
            "`{i}` is not an image reference (nginx:1.27, ghcr.io/me/app:v2)"
        ))
    }
}

/// A path inside the repository: relative, no `..`, no leading `-`.
pub fn repo_path(what: &str, p: &str) -> Result<()> {
    let ok = !p.is_empty()
        && p.len() <= 400
        && !p.starts_with('/')
        && !p.starts_with('-')
        && !p.split('/').any(|seg| seg == "..")
        && !p.contains('\0')
        && !p.contains('\n');
    if ok {
        Ok(())
    } else {
        invalid(format!("{what}: `{p}` must be a path inside the repository"))
    }
}

/// Where a volume is mounted in a container.
pub fn mount_path(p: &str) -> Result<()> {
    let ok = p.starts_with('/')
        && p.len() > 1
        && p.len() <= 400
        && !p.split('/').any(|seg| seg == "..")
        && !p.contains([':', ',', '\0', '\n'])
        && !matches!(
            p.trim_end_matches('/'),
            "/proc" | "/sys" | "/dev" | "/etc" | "/usr" | "/bin" | "/lib"
        )
        // The kernel's own trees: runc refuses a mount inside /proc at deploy
        // time, and sysfs is no place for app data.
        && !["/proc/", "/sys/"].iter().any(|t| p.starts_with(t));
    if ok {
        Ok(())
    } else {
        invalid(format!("`{p}` can't be a mount path (an absolute path such as /data)"))
    }
}

pub fn port(p: u16) -> Result<()> {
    if p == 0 {
        invalid("the port must be between 1 and 65535")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(host("Blog.Example.com.").unwrap(), "blog.example.com");
        assert_eq!(host("https://blog.example.com/").unwrap(), "blog.example.com");
        assert_eq!(host("app.10.77.0.1.sslip.io").unwrap(), "app.10.77.0.1.sslip.io");
        for bad in [
            "localhost",
            "*.example.com",
            "a_b.example.com",
            "-a.example.com",
            "10.0.0.1",
            "",
            "a..b",
        ] {
            assert!(host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn env_keys() {
        for ok in ["A", "DATABASE_URL", "_X", "a1"] {
            env_key(ok).unwrap();
        }
        for bad in ["", "1A", "A-B", "A B", "A=B"] {
            assert!(env_key(bad).is_err(), "{bad}");
        }
        assert!(env_value("K", &"x".repeat(MAX_ENV_VALUE + 1)).is_err());
    }

    #[test]
    fn repositories() {
        for ok in [
            "https://github.com/me/blog",
            "https://github.com/me/blog.git",
            "git@github.com:me/blog.git",
            "ssh://git@gitea.lan:2222/me/blog.git",
        ] {
            repository(ok).unwrap();
        }
        for bad in [
            "/srv/repo",
            "file:///srv/repo",
            "ext::sh -c touch% /tmp/x",
            "-uhttps://x/y",
            "http://github.com/me/blog",
            "github.com:me/blog",
            "git@github.com:/etc",
            "https://github.com/me/blog; rm -rf /",
        ] {
            assert!(repository(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn refs_images_paths() {
        branch("main").unwrap();
        branch("feature/x-1").unwrap();
        for bad in ["-x", "a..b", "a b", "x.lock", "a@{1}", "/a"] {
            assert!(branch(bad).is_err(), "{bad}");
        }
        commit_sha("abc1234").unwrap();
        assert!(commit_sha("xyz1234").is_err());
        image("nginx:1.27").unwrap();
        image("ghcr.io/me/app@sha256:abc").unwrap();
        assert!(image("--privileged").is_err());
        repo_path("dockerfile", "docker/Dockerfile.prod").unwrap();
        assert!(repo_path("dockerfile", "../etc/passwd").is_err());
        assert!(repo_path("context", "/abs").is_err());
        mount_path("/data").unwrap();
        mount_path("/etc/nginx/conf.d").unwrap();
        for bad in ["data", "/", "/etc", "/a:/b", "/x/../etc", "/proc/x", "/sys/fs/cgroup"] {
            assert!(mount_path(bad).is_err(), "{bad}");
        }
    }
}
