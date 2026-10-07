//! `git show` und der Baum-gegen-Baum-Vergleich.
//!
//! # Verantwortung
//! [`run`] zeigt ein Objekt:
//! - Commit: Kopf, Nachricht und Diff gegen den ersten Elternteil
//!   (Wurzel-Commits gegen den leeren Baum; bei Merges gegen den ersten
//!   Elternteil, kenntlich in `diff_against`),
//! - annotiertes Tag: Tagger, Nachricht und das gezeigte Ziel,
//! - Baum: Einträge wie `git ls-tree`,
//! - Blob (`rev:pfad`): Inhalt, gekürzt auf [`MAX_BLOB_BYTES`].
//!
//! Inhalte von Pfaden auf der Geheimnis-Denylist werden nie ausgegeben.
//!
//! [`tree_report`] ist der gemeinsame Vergleich zweier Bäume (auch für `git.diff`).

use crate::diffcore::{DiffReport, Format, Reader, RenderOpts, render};
use crate::fmtutil::{BODY_CLIP, commit_json, signature_json};
use crate::graph::read_commit;
use crate::object::{Kind, parse_tag, parse_tree};
use crate::odb::Odb;
use crate::oid::Oid;
use crate::pathspec::Pathspec;
use crate::refs::Refs;
use crate::rev::{Revs, split_path};
use crate::snapshot::{changes, from_flat};
use crate::tree::{flatten, lookup, safe_name};
use harw_tool_fsread::budget::{Collector, clip_text};
use harw_tool_fsread::scope::{Scope, is_secret_path};
use serde_json::{Value, json};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Höchstzahl Bytes eines gezeigten Blobs.
pub const MAX_BLOB_BYTES: usize = 32 * 1024;

/// Höchstzahl Baumeinträge.
pub const MAX_TREE_ENTRIES: usize = 1000;

/// Vergleicht zwei Bäume (`None` = leerer Baum).
///
/// # Errors
/// Meldung bei fehlenden Objekten.
pub fn tree_report(
    odb: &Odb<'_>,
    scope: &Scope,
    old: Option<&Oid>,
    new: Option<&Oid>,
    spec: &Pathspec,
    opts: &RenderOpts,
) -> Result<DiffReport, String> {
    let old = match old {
        Some(tree) => from_flat(flatten(odb, tree, spec)?),
        None => Default::default(),
    };
    let new = match new {
        Some(tree) => from_flat(flatten(odb, tree, spec)?),
        None => Default::default(),
    };
    render(&Reader { odb, scope }, &changes(&old, &new), opts)
}

/// Optionen für [`run`].
pub struct ShowOpts {
    /// Revision, optional `rev:pfad`.
    pub rev: String,
    /// Diff-Format für Commits.
    pub format: Format,
    /// `false` unterdrückt den Diff.
    pub diff: bool,
    /// Kontextzeilen.
    pub context: usize,
    /// Leerraum ignorieren.
    pub ignore_whitespace: bool,
    /// Pfadfilter für den Diff.
    pub spec: Pathspec,
    /// Höchstzahl Dateien.
    pub max_files: usize,
}

fn tree_listing(odb: &Odb<'_>, tree: &Oid, base: &[u8]) -> Result<Value, String> {
    let entries = parse_tree(&odb.read_kind(tree, Kind::Tree)?.data)?;
    let mut out = Collector::new(MAX_TREE_ENTRIES);
    for entry in &entries {
        if !safe_name(&entry.name) {
            return Err("tree contains an unsafe path name".to_owned());
        }
        let kind = if entry.is_tree() {
            "tree"
        } else if entry.is_gitlink() {
            "commit"
        } else {
            "blob"
        };
        let mut shown = String::from_utf8_lossy(base).into_owned();
        shown.push_str(&String::from_utf8_lossy(&entry.name));
        if entry.is_tree() {
            shown.push('/');
        }
        out.push(json!({"mode": format!("{:06o}", entry.mode), "type": kind, "oid": entry.oid.hex(), "name": shown}));
    }
    let truncated = out.truncated();
    Ok(json!({"entries": out.into_items(), "total_entries": entries.len(), "truncated": truncated}))
}

/// Zeigt ein Objekt.
///
/// # Errors
/// Meldung bei unbekannter Revision, fehlenden Objekten oder Geheimnispfaden.
pub fn run(
    odb: &Odb<'_>,
    refs: &Refs<'_>,
    scope: &Scope,
    opts: &ShowOpts,
) -> Result<Value, String> {
    let revs = Revs::new(odb, refs);
    let (rev, path) = split_path(&opts.rev);
    let oid = revs.object(rev)?;
    let render_opts = RenderOpts {
        format: opts.format,
        context: opts.context,
        ignore_whitespace: opts.ignore_whitespace,
        max_files: opts.max_files,
    };
    if let Some(path) = path {
        let tree = revs.peel_tree(oid)?;
        let normalized = path.trim_matches('/');
        if normalized.split('/').any(|p| p == ".." || p == ".git") {
            return Err("path must not contain '..' or .git".to_owned());
        }
        let (mode, found) = lookup(odb, &tree, normalized.as_bytes())?.ok_or_else(|| {
            format!(
                "path '{}' does not exist in {rev}",
                normalized.chars().take(128).collect::<String>()
            )
        })?;
        return match mode & 0o170_000 {
            0o040_000 => {
                let mut base = normalized.as_bytes().to_vec();
                if !base.is_empty() {
                    base.push(b'/');
                }
                let mut data = tree_listing(odb, &found, &base)?;
                data["kind"] = json!("tree");
                data["path"] = json!(normalized);
                Ok(data)
            }
            0o160_000 => Ok(json!({"kind": "gitlink", "path": normalized, "oid": found.hex()})),
            _ => {
                if is_secret_path(Path::new(std::ffi::OsStr::from_bytes(
                    normalized.as_bytes(),
                ))) {
                    return Err(format!(
                        "{normalized}: the path matches the secret-file denylist; contents are never shown"
                    ));
                }
                let blob = odb.read_kind(&found, Kind::Blob)?;
                let binary = blob.data.iter().take(8000).any(|b| *b == 0);
                let mut data = json!({"kind": "blob", "path": normalized, "oid": found.hex(), "size": blob.data.len(), "mode": format!("{mode:06o}"), "binary": binary});
                if !binary {
                    let (text, clipped) =
                        clip_text(&String::from_utf8_lossy(&blob.data), MAX_BLOB_BYTES);
                    data["content"] = json!(text);
                    data["truncated"] = json!(clipped);
                } else {
                    data["truncated"] = json!(false);
                }
                Ok(data)
            }
        };
    }
    let object = odb.read(&oid)?;
    match object.kind {
        Kind::Commit => {
            let commit = read_commit(odb, &oid)?;
            let mut data = commit_json(&oid, &commit, BODY_CLIP * 8);
            data["kind"] = json!("commit");
            if opts.diff {
                let parent_tree = match commit.parents.first() {
                    Some(parent) => Some(read_commit(odb, parent)?.tree),
                    None => None,
                };
                let report = tree_report(
                    odb,
                    scope,
                    parent_tree.as_ref(),
                    Some(&commit.tree),
                    &opts.spec,
                    &render_opts,
                )?;
                data["diff_against"] = json!(
                    commit
                        .parents
                        .first()
                        .map_or_else(|| "empty tree".to_owned(), Oid::hex)
                );
                report.into_json(opts.format, &mut data);
            }
            Ok(data)
        }
        Kind::Tag => {
            let tag = parse_tag(&object.data)?;
            let mut data = json!({
                "kind": "tag", "oid": oid.hex(), "name": tag.name, "target": tag.target.hex(), "target_kind": tag.target_kind.name(),
                "message": clip_text(tag.message.trim_end(), BODY_CLIP * 8).0,
            });
            if let Some(tagger) = &tag.tagger {
                data["tagger"] = signature_json(tagger);
            }
            if tag.target_kind == Kind::Commit && opts.diff {
                let inner = ShowOpts {
                    rev: tag.target.hex(),
                    format: opts.format,
                    diff: opts.diff,
                    context: opts.context,
                    ignore_whitespace: opts.ignore_whitespace,
                    spec: opts.spec.clone(),
                    max_files: opts.max_files,
                };
                data["target_object"] = run(odb, refs, scope, &inner)?;
            }
            Ok(data)
        }
        Kind::Tree => {
            let mut data = tree_listing(odb, &oid, b"")?;
            data["kind"] = json!("tree");
            data["oid"] = json!(oid.hex());
            Ok(data)
        }
        Kind::Blob => {
            Err("a bare blob id cannot be shown without its path; use <rev>:<path>".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Repo;
    use crate::test_support::{TestError, TestRepo, TestResult};

    fn opts(rev: &str) -> ShowOpts {
        ShowOpts {
            rev: rev.to_owned(),
            format: Format::Patch,
            diff: true,
            context: 3,
            ignore_whitespace: false,
            spec: Pathspec::all(),
            max_files: 100,
        }
    }

    fn show(repo: &TestRepo, opts: &ShowOpts) -> Result<Value, String> {
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        let refs = Refs::new(&opened);
        run(&odb, &refs, &opened.scope, opts)
    }

    fn setup() -> TestResult<(TestRepo, Oid, Oid)> {
        let repo = TestRepo::new()?;
        let f1: &[(&str, u32, &str)] = &[
            ("a.txt", 0o100_644, "1\n"),
            (".env", 0o100_644, "TOKEN=abc\n"),
            ("docs/readme.md", 0o100_644, "# hi\n"),
        ];
        let c1 = repo.commit_files(f1, &[], "root", 100)?;
        let f2: &[(&str, u32, &str)] = &[
            ("a.txt", 0o100_644, "1\n2\n"),
            (".env", 0o100_644, "TOKEN=zzz\n"),
            ("docs/readme.md", 0o100_644, "# hi\n"),
        ];
        let tree = repo.tree_from(f2)?;
        let c2 = repo.commit(tree, &[c1], "second\n\nlonger body", 200)?;
        repo.set_ref("refs/heads/main", c2)?;
        let tag_body = format!(
            "object {c2}\ntype commit\ntag rel\ntagger T <t@x> 300 +0100\n\nrelease notes\n"
        );
        let tag = repo.write_loose("tag", tag_body.as_bytes())?;
        repo.set_ref("refs/tags/rel", tag)?;
        Ok((repo, c1, c2))
    }

    #[test]
    fn commit_with_diff_and_root_commit() -> TestResult {
        let (repo, c1, _) = setup()?;
        let data = show(&repo, &opts("HEAD"))?;
        assert_eq!(data["kind"], "commit");
        assert_eq!(data["subject"], "second");
        assert_eq!(data["body"], "longer body");
        assert_eq!(data["diff_against"], c1.hex());
        assert_eq!(data["total_files"], 2);
        let patch = data["patch"].as_str().ok_or(TestError::Missing("patch"))?;
        assert!(patch.contains("+2\n"), "{patch}");
        assert!(
            !patch.contains("zzz") && !patch.contains("abc"),
            "secret leaked: {patch}"
        );
        let root = show(&repo, &opts(&c1.hex()))?;
        assert_eq!(root["diff_against"], "empty tree");
        assert_eq!(root["total_files"], 3);
        let mut quiet = opts("HEAD");
        quiet.diff = false;
        assert!(show(&repo, &quiet)?.get("patch").is_none());
        let mut names = opts("HEAD");
        names.format = Format::NameStatus;
        assert_eq!(
            show(&repo, &names)?["files"][0],
            json!({"status": "M", "path": ".env"})
        );
        Ok(())
    }

    #[test]
    fn tags_trees_and_blobs() -> TestResult {
        let (repo, _, c2) = setup()?;
        let tag = show(&repo, &opts("rel"))?;
        assert_eq!(tag["kind"], "tag");
        assert_eq!(tag["target"], c2.hex());
        assert_eq!(tag["tagger"]["date"], "1970-01-01T01:05:00+01:00");
        assert_eq!(tag["target_object"]["subject"], "second");
        let tree = show(&repo, &opts("HEAD^{tree}"))?;
        assert_eq!(tree["entries"][0]["name"], ".env");
        assert_eq!(tree["entries"][2]["name"], "docs/");
        assert_eq!(tree["entries"][2]["type"], "tree");
        let sub = show(&repo, &opts("HEAD:docs"))?;
        assert_eq!(sub["entries"][0]["name"], "docs/readme.md");
        let blob = show(&repo, &opts("HEAD:a.txt"))?;
        assert_eq!(blob["content"], "1\n2\n");
        assert_eq!(blob["size"], 4);
        assert_eq!(blob["truncated"], false);
        Ok(())
    }

    #[test]
    fn secrets_missing_paths_and_bad_input_are_refused() -> TestResult {
        let (repo, _, _) = setup()?;
        let error = show(&repo, &opts("HEAD:.env"))
            .err()
            .ok_or(TestError::Missing("error"))?;
        assert!(error.contains("secret"), "{error}");
        assert!(!error.contains("zzz"));
        for escape in ["HEAD:../x", "HEAD:.git/config", "HEAD:docs/../.env"] {
            let error = show(&repo, &opts(escape))
                .err()
                .ok_or(TestError::Missing("error"))?;
            assert!(error.contains("must not contain"), "{escape}: {error}");
        }
        for bad in ["HEAD:nope.txt", "nope", "HEAD:a.txt/x"] {
            assert!(show(&repo, &opts(bad)).is_err(), "{bad}");
        }
        let blob_id = hash_of(&repo)?;
        assert!(show(&repo, &opts(&blob_id)).is_err());
        Ok(())
    }

    fn hash_of(repo: &TestRepo) -> TestResult<String> {
        Ok(repo.blob("1\n2\n")?.hex())
    }

    #[test]
    fn large_blobs_are_clipped() -> TestResult {
        let repo = TestRepo::new()?;
        let big = "0123456789\n".repeat(10_000);
        repo.commit_files(&[("big.txt", 0o100_644, big.as_str())], &[], "big", 1)?;
        let data = show(&repo, &opts("HEAD:big.txt"))?;
        assert_eq!(data["truncated"], true);
        assert!(
            data["content"]
                .as_str()
                .is_some_and(|c| c.len() <= MAX_BLOB_BYTES)
        );
        assert_eq!(data["size"], 110_000);
        Ok(())
    }
}
