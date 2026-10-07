//! `git branch` (lesend): Zweige, Remote-Zweige und Tags.
//!
//! # Verantwortung
//! [`run`] listet lokale Zweige (und mit `remotes`/`tags` auch
//! `refs/remotes/*` und `refs/tags/*`) alphabetisch mit Spitze, Betreff des
//! Spitzen-Commits, Markierung des aktuellen Zweigs und – für lokale Zweige
//! mit konfiguriertem Upstream – Vorsprung/Rückstand.
//!
//! # Grenzen
//! Nur lesend (kein Anlegen/Löschen/Umbenennen). Vorsprung/Rückstand wird nur
//! für bis zu [`MAX_AHEAD_BEHIND`] Zweige berechnet und ist `null`, wenn die
//! Historie zu groß ist. Der Filter `pattern` kennt `*` und `?`.
//! Remote-URLs werden nie ausgegeben.

use crate::config::Config;
use crate::fmtutil::{deadline, iso_with_tz};
use crate::graph::{ahead_behind, read_commit};
use crate::odb::Odb;
use crate::oid::Oid;
use crate::refs::{Head, Refs};
use crate::repo::Repo;
use crate::rev::Revs;
use harw_tool_fsread::budget::Collector;
use serde_json::{Value, json};

/// Höchstzahl Zweige mit Vorsprung/Rückstand.
pub const MAX_AHEAD_BEHIND: usize = 50;

/// Standard-/Höchstzahl Einträge.
pub const DEFAULT_LIMIT: usize = 200;
/// Obergrenze.
pub const HARD_LIMIT: usize = 2000;

/// Optionen für [`run`].
pub struct BranchOpts {
    /// Remote-Zweige einschließen.
    pub remotes: bool,
    /// Tags einschließen.
    pub tags: bool,
    /// Muster mit `*`/`?`.
    pub pattern: Option<String>,
    /// Höchstzahl Einträge.
    pub limit: usize,
}

/// Einfacher Glob (`*`, `?`) über das Ganze.
#[must_use]
pub fn glob(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match p.split_first() {
            None => t.is_empty(),
            Some(('*', rest)) => (0..=t.len()).any(|skip| go(rest, &t[skip..])),
            Some(('?', rest)) => t.split_first().is_some_and(|(_, tail)| go(rest, tail)),
            Some((c, rest)) => t
                .split_first()
                .is_some_and(|(x, tail)| x == c && go(rest, tail)),
        }
    }
    let p: Vec<char> = pattern.chars().take(256).collect();
    let t: Vec<char> = text.chars().collect();
    // Begrenzung gegen exponentielle Muster: höchstens 3 Sterne.
    if p.iter().filter(|c| **c == '*').count() > 3 {
        return false;
    }
    go(&p, &t)
}

/// Listet Zweige.
///
/// # Errors
/// Meldung bei unlesbarem `HEAD`.
pub fn run(
    repo: &Repo,
    odb: &Odb<'_>,
    refs: &Refs<'_>,
    opts: &BranchOpts,
) -> Result<Value, String> {
    let head = refs.head()?;
    let current = match &head {
        Head::Branch { name, .. } => Some(name.clone()),
        Head::Detached(_) => None,
    };
    let config = Config::load(repo);
    let revs = Revs::new(odb, refs);
    let limit = opts.limit.clamp(1, HARD_LIMIT);
    let mut groups: Vec<(&str, String, &str)> = vec![("refs/heads/", "local".to_owned(), "branch")];
    if opts.remotes {
        groups.push(("refs/remotes/", "remote".to_owned(), "remote"));
    }
    if opts.tags {
        groups.push(("refs/tags/", "tag".to_owned(), "tag"));
    }
    let mut out = Collector::new(limit);
    let mut total = 0usize;
    let mut computed = 0usize;
    let end = deadline();
    for (prefix, kind, _) in groups {
        for (name, oid) in refs.list(prefix) {
            let short = name.strip_prefix(prefix).unwrap_or(&name).to_owned();
            if opts.pattern.as_ref().is_some_and(|p| !glob(p, &short)) {
                continue;
            }
            total += 1;
            let tip = revs.peel_commit(oid).unwrap_or(oid);
            let mut entry =
                json!({"name": short, "kind": kind, "oid": oid.hex(), "short": oid.short()});
            if let Ok(commit) = read_commit(odb, &tip) {
                entry["subject"] = json!(commit.subject().chars().take(200).collect::<String>());
                entry["date"] = json!(iso_with_tz(
                    commit.committer.when,
                    commit.committer.tz_minutes
                ));
            }
            if kind == "local" {
                entry["current"] = json!(current.as_deref() == Some(short.as_str()));
                if let Some((upstream_ref, upstream_short)) = config.upstream(&short) {
                    entry["upstream"] = json!(upstream_short);
                    let upstream_oid = refs.resolve(&upstream_ref).ok().flatten();
                    entry["upstream_gone"] = json!(upstream_oid.is_none());
                    let counts = match upstream_oid {
                        Some(up) if computed < MAX_AHEAD_BEHIND => {
                            computed += 1;
                            ahead_behind(odb, &tip, &up, end)
                        }
                        _ => None,
                    };
                    entry["ahead"] = counts.map_or(Value::Null, |(a, _)| json!(a));
                    entry["behind"] = counts.map_or(Value::Null, |(_, b)| json!(b));
                }
            }
            out.push(entry);
        }
    }
    let truncated = out.truncated();
    let reason = out.stop_reason();
    let (head_name, head_oid) = match &head {
        Head::Branch { name, oid } => (Some(name.clone()), *oid),
        Head::Detached(oid) => (None, Some(*oid)),
    };
    Ok(json!({
        "head": {"branch": head_name, "detached": current.is_none(), "oid": head_oid.map(|o: Oid| o.hex())},
        "branches": out.into_items(),
        "total": total,
        "truncated": truncated,
        "stop_reason": reason,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestRepo, TestResult};

    fn list(repo: &TestRepo, opts: &BranchOpts) -> TestResult<Value> {
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        let refs = Refs::new(&opened);
        Ok(run(&opened, &odb, &refs, opts)?)
    }

    fn opts() -> BranchOpts {
        BranchOpts {
            remotes: false,
            tags: false,
            pattern: None,
            limit: 100,
        }
    }

    fn setup() -> TestResult<TestRepo> {
        let repo = TestRepo::new()?;
        let c1 = repo.commit_files(&[("a", 0o100_644, "1\n")], &[], "one", 100)?;
        let tree = repo.tree_from(&[("a", 0o100_644, "2\n")])?;
        let c2 = repo.commit(tree, &[c1], "two", 200)?;
        let c3 = repo.commit(tree, &[c2], "three", 300)?;
        repo.set_ref("refs/heads/main", c3)?;
        repo.set_ref("refs/heads/feature/x", c2)?;
        repo.set_ref("refs/remotes/origin/main", c2)?;
        repo.set_ref("refs/tags/v1", c1)?;
        repo.write_git("config", "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n[branch \"feature/x\"]\n\tremote = origin\n\tmerge = refs/heads/gone\n[remote \"origin\"]\n\turl = https://user:SECRET@example.com/r.git\n")?;
        Ok(repo)
    }

    #[test]
    fn lists_local_branches_with_upstream_counts() -> TestResult {
        let repo = setup()?;
        let data = list(&repo, &opts())?;
        let names: Vec<&str> = data["branches"].as_array().map_or(vec![], |a| {
            a.iter().filter_map(|b| b["name"].as_str()).collect()
        });
        assert_eq!(names, vec!["feature/x", "main"]);
        let main = &data["branches"][1];
        assert_eq!(main["current"], true);
        assert_eq!(main["upstream"], "origin/main");
        assert_eq!(main["ahead"], 1);
        assert_eq!(main["behind"], 0);
        assert_eq!(main["subject"], "three");
        let feature = &data["branches"][0];
        assert_eq!(feature["upstream_gone"], true);
        assert!(feature["ahead"].is_null());
        assert_eq!(data["head"]["branch"], "main");
        let text = data.to_string();
        assert!(!text.contains("SECRET"), "remote url leaked");
        Ok(())
    }

    #[test]
    fn remotes_tags_pattern_and_limit() -> TestResult {
        let repo = setup()?;
        let mut o = opts();
        o.remotes = true;
        o.tags = true;
        let data = list(&repo, &o)?;
        assert_eq!(data["total"], 4);
        let kinds: Vec<&str> = data["branches"].as_array().map_or(vec![], |a| {
            a.iter().filter_map(|b| b["kind"].as_str()).collect()
        });
        assert_eq!(kinds, vec!["local", "local", "remote", "tag"]);
        let mut o = opts();
        o.pattern = Some("feat*".into());
        assert_eq!(list(&repo, &o)?["total"], 1);
        let mut o = opts();
        o.limit = 1;
        let data = list(&repo, &o)?;
        assert_eq!(data["truncated"], true);
        assert_eq!(data["branches"].as_array().map(Vec::len), Some(1));
        Ok(())
    }

    #[test]
    fn detached_and_unborn_heads() -> TestResult {
        let repo = setup()?;
        let tip = Oid::from_hex(
            list(&repo, &opts())?["branches"][1]["oid"]
                .as_str()
                .unwrap_or(""),
        );
        repo.head_detached(tip.unwrap_or(Oid::ZERO))?;
        let data = list(&repo, &opts())?;
        assert_eq!(data["head"]["detached"], true);
        assert_eq!(data["branches"][1]["current"], false);
        let empty = TestRepo::new()?;
        empty.head_branch("main")?;
        let data = list(&empty, &opts())?;
        assert_eq!(data["total"], 0);
        assert!(data["head"]["oid"].is_null());
        Ok(())
    }

    #[test]
    fn glob_semantics() -> TestResult {
        assert!(glob("*", "anything"));
        assert!(glob("fe*x", "feature/x"));
        assert!(glob("v?", "v1"));
        assert!(!glob("v?", "v10"));
        assert!(!glob("a", "b"));
        assert!(glob("", ""));
        assert!(!glob("********a", "b"));
        assert!(!glob(
            "a*a*a*a*a*a*a*a*a*b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        Ok(())
    }
}
