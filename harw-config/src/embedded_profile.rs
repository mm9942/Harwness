//! Secrets-free embedded default profile base layer.
use include_dir::{Dir, DirEntry, include_dir};

static PROFILE: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/embedded/profiles/default");

fn decode_name(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn collect(dir: &Dir<'static>, prefix: &str, out: &mut Vec<(String, String)>) {
    for entry in dir.entries() {
        match entry {
            DirEntry::Dir(sub) => {
                let name = sub
                    .path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                let next = if prefix.is_empty() {
                    name.to_owned()
                } else {
                    format!("{prefix}/{name}")
                };
                collect(sub, &next, out);
            }
            DirEntry::File(file) => {
                if let Some(text) = file.contents_utf8() {
                    let name = file
                        .path()
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default();
                    let decoded = decode_name(name);
                    let path = if prefix.is_empty() {
                        decoded
                    } else {
                        format!("{prefix}/{decoded}")
                    };
                    out.push((path, text.to_owned()));
                }
            }
        }
    }
}

/// Returns all embedded default-profile files as (relative path, TOML text).
pub fn embedded_profile_layer() -> Vec<(String, String)> {
    let mut files = Vec::new();
    collect(&PROFILE, "", &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}
