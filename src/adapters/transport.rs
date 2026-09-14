//! The two ways an adapter reaches a program or a service: a child process
//! with stdin and stdout, or an HTTP POST. Nothing here knows what is said.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// Run `{cmd} {args...}` with `stdin`, return stdout. Non-zero exit is an error.
pub(crate) fn run(cmd: &str, args: &[String], stdin: &[u8]) -> Result<String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot run `{cmd}`"))?;
    if let Some(mut pipe) = child.stdin.take() {
        // A program may exit without reading its input; that is its business.
        match pipe.write_all(stdin) {
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            other => other?,
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "`{cmd}` exited {}: {}",
            out.status.code().unwrap_or(-1),
            err.trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// POST a JSON body, return the response body as text.
pub(crate) fn post(url: &str, bearer: Option<&str>, body: &serde_json::Value) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("paddock/0.1")
        .timeout(HTTP_TIMEOUT)
        .build()?;
    let mut req = client.post(url).json(body);
    if let Some(k) = bearer {
        req = req.bearer_auth(k);
    }
    let resp = req.send().with_context(|| format!("post {url}"))?;
    if !resp.status().is_success() {
        bail!("http {} from {url}", resp.status());
    }
    Ok(resp.text()?)
}

/// POST JSON, parse JSON back.
pub(crate) fn post_json(
    url: &str,
    bearer: Option<&str>,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let text = post(url, bearer, body)?;
    serde_json::from_str(&text).with_context(|| format!("{url}: reply is not JSON"))
}

/// A secret from the settings: `NAME` inline, or `NAME_cmd`, a command whose
/// stdout is the secret, so the config file never holds it.
pub(crate) fn secret(settings: &crate::kernel::Settings, name: &str) -> Result<Option<String>> {
    if let Some(v) = crate::kernel::setting(settings, name) {
        return Ok(Some(v));
    }
    let Some(cmd) = crate::kernel::setting(settings, &format!("{name}_cmd")) else {
        return Ok(None);
    };
    let out = run("sh", &["-c".to_string(), cmd.clone()], b"")
        .with_context(|| format!("{name}_cmd `{cmd}`"))?;
    let out = out.trim();
    if out.is_empty() {
        bail!("{name}_cmd `{cmd}` printed nothing");
    }
    Ok(Some(out.to_string()))
}

pub(crate) fn join(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_inline_or_from_a_command() {
        let inline: crate::kernel::Settings = [("key".to_string(), serde_json::json!("k1"))]
            .into_iter()
            .collect();
        assert_eq!(secret(&inline, "key").unwrap(), Some("k1".into()));
        let cmd: crate::kernel::Settings =
            [("key_cmd".to_string(), serde_json::json!("printf '  k2\n'"))]
                .into_iter()
                .collect();
        assert_eq!(secret(&cmd, "key").unwrap(), Some("k2".into()));
        let empty: crate::kernel::Settings = [("key_cmd".to_string(), serde_json::json!("true"))]
            .into_iter()
            .collect();
        assert!(secret(&empty, "key").is_err());
        assert_eq!(secret(&Default::default(), "key").unwrap(), None);
    }

    #[test]
    fn run_pipes_stdin_and_fails_loudly() {
        let out = run("sh", &["-c".into(), "cat".into()], b"hello").unwrap();
        assert_eq!(out, "hello");
        let err = run("sh", &["-c".into(), "echo bad >&2; exit 3".into()], b"").unwrap_err();
        assert!(err.to_string().contains("exited 3"), "{err}");
        assert!(run("paddock-no-such-cmd", &[], b"").is_err());
    }
}
