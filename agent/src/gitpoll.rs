//! What push-to-deploy needs besides git (`platform_git.rs`): which pull
//! requests get a preview, and whether a push is worth a build at all.

use crate::github;

/// What the previews of a repository should become.
#[derive(Debug, PartialEq, Default)]
pub struct PreviewPlan {
    /// (number, head sha, title): deploy or redeploy.
    pub deploy: Vec<(u64, String, String)>,
    /// Pull requests that closed: remove their preview.
    pub remove: Vec<u64>,
    /// Fork pull requests left alone (someone else's code).
    pub skipped_forks: Vec<u64>,
    /// Over the per-app limit.
    pub over_limit: Vec<u64>,
}

pub fn plan_previews(open: &[github::PullRequest], live: &[(u64, String)], max: usize) -> PreviewPlan {
    let mut plan = PreviewPlan::default();
    let is_open = |n: u64| open.iter().any(|p| p.number == n && !p.from_fork);
    // Live previews keep their slot; new pull requests take what is left.
    let mut used = live.iter().filter(|(n, _)| is_open(*n)).count();
    for pr in open {
        if pr.from_fork {
            plan.skipped_forks.push(pr.number);
            continue;
        }
        match live.iter().find(|(n, _)| *n == pr.number) {
            Some((_, sha)) if *sha == pr.head_sha => {}
            Some(_) => plan.deploy.push((pr.number, pr.head_sha.clone(), pr.title.clone())),
            None if used < max => {
                used += 1;
                plan.deploy.push((pr.number, pr.head_sha.clone(), pr.title.clone()));
            }
            None => plan.over_limit.push(pr.number),
        }
    }
    for (n, _) in live {
        if !is_open(*n) {
            plan.remove.push(*n);
        }
    }
    plan
}

/// Watch-path globs: `*` and `?`
/// stay within a directory, `**` (and `**/`) cross them, `[…]`/`[!…]` are
/// classes.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    #[derive(Debug)]
    enum T {
        Lit(char),
        Star,
        Any,
        One,
        Class(bool, Vec<(char, char)>),
    }
    let mut toks = Vec::new();
    let p: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            '*' if p.get(i + 1) == Some(&'*') => {
                toks.push(T::Any);
                i += 2;
                if p.get(i) == Some(&'/') {
                    i += 1;
                }
                continue;
            }
            '*' => toks.push(T::Star),
            '?' => toks.push(T::One),
            '[' if p[i + 1..].contains(&']') => {
                i += 1;
                let neg = matches!(p.get(i), Some('!' | '^'));
                if neg {
                    i += 1;
                }
                let mut set = Vec::new();
                while i < p.len() && p[i] != ']' {
                    if p.get(i + 1) == Some(&'-') && p.get(i + 2).is_some_and(|c| *c != ']') {
                        set.push((p[i], p[i + 2]));
                        i += 3;
                    } else {
                        set.push((p[i], p[i]));
                        i += 1;
                    }
                }
                toks.push(T::Class(neg, set));
            }
            c => toks.push(T::Lit(c)),
        }
        i += 1;
    }
    fn go(t: &[T], s: &[char]) -> bool {
        match t.first() {
            None => s.is_empty(),
            Some(T::Lit(c)) => s.first() == Some(c) && go(&t[1..], &s[1..]),
            Some(T::One) => s.first().is_some_and(|c| *c != '/') && go(&t[1..], &s[1..]),
            Some(T::Class(neg, set)) => {
                s.first()
                    .is_some_and(|c| set.iter().any(|(a, b)| a <= c && c <= b) != *neg)
                    && go(&t[1..], &s[1..])
            }
            Some(T::Star) => (0..=s.len())
                .take_while(|&k| k == 0 || s[k - 1] != '/')
                .any(|k| go(&t[1..], &s[k..])),
            Some(T::Any) => (0..=s.len()).any(|k| go(&t[1..], &s[k..])),
        }
    }
    let s: Vec<char> = path.chars().collect();
    go(&toks, &s)
}

/// The last matching pattern decides (`!` excludes);
/// a file no pattern matches counts only when all patterns are exclusions.
pub fn watched(file: &str, patterns: &[&str]) -> bool {
    let mut decision = None;
    for p in patterns {
        let (exclude, pat) = match p.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, *p),
        };
        if glob_match(pat, file) {
            decision = Some(!exclude);
        }
    }
    decision.unwrap_or_else(|| patterns.iter().all(|p| p.starts_with('!')))
}

const SKIP_MARKERS: [&str; 4] = ["[skip ci]", "[ci skip]", "[skip deploy]", "[deploy skip]"];

/// Why a push needn't be built (Vercel's "Ignored Build Step"): the newest
/// commit asks so, or no changed file is in the app's watch paths. None:
/// build it (also whenever the list of changes may be incomplete).
pub fn skip_reason(changes: &github::Changes, watch_paths: Option<&str>) -> Option<String> {
    if let Some(msg) = changes.messages.last() {
        let lower = msg.to_ascii_lowercase();
        if let Some(m) = SKIP_MARKERS.iter().find(|m| lower.contains(*m)) {
            return Some(format!("the commit message says {m}"));
        }
    }
    let patterns: Vec<&str> = watch_paths
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if patterns.is_empty() || changes.truncated || changes.files.is_empty() {
        return None;
    }
    (!changes.files.iter().any(|f| watched(f, &patterns)))
        .then(|| "no changed file is in the app's watch paths".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("src/*", "src/a.ts"));
        assert!(!glob_match("src/*", "src/a/b.ts"));
        assert!(glob_match("src/**", "src/a/b.ts"));
        assert!(glob_match("apps/web/**/*.ts", "apps/web/x.ts"));
        assert!(glob_match("apps/web/**/*.ts", "apps/web/a/b/x.ts"));
        assert!(!glob_match("apps/web/**/*.ts", "apps/api/x.ts"));
        assert!(glob_match("file?.md", "file1.md"));
        assert!(!glob_match("file?.md", "file/.md"));
        assert!(glob_match("v[0-9].txt", "v3.txt"));
        assert!(!glob_match("v[!0-9].txt", "v3.txt"));
        assert!(glob_match("a.b", "a.b"));
        assert!(!glob_match("a.b", "axb"));
        assert!(glob_match("[x", "[x"));
    }

    #[test]
    fn watch_paths_last_match_wins() {
        let p = ["apps/web/**", "!apps/web/**/*.md"];
        assert!(watched("apps/web/page.tsx", &p));
        assert!(!watched("apps/web/README.md", &p));
        assert!(!watched("apps/api/x.ts", &p));
        // Only exclusions: everything else counts.
        assert!(watched("src/x.ts", &["!docs/**"]));
        assert!(!watched("docs/a.md", &["!docs/**"]));
    }

    #[test]
    fn ignored_build_step() {
        let ch = |files: &[&str], msg: &str| github::Changes {
            files: files.iter().map(|f| f.to_string()).collect(),
            messages: vec!["older [skip ci]".into(), msg.into()],
            truncated: false,
        };
        let wp = Some("apps/web/**\npackages/**\n");
        assert_eq!(skip_reason(&ch(&["apps/web/a.ts"], "feat"), wp), None);
        assert_eq!(skip_reason(&ch(&["packages/ui/b.ts"], "feat"), wp), None);
        assert!(
            skip_reason(&ch(&["apps/api/a.ts", "README.md"], "feat"), wp).is_some_and(|r| r.contains("watch paths"))
        );
        // Only the newest commit's message counts.
        assert_eq!(skip_reason(&ch(&["apps/web/a.ts"], "feat"), None), None);
        assert!(skip_reason(&ch(&["apps/web/a.ts"], "Docs [Skip Deploy]"), None)
            .is_some_and(|r| r.contains("[skip deploy]")));
        // Without watch paths or with an incomplete list: build.
        assert_eq!(skip_reason(&ch(&["x"], "feat"), Some("  \n")), None);
        let mut big = ch(&["apps/api/a.ts"], "feat");
        big.truncated = true;
        assert_eq!(skip_reason(&big, wp), None);
        assert_eq!(skip_reason(&ch(&[], "empty"), wp), None);
    }

    fn pr(n: u64, sha: &str, fork: bool) -> github::PullRequest {
        github::PullRequest {
            number: n,
            title: format!("PR {n}"),
            head_sha: sha.into(),
            from_fork: fork,
        }
    }

    #[test]
    fn preview_plans() {
        // new, updated, unchanged, closed, fork
        let open = vec![
            pr(1, "a", false),
            pr(2, "b2", false),
            pr(3, "c", false),
            pr(9, "x", true),
        ];
        let live = vec![(2, "b1".to_string()), (3, "c".to_string()), (4, "d".to_string())];
        let p = plan_previews(&open, &live, 5);
        assert_eq!(
            p.deploy,
            vec![
                (1, "a".to_string(), "PR 1".to_string()),
                (2, "b2".to_string(), "PR 2".to_string())
            ]
        );
        assert_eq!(p.remove, vec![4]);
        assert_eq!(p.skipped_forks, vec![9]);
        assert!(p.over_limit.is_empty());
        // limit: live previews keep their slot, new ones wait
        let p = plan_previews(&[pr(5, "e", false), pr(3, "c2", false)], &[(3, "c".into())], 1);
        assert_eq!(p.over_limit, vec![5]);
        assert_eq!(p.deploy, vec![(3, "c2".to_string(), "PR 3".to_string())]);
    }
}
