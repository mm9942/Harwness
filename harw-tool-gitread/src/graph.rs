//! Commit-Graph: Erreichbarkeit, Vorsprung/Rückstand und Log-Reihenfolge.
//!
//! # Verantwortung
//! - [`read_commit`]: Commit lesen und parsen,
//! - [`ancestors`]: alle von einer Menge von Spitzen erreichbaren Commits,
//! - [`ahead_behind`]: `(nur in a, nur in b)` wie `git rev-list --left-right --count`,
//! - [`LogWalk`]: Commits in Git-Standardreihenfolge (neuester
//!   Committer-Zeitstempel zuerst, bei Gleichstand in Einfügereihenfolge).
//!
//! # Grenzen
//! Jede Funktion hat eine Obergrenze für gelesene Commits
//! ([`MAX_WALK_COMMITS`]) und eine Frist; bei Überschreitung gibt es einen
//! Fehler bzw. `None`, nie eine Endlosschleife und kein unbegrenztes
//! Speicherwachstum.

use crate::object::{Commit, Kind, parse_commit};
use crate::odb::Odb;
use crate::oid::Oid;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::time::Instant;

/// Höchstzahl gelesener Commits je Aufruf.
pub const MAX_WALK_COMMITS: usize = 200_000;

/// Liest einen Commit.
///
/// # Errors
/// Meldung, wenn das Objekt fehlt, kein Commit oder beschädigt ist.
pub fn read_commit(odb: &Odb<'_>, oid: &Oid) -> Result<Commit, String> {
    parse_commit(&odb.read_kind(oid, Kind::Commit)?.data)
}

/// Alle Commits, die von `tips` aus erreichbar sind (einschließlich `tips`).
///
/// # Errors
/// Meldung bei fehlenden Objekten, Überschreitung von [`MAX_WALK_COMMITS`]
/// oder der Frist.
pub fn ancestors(odb: &Odb<'_>, tips: &[Oid], deadline: Instant) -> Result<HashSet<Oid>, String> {
    let mut seen: HashSet<Oid> = HashSet::new();
    let mut stack: Vec<Oid> = tips.to_vec();
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        if seen.len() > MAX_WALK_COMMITS {
            return Err(format!(
                "history has more than {MAX_WALK_COMMITS} commits to examine"
            ));
        }
        if Instant::now() > deadline {
            return Err("history walk timed out".to_owned());
        }
        stack.extend(read_commit(odb, &oid)?.parents);
    }
    Ok(seen)
}

/// `(nur in a erreichbar, nur in b erreichbar)`; `None`, wenn die Historie zu groß ist.
#[must_use]
pub fn ahead_behind(odb: &Odb<'_>, a: &Oid, b: &Oid, deadline: Instant) -> Option<(usize, usize)> {
    let from_a = ancestors(odb, &[*a], deadline).ok()?;
    let from_b = ancestors(odb, &[*b], deadline).ok()?;
    Some((
        from_a.difference(&from_b).count(),
        from_b.difference(&from_a).count(),
    ))
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Queued {
    when: i64,
    order: Reverse<u64>,
    oid: Oid,
}

/// Läuft die Historie ab (Standard-`git log`-Reihenfolge).
pub struct LogWalk<'a, 'o> {
    odb: &'a Odb<'o>,
    heap: BinaryHeap<Queued>,
    seen: HashSet<Oid>,
    hidden: HashSet<Oid>,
    counter: u64,
    first_parent: bool,
    deadline: Instant,
}

impl<'a, 'o> LogWalk<'a, 'o> {
    /// Startet bei `tips`; Commits in `hidden` (und alles darunter, sofern
    /// vollständig in `hidden` enthalten) werden nicht geliefert.
    ///
    /// # Errors
    /// Meldung, wenn eine Spitze kein lesbarer Commit ist.
    pub fn new(
        odb: &'a Odb<'o>,
        tips: &[Oid],
        hidden: HashSet<Oid>,
        first_parent: bool,
        deadline: Instant,
    ) -> Result<Self, String> {
        let mut walk = Self {
            odb,
            heap: BinaryHeap::new(),
            seen: HashSet::new(),
            hidden,
            counter: 0,
            first_parent,
            deadline,
        };
        for tip in tips {
            walk.push(*tip)?;
        }
        Ok(walk)
    }

    fn push(&mut self, oid: Oid) -> Result<(), String> {
        if self.hidden.contains(&oid) || !self.seen.insert(oid) {
            return Ok(());
        }
        if self.seen.len() > MAX_WALK_COMMITS {
            return Err(format!(
                "history has more than {MAX_WALK_COMMITS} commits to examine"
            ));
        }
        let commit = read_commit(self.odb, &oid)?;
        self.counter += 1;
        self.heap.push(Queued {
            when: commit.committer.when,
            order: Reverse(self.counter),
            oid,
        });
        Ok(())
    }

    /// Der nächste Commit samt Inhalt.
    ///
    /// # Errors
    /// Meldung bei Frist, Übergröße oder beschädigter Historie.
    pub fn next_commit(&mut self) -> Result<Option<(Oid, Commit)>, String> {
        if Instant::now() > self.deadline {
            return Err("history walk timed out".to_owned());
        }
        let Some(item) = self.heap.pop() else {
            return Ok(None);
        };
        let commit = read_commit(self.odb, &item.oid)?;
        let parents: Vec<Oid> = if self.first_parent {
            commit.parents.iter().take(1).copied().collect()
        } else {
            commit.parents.clone()
        };
        for parent in parents {
            self.push(parent)?;
        }
        Ok(Some((item.oid, commit)))
    }

    /// Warten noch Commits in der Warteschlange?
    #[must_use]
    pub fn has_more(&self) -> bool {
        !self.heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Repo;
    use crate::test_support::{TestError, TestRepo, TestResult};
    use std::time::Duration;

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    struct Graph {
        repo: TestRepo,
        c: Vec<Oid>,
    }

    /// c0 <- c1 <- c2 <- c4(merge) und c1 <- c3 <- c4
    fn graph() -> TestResult<Graph> {
        let repo = TestRepo::new()?;
        let blob = repo.blob("x")?;
        let tree = repo.tree(&[(0o100_644, "f", blob)])?;
        let c0 = repo.commit(tree, &[], "c0", 100)?;
        let c1 = repo.commit(tree, &[c0], "c1", 200)?;
        let c2 = repo.commit(tree, &[c1], "c2", 300)?;
        let c3 = repo.commit(tree, &[c1], "c3", 350)?;
        let c4 = repo.commit(tree, &[c2, c3], "c4", 400)?;
        Ok(Graph {
            repo,
            c: vec![c0, c1, c2, c3, c4],
        })
    }

    #[test]
    fn ancestors_and_ahead_behind() -> TestResult {
        let g = graph()?;
        let opened = Repo::open(&g.repo.ws)?;
        let odb = Odb::new(&opened);
        assert_eq!(ancestors(&odb, &[g.c[4]], soon())?.len(), 5);
        assert_eq!(ancestors(&odb, &[g.c[2]], soon())?.len(), 3);
        assert_eq!(ahead_behind(&odb, &g.c[2], &g.c[3], soon()), Some((1, 1)));
        assert_eq!(ahead_behind(&odb, &g.c[4], &g.c[1], soon()), Some((3, 0)));
        assert_eq!(ahead_behind(&odb, &g.c[1], &g.c[4], soon()), Some((0, 3)));
        assert_eq!(ahead_behind(&odb, &g.c[2], &g.c[2], soon()), Some((0, 0)));
        Ok(())
    }

    #[test]
    fn log_order_is_newest_first_and_ties_keep_insertion_order() -> TestResult {
        let g = graph()?;
        let opened = Repo::open(&g.repo.ws)?;
        let odb = Odb::new(&opened);
        let mut walk = LogWalk::new(&odb, &[g.c[4]], HashSet::new(), false, soon())?;
        let mut order = Vec::new();
        while let Some((oid, _)) = walk.next_commit()? {
            order.push(oid);
        }
        assert_eq!(order, vec![g.c[4], g.c[3], g.c[2], g.c[1], g.c[0]]);
        // first-parent
        let mut walk = LogWalk::new(&odb, &[g.c[4]], HashSet::new(), true, soon())?;
        let mut order = Vec::new();
        while let Some((oid, _)) = walk.next_commit()? {
            order.push(oid);
        }
        assert_eq!(order, vec![g.c[4], g.c[2], g.c[1], g.c[0]]);
        // hidden (A..B)
        let hidden = ancestors(&odb, &[g.c[2]], soon())?;
        let mut walk = LogWalk::new(&odb, &[g.c[4]], hidden, false, soon())?;
        let mut order = Vec::new();
        while let Some((oid, _)) = walk.next_commit()? {
            order.push(oid);
        }
        assert_eq!(order, vec![g.c[4], g.c[3]]);
        Ok(())
    }

    #[test]
    fn missing_parents_and_expired_deadlines_are_errors() -> TestResult {
        let repo = TestRepo::new()?;
        let blob = repo.blob("x")?;
        let tree = repo.tree(&[(0o100_644, "f", blob)])?;
        let ghost = crate::oid::hash_object("commit", b"ghost");
        let child = repo.commit(tree, &[ghost], "child", 10)?;
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        assert!(ancestors(&odb, &[child], soon()).is_err());
        assert!(ahead_behind(&odb, &child, &child, soon()).is_none());
        assert!(
            LogWalk::new(&odb, &[child], HashSet::new(), false, soon())?
                .next_commit()
                .is_err()
        );
        let past = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .ok_or(TestError::Missing("past"))?;
        assert!(ancestors(&odb, &[child], past).is_err());
        let blob_oid = blob;
        assert!(ancestors(&odb, &[blob_oid], soon()).is_err());
        Ok(())
    }
}
