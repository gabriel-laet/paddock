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
        pipe.write_all(stdin)?;
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
    fn run_pipes_stdin_and_fails_loudly() {
        let out = run("sh", &["-c".into(), "cat".into()], b"hello").unwrap();
        assert_eq!(out, "hello");
        let err = run("sh", &["-c".into(), "echo bad >&2; exit 3".into()], b"").unwrap_err();
        assert!(err.to_string().contains("exited 3"), "{err}");
        assert!(run("paddock-no-such-cmd", &[], b"").is_err());
    }
}
