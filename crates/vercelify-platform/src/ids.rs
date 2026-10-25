//! Resource identifiers and names.
//!
//! Ids are `<prefix>_<16 chars Crockford base32>` (80 random bits): readable
//! in logs, unambiguous across tables, safe in URLs, file names and Docker
//! object names. Names that become hostnames are DNS labels.

/// Crockford base32: no I, L, O, U, so ids survive being read aloud.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

pub const PREFIXES: &[&str] = &[
    "prj", "env", "app", "dom", "var", "dpl", "prv", "db", "bak", "vol", "node", "mn", "git", "hook", "task", "tok",
];

/// A new random id with this prefix.
pub fn new_id(prefix: &str) -> String {
    debug_assert!(PREFIXES.contains(&prefix), "unknown id prefix {prefix}");
    let mut raw = [0u8; 10];
    getrandom::fill(&mut raw).expect("OS RNG available");
    let mut out = String::with_capacity(prefix.len() + 17);
    out.push_str(prefix);
    out.push('_');
    // 80 bits = 16 groups of 5 bits.
    let bits = raw.iter().fold(0u128, |acc, b| (acc << 8) | *b as u128);
    for i in (0..16).rev() {
        out.push(ALPHABET[((bits >> (i * 5)) & 31) as usize] as char);
    }
    out
}

/// Whether `s` looks like an id with this prefix.
pub fn valid_id(prefix: &str, s: &str) -> bool {
    s.strip_prefix(prefix)
        .and_then(|r| r.strip_prefix('_'))
        .is_some_and(|r| r.len() == 16 && r.bytes().all(|b| ALPHABET.contains(&b)))
}

pub const MAX_NAME_LEN: usize = 40;

/// App and database names: DNS labels of at most 40 characters, so
/// `pr-1234-<name>.<domain>` still fits in one 63-character label.
/// The same rule is a CHECK constraint in `resource_names`.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_NAME_LEN
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// Turn a free-form name ("My Blog!") into a valid one ("my-blog"), or None
/// if nothing usable is left.
pub fn slugify(s: &str) -> Option<String> {
    let mut out = String::new();
    for c in s.trim().chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.chars().take(MAX_NAME_LEN).collect();
    let out = out.trim_end_matches('-').to_string();
    valid_name(&out).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_the_documented_shape() {
        let a = new_id("app");
        assert_eq!(a.len(), 4 + 16);
        assert!(valid_id("app", &a));
        assert!(!valid_id("dpl", &a));
        assert!(!valid_id("app", "app_0123456789abcdei"), "i is not in the alphabet");
        assert!(!valid_id("app", "app_short"));
        assert_ne!(new_id("app"), new_id("app"));
    }

    #[test]
    fn names_are_dns_labels() {
        for ok in ["a", "blog", "my-blog-2", &"x".repeat(40)] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "-a", "a-", "A", "a_b", "a.b", &"x".repeat(41), "ü"] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("My Blog!").as_deref(), Some("my-blog"));
        assert_eq!(slugify("  --API v2--  ").as_deref(), Some("api-v2"));
        assert_eq!(slugify("!!!"), None);
        assert_eq!(
            slugify(&"abc ".repeat(20)).unwrap().len(),
            39,
            "trailing hyphen removed after truncation"
        );
    }
}
