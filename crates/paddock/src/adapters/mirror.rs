//! A copy of the store somewhere else: an S3 bucket (Cloudflare R2, AWS, any
//! S3), or wherever a command puts it (scp, rsync, rclone). The store is
//! snapshotted first so the copy is consistent, and an encrypted store
//! stays encrypted, so an R2 bucket holds ciphertext.
//!
//! ```toml
//! [store.mirror]
//! kind = "s3"
//! url = "https://<account>.r2.cloudflarestorage.com"   # the endpoint; default AWS
//! bucket = "paddock"
//! path = "laptop/paddock.db"      # the object key
//! region = "auto"                 # R2 says auto; AWS wants a region
//! key_id = "..."
//! secret_cmd = "pass show r2"
//! after_pull = true               # push after every `paddock pull`
//!
//! [store.mirror]
//! kind = "exec"
//! push = "scp {file} box:paddock.db"    # {file} is the snapshot
//! pull = "scp box:paddock.db {file}"    # {file} is where the store goes
//! ```

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

use super::transport::{run, secret};
use crate::kernel::{setting, Settings};

pub enum Mirror {
    S3 {
        endpoint: String,
        bucket: String,
        key: String,
        region: String,
        key_id: String,
        secret: String,
    },
    Exec {
        push: String,
        pull: String,
    },
}

/// The `mirror` table of `[store]`, if any.
pub fn build(store: &Settings) -> Result<Option<Mirror>> {
    let Some(spec) = store.get("mirror").and_then(|v| v.as_object()) else {
        return Ok(None);
    };
    let need =
        |k: &str| setting(spec, k).ok_or_else(|| anyhow::anyhow!("[store.mirror] needs {k}"));
    Ok(Some(match setting(spec, "kind").as_deref().unwrap_or("") {
        "s3" => {
            let region = setting(spec, "region").unwrap_or_else(|| "us-east-1".into());
            Mirror::S3 {
                endpoint: setting(spec, "url")
                    .unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com")),
                bucket: need("bucket")?,
                key: setting(spec, "path").unwrap_or_else(|| "paddock.db".into()),
                region,
                key_id: need("key_id")?,
                secret: secret(spec, "secret")?
                    .ok_or_else(|| anyhow::anyhow!("[store.mirror] needs secret or secret_cmd"))?,
            }
        }
        "exec" => Mirror::Exec {
            push: need("push")?,
            pull: need("pull")?,
        },
        "" => bail!("[store.mirror] has no kind (s3, exec)"),
        other => bail!("unknown mirror kind `{other}` (s3, exec)"),
    }))
}

/// Whether `paddock pull` pushes when it is done.
pub fn after_pull(store: &Settings) -> bool {
    store
        .get("mirror")
        .and_then(|m| m.get("after_pull"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

impl Mirror {
    /// Where the copy goes, for the record.
    pub fn describe(&self) -> String {
        match self {
            Mirror::S3 { bucket, key, .. } => format!("s3://{bucket}/{key}"),
            Mirror::Exec { push, .. } => push.clone(),
        }
    }

    /// Copy `file` (a snapshot of the store) out.
    pub fn push(&self, file: &Path) -> Result<()> {
        match self {
            Mirror::S3 { .. } => {
                let bytes =
                    std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
                self.s3("PUT", Some(bytes)).map(|_| ())
            }
            Mirror::Exec { push, .. } => shell(push, file),
        }
    }

    /// Copy the store back into `file`.
    pub fn pull(&self, file: &Path) -> Result<()> {
        match self {
            Mirror::S3 { .. } => {
                let bytes = self.s3("GET", None)?;
                std::fs::write(file, bytes).with_context(|| format!("write {}", file.display()))
            }
            Mirror::Exec { pull, .. } => shell(pull, file),
        }
    }

    fn s3(&self, method: &str, body: Option<Vec<u8>>) -> Result<Vec<u8>> {
        let Mirror::S3 {
            endpoint,
            bucket,
            key,
            region,
            key_id,
            secret,
        } = self
        else {
            unreachable!()
        };
        let url = format!(
            "{}/{bucket}/{}",
            endpoint.trim_end_matches('/'),
            uri_encode(key, true)
        );
        let host = url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or_default()
            .to_string();
        let now = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host);
        headers.insert(
            "x-amz-content-sha256".to_string(),
            "UNSIGNED-PAYLOAD".to_string(),
        );
        headers.insert("x-amz-date".to_string(), now.clone());
        let path = format!("/{bucket}/{}", uri_encode(key, true));
        let auth = sign(method, &path, &headers, region, key_id, secret, &now);
        let client = reqwest::blocking::Client::builder()
            .user_agent("paddock/0.1")
            .timeout(std::time::Duration::from_secs(600))
            .build()?;
        let mut req = match method {
            "PUT" => client.put(&url).body(body.unwrap_or_default()),
            _ => client.get(&url),
        };
        for (k, v) in &headers {
            if k != "host" {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        let resp = req
            .header("authorization", auth)
            .send()
            .with_context(|| format!("{method} {url}"))?;
        let status = resp.status();
        let bytes = resp.bytes()?.to_vec();
        if !status.is_success() {
            bail!(
                "{method} {url}: http {status}: {}",
                String::from_utf8_lossy(&bytes)
                    .chars()
                    .take(300)
                    .collect::<String>()
            );
        }
        Ok(bytes)
    }
}

fn shell(template: &str, file: &Path) -> Result<()> {
    let cmd = template.replace("{file}", &file.display().to_string());
    run("sh", &["-c".to_string(), cmd.clone()], b"")
        .map(|_| ())
        .with_context(|| format!("mirror `{cmd}`"))
}

/// AWS Signature Version 4 for one request with no query string:
/// the `Authorization` header value.
fn sign(
    method: &str,
    path: &str,
    headers: &BTreeMap<String, String>,
    region: &str,
    key_id: &str,
    secret: &str,
    datetime: &str,
) -> String {
    let date = &datetime[..8];
    let signed: Vec<&str> = headers.keys().map(String::as_str).collect();
    let canonical_headers: String = headers
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let payload = headers
        .get("x-amz-content-sha256")
        .cloned()
        .unwrap_or_else(|| "UNSIGNED-PAYLOAD".into());
    let canonical = format!(
        "{method}\n{path}\n\n{canonical_headers}\n{}\n{payload}",
        signed.join(";")
    );
    let scope = format!("{date}/{region}/s3/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let k = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k = hmac(&k, region.as_bytes());
    let k = hmac(&k, b"s3");
    let k = hmac(&k, b"aws4_request");
    let signature = hex(&hmac(&k, to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={key_id}/{scope}, SignedHeaders={}, Signature={signature}",
        signed.join(";")
    )
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// RFC 3986 unreserved characters stay; `/` stays when `keep_slash`.
fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example in the S3 docs ("GET Object", Authorization header,
    /// single chunk): a known-good signature.
    #[test]
    fn the_signature_matches_the_s3_worked_example() {
        let mut headers = BTreeMap::new();
        headers.insert("host".into(), "examplebucket.s3.amazonaws.com".into());
        headers.insert("range".into(), "bytes=0-9".into());
        headers.insert(
            "x-amz-content-sha256".into(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        );
        headers.insert("x-amz-date".into(), "20130524T000000Z".into());
        let auth = sign(
            "GET",
            "/test.txt",
            &headers,
            "us-east-1",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "20130524T000000Z",
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn keys_are_encoded_and_specs_are_checked() {
        assert_eq!(uri_encode("a b/c~d.db", true), "a%20b/c~d.db");
        let s3: Settings = serde_json::from_str(
            r#"{"mirror": {"kind": "s3", "bucket": "b", "key_id": "k", "secret": "s", "url": "https://x.r2.cloudflarestorage.com/"}}"#,
        )
        .unwrap();
        let m = build(&s3).unwrap().unwrap();
        assert_eq!(m.describe(), "s3://b/paddock.db");
        let exec: Settings =
            serde_json::from_str(r#"{"mirror": {"kind": "exec", "push": "cp {file} /tmp/x", "pull": "cp /tmp/x {file}"}}"#)
                .unwrap();
        assert!(matches!(build(&exec).unwrap(), Some(Mirror::Exec { .. })));
        let bad: Settings = serde_json::from_str(r#"{"mirror": {"kind": "ftp"}}"#).unwrap();
        assert!(build(&bad).is_err());
        assert!(build(&Settings::new()).unwrap().is_none());
        assert!(!after_pull(&s3));
    }

    #[test]
    fn an_exec_mirror_pushes_and_pulls_through_the_shell() {
        let tmp = tempfile::tempdir().unwrap();
        let copy = tmp.path().join("copy.db");
        let m = Mirror::Exec {
            push: format!("cp {{file}} {}", copy.display()),
            pull: format!("cp {} {{file}}", copy.display()),
        };
        let src = tmp.path().join("src.db");
        std::fs::write(&src, b"store").unwrap();
        m.push(&src).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"store");
        let back = tmp.path().join("back.db");
        m.pull(&back).unwrap();
        assert_eq!(std::fs::read(&back).unwrap(), b"store");
    }
}
