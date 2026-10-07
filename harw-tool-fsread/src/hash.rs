//! `fsread.hash` — `sha256sum` und `b3sum` in reinem Rust.
//!
//! SHA-256 (`sha2`, streamend) und BLAKE3 (`harw-digest`, die Datei wird dafür
//! vollständig eingelesen). Beide sind auf [`MAX_STREAM_BYTES`] (64 MiB) je
//! Datei begrenzt; größere Dateien werden **abgelehnt**, nicht still gekürzt —
//! ein Hash über ein Präfix wäre irreführend. Symlinks und Geheimnis-Pfade
//! werden wie überall nicht gelesen.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{choice, ok};
use crate::io::{MAX_STREAM_BYTES, open_text};
use harw_digest::ContentDigest;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.hash";

/// Höchstzahl Pfade je Aufruf.
pub const MAX_PATHS: usize = 16;

/// Argumente für `fsread.hash`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct HashArgs {
    /// Files relative to the workspace root (1-16).
    pub paths: Vec<String>,
    /// Algorithm: 'sha256' (sha256sum, default) or 'blake3' (b3sum).
    #[serde(default)]
    pub algorithm: Option<String>,
}

/// Hex-Kodierung.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // `write!` auf `String` schlägt nie fehl.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// SHA-256 über einen Reader, höchstens `cap` Bytes. `Err` bei Überschreitung.
///
/// # Errors
/// I/O-Fehler oder Überschreitung von `cap`.
pub fn sha256_reader<R: Read>(reader: &mut R, cap: u64) -> Result<(String, u64), String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > cap {
            return Err(format!("file is larger than {cap} bytes"));
        }
        hasher.update(&buf[..read]);
    }
    Ok((hex(hasher.finalize().as_slice()), total))
}

/// BLAKE3 über einen Reader (vollständig eingelesen), höchstens `cap` Bytes.
///
/// # Errors
/// I/O-Fehler oder Überschreitung von `cap`.
pub fn blake3_reader<R: Read>(reader: &mut R, cap: u64) -> Result<(String, u64), String> {
    let mut data = Vec::new();
    reader
        .take(cap.saturating_add(1))
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    let len = data.len() as u64;
    if len > cap {
        return Err(format!("file is larger than {cap} bytes"));
    }
    Ok((hex(ContentDigest::of(&data).as_bytes()), len))
}

/// Führt `fsread.hash` aus.
#[must_use]
pub fn run(root: &Path, args: &HashArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        if args.paths.is_empty() {
            return Err("paths must contain at least one file".to_owned());
        }
        if args.paths.len() > MAX_PATHS {
            return Err(format!("too many paths (max {MAX_PATHS})"));
        }
        let algorithm = choice(
            "algorithm",
            args.algorithm.as_deref(),
            &["sha256", "blake3"],
            "sha256",
        )?;
        let mut results: Vec<Value> = Vec::new();
        for input in &args.paths {
            match open_text(scope, input) {
                Err(message) => results.push(json!({"path": input, "ok": false, "error": message})),
                Ok((rel, mut file)) => {
                    let hashed = if algorithm == "sha256" {
                        sha256_reader(&mut file, MAX_STREAM_BYTES)
                    } else {
                        blake3_reader(&mut file, MAX_STREAM_BYTES)
                    };
                    match hashed {
                        Ok((digest, size)) => results.push(json!({
                            "path": rel.display(),
                            "ok": true,
                            "algorithm": algorithm,
                            "digest": digest,
                            "size": size,
                        })),
                        Err(error) => results
                            .push(json!({"path": rel.display(), "ok": false, "error": error})),
                    }
                }
            }
        }
        let failed = results.iter().filter(|r| r["ok"] == false).count();
        Ok(ok(
            TOOL,
            format!(
                "{} {algorithm} digests, {failed} failed",
                results.len() - failed
            ),
            json!({"results": results, "failed": failed, "algorithm": algorithm, "truncated": false}),
        ))
    })
}

/// Berechnet Prüfsummen wie `sha256sum`/`b3sum`.
#[harw_macros::tool(
    name = "fsread.hash",
    description = "Computes SHA-256 (sha256sum) or BLAKE3 (b3sum) digests of up to 16 workspace files. Use when you need to fingerprint or compare files instead of running sha256sum or b3sum. Returns JSON {results:[{path, digest, size}], failed}. Files above 64 MiB are rejected rather than hashed partially; symlinks and secret files are never read.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_hash(
    context: &ToolExecutionContext,
    args: HashArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult, error_of, json_of};

    #[test]
    fn sha256_known_vectors() -> TestResult {
        let (empty, _) =
            sha256_reader(&mut &b""[..], 10).map_err(crate::test_support::TestError::Unexpected)?;
        assert_eq!(
            empty,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let (abc, size) = sha256_reader(&mut &b"abc"[..], 10)
            .map_err(crate::test_support::TestError::Unexpected)?;
        assert_eq!(
            abc,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(size, 3);
        Ok(())
    }

    #[test]
    fn blake3_known_vectors() -> TestResult {
        let (empty, _) =
            blake3_reader(&mut &b""[..], 10).map_err(crate::test_support::TestError::Unexpected)?;
        assert_eq!(
            empty,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        Ok(())
    }

    #[test]
    fn cap_rejects_oversized_input() -> TestResult {
        assert!(sha256_reader(&mut &b"abcdef"[..], 5).is_err());
        assert!(blake3_reader(&mut &b"abcdef"[..], 5).is_err());
        assert!(sha256_reader(&mut &b"abcde"[..], 5).is_ok());
        assert!(blake3_reader(&mut &b"abcde"[..], 5).is_ok());
        Ok(())
    }

    #[test]
    fn tool_hashes_files_and_reports_errors() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("a", b"abc")?;
        fx.write(".env", b"X=1")?;
        let value = json_of(run(
            &fx.ws,
            &serde_json::from_value(
                json!({"paths": ["a", "missing", "link_file", "../outside/secret.txt", ".env", "nested"]}),
            )?,
        ))?;
        assert_eq!(
            value["results"][0]["digest"],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(value["failed"], 5);
        let b3 = json_of(run(
            &fx.ws,
            &serde_json::from_value(json!({"paths": ["a"], "algorithm": "blake3"}))?,
        ))?;
        assert_eq!(b3["results"][0]["algorithm"], "blake3");
        assert_eq!(b3["results"][0]["digest"].as_str().map(str::len), Some(64));
        error_of(run(
            &fx.ws,
            &serde_json::from_value(json!({"paths": ["a"], "algorithm": "md5"}))?,
        ))?;
        error_of(run(&fx.ws, &serde_json::from_value(json!({"paths": []}))?))?;
        Ok(())
    }
}
