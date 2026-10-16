//! `fsread.df` — `df` für das Dateisystem unter einem Workspace-Pfad.
//!
//! `fstatvfs` auf einem symlinkfrei geöffneten Deskriptor liefert Größe,
//! freien und verfügbaren Platz sowie Inodes. Dateisystemtyp und Quellgerät
//! kommen aus `/proc/self/mountinfo` (Mount mit dem längsten passenden
//! Präfix). Der Mountpunkt selbst wird nicht ausgegeben (er liegt außerhalb
//! des Workspace).

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{flag, ok};
use crate::meta::human_size;
use crate::scope::io_message;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::json;
use std::io::Read;
use std::path::Path;

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.df";

/// Höchstgröße der gelesenen `mountinfo`.
const MAX_MOUNTINFO_BYTES: u64 = 2 * 1024 * 1024;

/// Argumente für `fsread.df`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct DfArgs {
    /// Path whose filesystem is reported, relative to the workspace root (default: the root).
    #[serde(default)]
    pub path: Option<String>,
    /// -h: add human-readable sizes (1024-based).
    #[serde(default)]
    pub human: Option<bool>,
}

/// Ein Eintrag aus `mountinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    /// Mountpunkt (Escape-Folgen aufgelöst).
    pub mount_point: String,
    /// Dateisystemtyp (`ext4`, `tmpfs`, …).
    pub fs_type: String,
    /// Quelle (`/dev/vda`, `tmpfs`, …).
    pub source: String,
    /// Mount-Optionen (`rw,relatime`).
    pub options: String,
}

/// Löst `\040`-artige Oktal-Escapes in `mountinfo`-Feldern auf.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            if let Some(digits) = bytes.get(index + 1..index + 4) {
                if digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                    let value = digits
                        .iter()
                        .fold(0u32, |acc, d| acc * 8 + u32::from(d - b'0'));
                    if let Ok(byte) = u8::try_from(value) {
                        out.push(byte);
                        index += 4;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parst `/proc/self/mountinfo`; kaputte Zeilen werden übersprungen.
#[must_use]
pub fn parse_mountinfo(text: &str) -> Vec<MountEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let left: Vec<&str> = left.split(' ').collect();
        let right: Vec<&str> = right.split(' ').collect();
        if left.len() < 6 || right.len() < 2 {
            continue;
        }
        entries.push(MountEntry {
            mount_point: unescape(left[4]),
            options: left[5].to_owned(),
            fs_type: right[0].to_owned(),
            source: unescape(right[1]),
        });
    }
    entries
}

/// Wählt den Mount mit dem längsten Präfix von `absolute`.
#[must_use]
pub fn best_mount<'a>(mounts: &'a [MountEntry], absolute: &Path) -> Option<&'a MountEntry> {
    mounts
        .iter()
        .filter(|mount| absolute.starts_with(&mount.mount_point))
        .max_by_key(|mount| Path::new(&mount.mount_point).components().count())
}

fn read_mountinfo() -> String {
    let Ok(file) = std::fs::File::open("/proc/self/mountinfo") else {
        return String::new();
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_MOUNTINFO_BYTES)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Führt `fsread.df` aus.
#[must_use]
pub fn run(root: &Path, args: &DfArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        let human = flag(args.human);
        let input = args
            .path
            .as_deref()
            .filter(|p| !p.is_empty())
            .unwrap_or(".");
        let rel = scope.rel(input).map_err(|e| e.to_string())?;
        // Für Dateien gilt das Dateisystem des Elternverzeichnisses.
        let stat = scope
            .lstat(&rel)
            .map_err(|e| format!("cannot access '{}': {}", rel.display(), io_message(&e)))?;
        let is_dir = crate::meta::Meta::from_stat(&stat).kind == crate::meta::Kind::Dir;
        let dir_rel = if is_dir {
            rel.clone()
        } else {
            let mut parent = crate::scope::RelPath::root();
            if let Some(p) = rel.as_path().parent() {
                for component in p.components() {
                    parent = parent.join(component.as_os_str());
                }
            }
            parent
        };
        let fd = scope
            .open_dir(&dir_rel)
            .map_err(|e| format!("cannot open '{}': {}", dir_rel.display(), io_message(&e)))?;
        let vfs = rustix::fs::fstatvfs(&fd).map_err(|e| format!("statvfs failed: {e}"))?;
        let frsize = if vfs.f_frsize > 0 {
            vfs.f_frsize
        } else {
            vfs.f_bsize
        };
        let total = vfs.f_blocks.saturating_mul(frsize);
        let free = vfs.f_bfree.saturating_mul(frsize);
        let avail = vfs.f_bavail.saturating_mul(frsize);
        let used = total.saturating_sub(free);
        // Wie `df`: Auslastung relativ zu used + avail.
        let denominator = used.saturating_add(avail);
        let used_percent = if denominator == 0 {
            0.0
        } else {
            // Rundung auf eine Nachkommastelle; Genauigkeitsverlust bei > 2^53 ist hier irrelevant.
            (used as f64 / denominator as f64 * 1000.0).round() / 10.0
        };
        let mounts = parse_mountinfo(&read_mountinfo());
        let absolute = scope.root().join(dir_rel.as_path());
        let mount = best_mount(&mounts, &absolute);
        let mut value = json!({
            "path": rel.display(),
            "block_size": frsize,
            "total_bytes": total,
            "used_bytes": used,
            "free_bytes": free,
            "available_bytes": avail,
            "used_percent": used_percent,
            "inodes_total": vfs.f_files,
            "inodes_free": vfs.f_ffree,
            "inodes_used": vfs.f_files.saturating_sub(vfs.f_ffree),
            "name_max": vfs.f_namemax,
            "read_only": vfs.f_flag.contains(rustix::fs::StatVfsMountFlags::RDONLY),
            "fs_type": mount.map(|m| m.fs_type.clone()),
            "source": mount.map(|m| m.source.clone()),
            "truncated": false,
        });
        if human {
            value["total_human"] = json!(human_size(total));
            value["used_human"] = json!(human_size(used));
            value["available_human"] = json!(human_size(avail));
        }
        let summary = format!(
            "{} used of {} ({}%), {} available",
            human_size(used),
            human_size(total),
            used_percent,
            human_size(avail)
        );
        Ok(ok(TOOL, summary, value))
    })
}

/// Zeigt Dateisystem-Belegung wie `df`.
#[harw_macros::tool(
    name = "fsread.df",
    description = "Reports size, used, free and available bytes plus inode counts of the filesystem that holds a workspace path, like df (-h via human=true). Use when you need free disk space or inode exhaustion instead of running df. Returns JSON {total_bytes, used_bytes, available_bytes, used_percent, inodes_*, fs_type, source, read_only}. The mount point itself is not reported.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_df(context: &ToolExecutionContext, args: DfArgs) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestResult, error_of, json_of};
    use serde_json::Value;

    #[test]
    fn parses_mountinfo_and_picks_longest_prefix() -> TestResult {
        let text = "\
22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
23 22 0:20 / /mnt/my\\040disk rw - tmpfs tmpfs rw,size=4k\n\
broken line\n\
24 22 0:21 / /home rw - xfs /dev/sdb1 rw\n";
        let mounts = parse_mountinfo(text);
        assert_eq!(mounts.len(), 3);
        assert_eq!(mounts[1].mount_point, "/mnt/my disk");
        let best = best_mount(&mounts, Path::new("/home/user/ws"))
            .ok_or(crate::test_support::TestError::Missing("mount"))?;
        assert_eq!(best.fs_type, "xfs");
        let best = best_mount(&mounts, Path::new("/mnt/my disk/x"))
            .ok_or(crate::test_support::TestError::Missing("mount"))?;
        assert_eq!(best.fs_type, "tmpfs");
        let best = best_mount(&mounts, Path::new("/etc"))
            .ok_or(crate::test_support::TestError::Missing("mount"))?;
        assert_eq!(best.fs_type, "ext4");
        Ok(())
    }

    #[test]
    fn reports_real_filesystem_numbers() -> TestResult {
        let fx = Fixture::new()?;
        fx.write("f", b"x")?;
        for path in [Value::Null, Value::String("f".into())] {
            let args: DfArgs = serde_json::from_value(json!({"path": path, "human": true}))?;
            let value = json_of(run(&fx.ws, &args))?;
            let total = value["total_bytes"].as_u64().unwrap_or(0);
            let used = value["used_bytes"].as_u64().unwrap_or(u64::MAX);
            let avail = value["available_bytes"].as_u64().unwrap_or(u64::MAX);
            assert!(total > 0, "{value}");
            assert!(used <= total && avail <= total);
            assert!(value["total_human"].is_string());
            assert!(value["fs_type"].is_string() || value["fs_type"].is_null());
            assert!(
                value.get("mount_point").is_none(),
                "mount point must not leak"
            );
        }
        Ok(())
    }

    #[test]
    fn escapes_are_rejected() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        for bad in [
            json!({"path": ".."}),
            json!({"path": "link_dir/deep"}),
            json!({"path": "missing"}),
        ] {
            error_of(run(&fx.ws, &serde_json::from_value(bad)?))?;
        }
        Ok(())
    }
}
