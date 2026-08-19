/// Pick a remote host. Flag wins, then PADDOCK_REMOTE, then config `remote`.
/// Empty strings are ignored.
pub fn resolve_remote(
    flag: Option<&str>,
    env: Option<&str>,
    config: Option<&str>,
) -> Option<String> {
    for raw in [flag, env, config] {
        if let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) {
            return Some(s.to_string());
        }
    }
    None
}

pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'-' | b'_' | b'.' | b'/' | b':'))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Remote argv after `paddock` (no --remote / --local).
pub fn remote_argv(cmd: Option<&str>, extra: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(c) = cmd {
        out.push(c.to_string());
    }
    out.extend(extra.iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_order() {
        assert_eq!(
            resolve_remote(Some("a"), Some("b"), Some("c")).as_deref(),
            Some("a")
        );
        assert_eq!(
            resolve_remote(Some(""), Some("b"), Some("c")).as_deref(),
            Some("b")
        );
        assert_eq!(
            resolve_remote(Some("  "), None, Some("c")).as_deref(),
            Some("c")
        );
        assert_eq!(resolve_remote(None, None, None), None);
    }

    #[test]
    fn quote_plain_and_spaces() {
        assert_eq!(shell_quote("context"), "context");
        assert_eq!(shell_quote("0.0.0.0:8000"), "0.0.0.0:8000");
        assert_eq!(shell_quote("a b"), "'a b'");
    }
}
