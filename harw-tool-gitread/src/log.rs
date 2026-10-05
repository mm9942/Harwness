//! `git log`: Commit-Historie mit Filtern.
//!
//! # Verantwortung
//! [`run`] läuft ab `rev` (Standard `HEAD`; auch `A..B`) in Git-Standard-
//! reihenfolge (Committer-Zeit, neueste zuerst) und liefert Commits als JSON.
//! Filter: `author` und `grep` (Teilstring, ohne Beachtung der Groß-/
//! Kleinschreibung), `since`/`until` (Committer-Datum), Pfadfilter,
//! `first_parent`, `no_merges`, `skip`, `max_count`.
//!
//! # Pfadfilter
//! Semantik wie `git log --full-history -- pfad`: ein Commit wird gezeigt,
//! wenn sein Baum sich unter den Pfaden von **mindestens einem** Elternteil
//! unterscheidet (Wurzel: wenn der Pfad existiert). Es gibt keine
//! Historienvereinfachung (das Standard-`git log -- pfad` folgt bei Merges nur
//! dem „treesame“-Elternteil und kann Commits auslassen) und keine
//! Umbenennungsverfolgung.
//!
//! # Grenzen
//! `max_count` höchstens [`HARD_MAX_COUNT`]; Ausgabe über [`Collector`]
//! begrenzt; Lauf höchstens [`crate::graph::MAX_WALK_COMMITS`] Commits und
//! 20 s. `A...B` und `--all` werden nicht unterstützt.

use crate::fmtutil::{BODY_CLIP, commit_json, deadline};
use crate::graph::{LogWalk, ancestors, read_commit};
use crate::object::Commit;
use crate::odb::Odb;
use crate::oid::Oid;
use crate::pathspec::Pathspec;
use crate::refs::Refs;
use crate::rev::Revs;
use crate::snapshot::{changes, from_flat};
use crate::tree::flatten;
use harw_tool_fsread::budget::Collector;
use serde_json::Value;
use std::collections::HashSet;

/// Standardzahl Commits.
pub const DEFAULT_MAX_COUNT: usize = 20;
/// Höchstzahl Commits.
pub const HARD_MAX_COUNT: usize = 500;

/// Optionen für [`run`].
pub struct LogOpts {
    /// Revision oder `A..B`.
    pub rev: String,
    /// Höchstzahl Commits.
    pub max_count: usize,
    /// Zu überspringende Treffer.
    pub skip: usize,
    /// Autor-Teilstring.
    pub author: Option<String>,
    /// Nachrichten-Teilstring.
    pub grep: Option<String>,
    /// Frühestes Committer-Datum.
    pub since: Option<i64>,
    /// Spätestes Committer-Datum.
    pub until: Option<i64>,
    /// Pfadfilter.
    pub spec: Pathspec,
    /// Nur dem ersten Elternteil folgen.
    pub first_parent: bool,
    /// Merge-Commits auslassen.
    pub no_merges: bool,
}

/// Ergebnis von [`run`].
pub struct LogResult {
    /// Commit-Einträge.
    pub commits: Vec<Value>,
    /// Es gab weitere Treffer, die nicht geliefert wurden.
    pub truncated: bool,
    /// Grund der Kürzung.
    pub reason: Option<&'static str>,
}

fn touches(odb: &Odb<'_>, commit: &Commit, spec: &Pathspec) -> Result<bool, String> {
    if spec.is_all() {
        return Ok(true);
    }
    let new = from_flat(flatten(odb, &commit.tree, spec)?);
    if commit.parents.is_empty() {
        return Ok(!new.is_empty());
    }
    // Wie `git log --full-history`: ein Commit zählt, sobald er sich von
    // mindestens einem Elternteil unterscheidet.
    for parent in &commit.parents {
        let old = from_flat(flatten(odb, &read_commit(odb, parent)?.tree, spec)?);
        if !changes(&old, &new).is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn wanted(commit: &Commit, opts: &LogOpts) -> bool {
    if opts.no_merges && commit.parents.len() > 1 {
        return false;
    }
    let when = commit.committer.when;
    if opts.since.is_some_and(|since| when < since) || opts.until.is_some_and(|until| when > until)
    {
        return false;
    }
    if let Some(author) = &opts.author {
        let hay = format!("{} <{}>", commit.author.name, commit.author.email).to_lowercase();
        if !hay.contains(&author.to_lowercase()) {
            return false;
        }
    }
    if let Some(grep) = &opts.grep {
        if !commit.message.to_lowercase().contains(&grep.to_lowercase()) {
            return false;
        }
    }
    true
}

/// Führt `git log` aus.
///
/// # Errors
/// Meldung bei unbekannter Revision, beschädigter Historie oder Fristüberschreitung.
pub fn run(odb: &Odb<'_>, refs: &Refs<'_>, opts: &LogOpts) -> Result<LogResult, String> {
    let revs = Revs::new(odb, refs);
    let limit = deadline();
    if opts.rev.contains("...") {
        return Err("symmetric ranges (A...B) are not supported; use A..B".to_owned());
    }
    let (tips, hidden): (Vec<Oid>, HashSet<Oid>) =
        if let Some((from, to)) = opts.rev.split_once("..") {
            let from = if from.is_empty() { "HEAD" } else { from };
            let to = if to.is_empty() { "HEAD" } else { to };
            let hidden = ancestors(odb, &[revs.commit(from)?], limit)?;
            (vec![revs.commit(to)?], hidden)
        } else {
            (vec![revs.commit(&opts.rev)?], HashSet::new())
        };
    let mut walk = LogWalk::new(odb, &tips, hidden, opts.first_parent, limit)?;
    let max_count = opts.max_count.clamp(1, HARD_MAX_COUNT);
    let mut out = Collector::new(max_count);
    let mut skipped = 0usize;
    let mut reason = None;
    while let Some((oid, commit)) = walk.next_commit()? {
        if !wanted(&commit, opts) || !touches(odb, &commit, &opts.spec)? {
            continue;
        }
        if skipped < opts.skip {
            skipped += 1;
            continue;
        }
        if !out.push(commit_json(&oid, &commit, BODY_CLIP)) {
            reason = out.stop_reason().or(Some("entry_limit"));
            break;
        }
        if out.len() >= max_count {
            if walk.has_more() {
                reason = Some("max_count");
            }
            break;
        }
    }
    let truncated = reason.is_some();
    Ok(LogResult {
        commits: out.into_items(),
        truncated,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Repo;
    use crate::test_support::{TestRepo, TestResult};

    fn setup() -> TestResult<TestRepo> {
        let repo = TestRepo::new()?;
        let f1: &[(&str, u32, &str)] =
            &[("a.txt", 0o100_644, "1\n"), ("src/x.rs", 0o100_644, "x\n")];
        let c1 = repo.commit_files(f1, &[], "first commit\n\nbody of first", 1_700_000_000)?;
        let f2: &[(&str, u32, &str)] =
            &[("a.txt", 0o100_644, "2\n"), ("src/x.rs", 0o100_644, "x\n")];
        let tree2 = repo.tree_from(f2)?;
        let c2 = repo.commit_as(
            tree2,
            &[c1],
            "touch a",
            1_700_000_100,
            "Bob",
            "bob@example.com",
        )?;
        let f3: &[(&str, u32, &str)] =
            &[("a.txt", 0o100_644, "2\n"), ("src/x.rs", 0o100_644, "y\n")];
        let tree3 = repo.tree_from(f3)?;
        let c3 = repo.commit(tree3, &[c2], "touch src FIX", 1_700_000_200)?;
        let side_tree = repo.tree_from(&[
            ("a.txt", 0o100_644, "2\n"),
            ("src/x.rs", 0o100_644, "x\n"),
            ("side.txt", 0o100_644, "s\n"),
        ])?;
        let side = repo.commit(side_tree, &[c2], "side work", 1_700_000_150)?;
        let merged = repo.tree_from(&[
            ("a.txt", 0o100_644, "2\n"),
            ("src/x.rs", 0o100_644, "y\n"),
            ("side.txt", 0o100_644, "s\n"),
        ])?;
        let merge = repo.commit(merged, &[c3, side], "merge side", 1_700_000_300)?;
        repo.set_ref("refs/heads/main", merge)?;
        repo.set_ref("refs/tags/v1", c1)?;
        Ok(repo)
    }

    fn opts(rev: &str) -> LogOpts {
        LogOpts {
            rev: rev.to_owned(),
            max_count: 20,
            skip: 0,
            author: None,
            grep: None,
            since: None,
            until: None,
            spec: Pathspec::all(),
            first_parent: false,
            no_merges: false,
        }
    }

    fn subjects(repo: &TestRepo, opts: &LogOpts) -> TestResult<Vec<String>> {
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        let refs = Refs::new(&opened);
        let result = run(&odb, &refs, opts)?;
        Ok(result
            .commits
            .iter()
            .map(|c| c["subject"].as_str().unwrap_or("").to_owned())
            .collect())
    }

    #[test]
    fn default_order_and_fields() -> TestResult {
        let repo = setup()?;
        assert_eq!(
            subjects(&repo, &opts("HEAD"))?,
            vec![
                "merge side",
                "touch src FIX",
                "side work",
                "touch a",
                "first commit"
            ]
        );
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        let refs = Refs::new(&opened);
        let result = run(&odb, &refs, &opts("v1"))?;
        assert_eq!(result.commits.len(), 1);
        let first = &result.commits[0];
        assert_eq!(first["author"]["name"], "Ada");
        assert_eq!(first["author"]["date"], "2023-11-14T22:13:20+00:00");
        assert_eq!(first["body"], "body of first");
        assert_eq!(first["merge"], false);
        assert_eq!(first["short"].as_str().map(str::len), Some(7));
        assert!(!result.truncated);
        Ok(())
    }

    #[test]
    fn filters_combine() -> TestResult {
        let repo = setup()?;
        let mut o = opts("HEAD");
        o.first_parent = true;
        assert_eq!(
            subjects(&repo, &o)?,
            vec!["merge side", "touch src FIX", "touch a", "first commit"]
        );
        let mut o = opts("HEAD");
        o.no_merges = true;
        assert_eq!(subjects(&repo, &o)?.len(), 4);
        let mut o = opts("HEAD");
        o.author = Some("BOB".into());
        assert_eq!(subjects(&repo, &o)?, vec!["touch a"]);
        let mut o = opts("HEAD");
        o.grep = Some("fix".into());
        assert_eq!(subjects(&repo, &o)?, vec!["touch src FIX"]);
        let mut o = opts("HEAD");
        o.since = Some(1_700_000_150);
        o.until = Some(1_700_000_250);
        assert_eq!(subjects(&repo, &o)?, vec!["touch src FIX", "side work"]);
        let mut o = opts("HEAD");
        o.skip = 1;
        o.max_count = 2;
        assert_eq!(subjects(&repo, &o)?, vec!["touch src FIX", "side work"]);
        let mut o = opts("HEAD");
        o.spec = Pathspec::parse(&["src".to_owned()])?;
        assert_eq!(
            subjects(&repo, &o)?,
            vec!["merge side", "touch src FIX", "first commit"]
        );
        let mut o = opts("HEAD");
        o.spec = Pathspec::parse(&["side.txt".to_owned()])?;
        assert_eq!(subjects(&repo, &o)?, vec!["merge side", "side work"]);
        Ok(())
    }

    #[test]
    fn the_hard_count_limit_applies() -> TestResult {
        let repo = TestRepo::new()?;
        let blob = repo.blob("x")?;
        let tree = repo.tree(&[(0o100_644, "f", blob)])?;
        let mut parent: Vec<Oid> = Vec::new();
        for i in 0..(HARD_MAX_COUNT + 20) {
            let commit = repo.commit(
                tree,
                &parent,
                &format!("c{i}"),
                1_000 + i64::try_from(i).unwrap_or(0),
            )?;
            parent = vec![commit];
        }
        repo.set_ref("refs/heads/main", parent[0])?;
        repo.head_branch("main")?;
        let mut o = opts("HEAD");
        o.max_count = 1_000_000;
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        let refs = Refs::new(&opened);
        let result = run(&odb, &refs, &o)?;
        // Die Byte-Obergrenze der Ausgabe greift vor der Zahlgrenze: nie mehr als erlaubt, und ehrlich gekürzt.
        assert!(result.commits.len() <= HARD_MAX_COUNT && result.commits.len() > 50);
        assert!(
            matches!(result.reason, Some("output_limit" | "max_count")),
            "{:?}",
            result.reason
        );
        assert!(result.truncated);
        Ok(())
    }

    #[test]
    fn ranges_truncation_and_errors() -> TestResult {
        let repo = setup()?;
        assert_eq!(subjects(&repo, &opts("v1..HEAD"))?.len(), 4);
        assert_eq!(subjects(&repo, &opts("v1.."))?.len(), 4);
        assert!(subjects(&repo, &opts("HEAD..HEAD"))?.is_empty());
        let mut o = opts("HEAD");
        o.max_count = 2;
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        let refs = Refs::new(&opened);
        let result = run(&odb, &refs, &o)?;
        assert!(result.truncated);
        assert_eq!(result.reason, Some("max_count"));
        assert!(run(&odb, &refs, &opts("nope")).is_err());
        assert!(run(&odb, &refs, &opts("v1...HEAD")).is_err());
        let mut huge = opts("HEAD");
        huge.max_count = 1_000_000;
        assert_eq!(run(&odb, &refs, &huge)?.commits.len(), 5);
        Ok(())
    }
}
