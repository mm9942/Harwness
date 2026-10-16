//! Zeilenbasierter Diff (Myers, O(ND)) und Unified-Format.
//!
//! # Verantwortung
//! - [`myers`]: kürzestes Editskript zweier Folgen, mit harter Obergrenze `max_d`
//!   für die Editdistanz (Speicher O(D²)); darüber liefert es `None`.
//! - [`diff_lines`]: gemeinsamer Präfix/Suffix wird vorab abgeschnitten, dann
//!   [`myers`] auf dem Rest.
//! - [`unified`]: formatiert ein Editskript als Unified Diff mit `context`
//!   Kontextzeilen und `\ No newline at end of file`.
//!
//! Wird von `fsread.diff` und `git.diff` gemeinsam verwendet.
//!
//! # Nebenläufigkeit
//! Reine Funktionen ohne Zustand.

/// Eine Editoperation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Zeile in beiden Seiten gleich.
    Equal,
    /// Zeile nur links (entfernt).
    Delete,
    /// Zeile nur rechts (hinzugefügt).
    Insert,
}

/// Standard-Obergrenze der Editdistanz.
pub const DEFAULT_MAX_D: usize = 1500;

/// Kürzestes Editskript von `a` nach `b`; `None`, wenn mehr als `max_d` Edits nötig wären.
#[must_use]
pub fn myers<T: PartialEq>(a: &[T], b: &[T], max_d: usize) -> Option<Vec<Op>> {
    let n = isize::try_from(a.len()).ok()?;
    let m = isize::try_from(b.len()).ok()?;
    if n == 0 && m == 0 {
        return Some(Vec::new());
    }
    let max = usize::try_from(n + m).ok()?;
    let limit = max.min(max_d);
    let limit_i = isize::try_from(limit).ok()?;
    let off = limit_i + 1;
    let mut v = vec![0isize; 2 * limit + 3];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let idx = |k: isize| usize::try_from(k + off).ok();
    let mut found: Option<isize> = None;
    'outer: for d in 0..=limit_i {
        let mut snapshot = Vec::with_capacity(usize::try_from(2 * d + 3).ok()?);
        for k in (-d - 1)..=(d + 1) {
            snapshot.push(v[idx(k)?]);
        }
        trace.push(snapshot);
        let mut k = -d;
        while k <= d {
            let down = k == -d || (k != d && v[idx(k - 1)?] < v[idx(k + 1)?]);
            let mut x = if down {
                v[idx(k + 1)?]
            } else {
                v[idx(k - 1)?] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[usize::try_from(x).ok()?] == b[usize::try_from(y).ok()?] {
                x += 1;
                y += 1;
            }
            v[idx(k)?] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'outer;
            }
            k += 2;
        }
    }
    let total_d = found?;
    let mut ops: Vec<Op> = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..=total_d).rev() {
        let snap = &trace[usize::try_from(d).ok()?];
        let get =
            |k: isize| -> Option<isize> { snap.get(usize::try_from(k + d + 1).ok()?).copied() };
        let k = x - y;
        let prev_k = if k == -d || (k != d && get(k - 1)? < get(k + 1)?) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = get(prev_k)?;
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops.push(Op::Equal);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == prev_x {
                ops.push(Op::Insert);
                y -= 1;
            } else {
                ops.push(Op::Delete);
                x -= 1;
            }
        }
    }
    ops.reverse();
    Some(ops)
}

/// Wie [`myers`], schneidet aber gemeinsamen Präfix und Suffix vorab ab.
#[must_use]
pub fn diff_lines<T: PartialEq>(a: &[T], b: &[T], max_d: usize) -> Option<Vec<Op>> {
    let prefix = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    let a_rest = &a[prefix..];
    let b_rest = &b[prefix..];
    let suffix = a_rest
        .iter()
        .rev()
        .zip(b_rest.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let a_mid = &a_rest[..a_rest.len() - suffix];
    let b_mid = &b_rest[..b_rest.len() - suffix];
    let middle = myers(a_mid, b_mid, max_d)?;
    let mut ops = Vec::with_capacity(prefix + middle.len() + suffix);
    ops.extend(std::iter::repeat_n(Op::Equal, prefix));
    ops.extend(middle);
    ops.extend(std::iter::repeat_n(Op::Equal, suffix));
    Some(ops)
}

/// Zählwerte eines Diffs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Hinzugefügte Zeilen.
    pub additions: usize,
    /// Entfernte Zeilen.
    pub deletions: usize,
    /// Anzahl Hunks.
    pub hunks: usize,
}

/// Zählt Hinzufügungen und Löschungen eines Editskripts.
#[must_use]
pub fn count(ops: &[Op]) -> (usize, usize) {
    let additions = ops.iter().filter(|op| **op == Op::Insert).count();
    let deletions = ops.iter().filter(|op| **op == Op::Delete).count();
    (additions, deletions)
}

/// Zerlegt Text in Zeilen mit Zeilenende (`split_inclusive`).
#[must_use]
pub fn split_lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

fn push_line(out: &mut String, prefix: char, line: &str) {
    out.push(prefix);
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push('\n');
        out.push_str("\\ No newline at end of file\n");
    }
}

/// Formatiert ein Editskript als Unified Diff. `a_lines`/`b_lines` sind die
/// Originalzeilen (mit Zeilenende). Liefert Text und Zählwerte; bei
/// identischen Seiten ist der Text leer.
#[must_use]
pub fn unified(
    a_name: &str,
    b_name: &str,
    a_lines: &[&str],
    b_lines: &[&str],
    ops: &[Op],
    context: usize,
) -> (String, Stats) {
    // Positionen (ai, bi) vor jeder Operation.
    let mut positions: Vec<(usize, usize)> = Vec::with_capacity(ops.len());
    let (mut ai, mut bi) = (0usize, 0usize);
    for op in ops {
        positions.push((ai, bi));
        match op {
            Op::Equal => {
                ai += 1;
                bi += 1;
            }
            Op::Delete => ai += 1,
            Op::Insert => bi += 1,
        }
    }
    let changes: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| **op != Op::Equal)
        .map(|(index, _)| index)
        .collect();
    let mut stats = Stats::default();
    let (additions, deletions) = count(ops);
    stats.additions = additions;
    stats.deletions = deletions;
    if changes.is_empty() {
        return (String::new(), stats);
    }
    // Gruppen von Änderungen, deren Kontext sich berührt.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut start = changes[0];
    let mut end = changes[0];
    for &index in &changes[1..] {
        if index - end > 2 * context + 1 {
            groups.push((start, end));
            start = index;
        }
        end = index;
    }
    groups.push((start, end));

    let mut out = String::new();
    out.push_str(&format!("--- {a_name}\n+++ {b_name}\n"));
    for (first, last) in groups {
        let from = first.saturating_sub(context);
        let to = (last + context + 1).min(ops.len());
        let (a_start, b_start) = positions[from];
        let a_len = ops[from..to].iter().filter(|op| **op != Op::Insert).count();
        let b_len = ops[from..to].iter().filter(|op| **op != Op::Delete).count();
        let a_show = if a_len == 0 { a_start } else { a_start + 1 };
        let b_show = if b_len == 0 { b_start } else { b_start + 1 };
        // Wie GNU diff: bei Länge 1 entfällt „,1“.
        let range = |start: usize, len: usize| {
            if len == 1 {
                start.to_string()
            } else {
                format!("{start},{len}")
            }
        };
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            range(a_show, a_len),
            range(b_show, b_len)
        ));
        for index in from..to {
            let (ai, bi) = positions[index];
            match ops[index] {
                Op::Equal => {
                    if let Some(line) = a_lines.get(ai) {
                        push_line(&mut out, ' ', line);
                    }
                }
                Op::Delete => {
                    if let Some(line) = a_lines.get(ai) {
                        push_line(&mut out, '-', line);
                    }
                }
                Op::Insert => {
                    if let Some(line) = b_lines.get(bi) {
                        push_line(&mut out, '+', line);
                    }
                }
            }
        }
        stats.hunks += 1;
    }
    (out, stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn apply(a: &[&str], b: &[&str], ops: &[Op]) -> Vec<String> {
        let (mut ai, mut bi) = (0, 0);
        let mut out = Vec::new();
        for op in ops {
            match op {
                Op::Equal => {
                    out.push(a[ai].to_owned());
                    ai += 1;
                    bi += 1;
                }
                Op::Delete => ai += 1,
                Op::Insert => {
                    out.push(b[bi].to_owned());
                    bi += 1;
                }
            }
        }
        out
    }

    #[test]
    fn script_reconstructs_b_and_is_minimal() -> TestResult {
        let a = ["a", "b", "c", "a", "b", "b", "a"];
        let b = ["c", "b", "a", "b", "a", "c"];
        let ops = myers(&a, &b, 100).ok_or(crate::test_support::TestError::Missing("ops"))?;
        let rebuilt = apply(&a, &b, &ops);
        assert_eq!(
            rebuilt,
            b.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()
        );
        let (adds, dels) = count(&ops);
        // Klassisches Beispiel von Myers: D = 5.
        assert_eq!(adds + dels, 5);
        Ok(())
    }

    #[test]
    fn empty_and_identical_inputs() -> TestResult {
        let none: [&str; 0] = [];
        assert_eq!(myers(&none, &none, 5), Some(vec![]));
        assert_eq!(myers(&["x"], &none, 5), Some(vec![Op::Delete]));
        assert_eq!(myers(&none, &["x"], 5), Some(vec![Op::Insert]));
        assert_eq!(
            diff_lines(&["a", "b"], &["a", "b"], 0),
            Some(vec![Op::Equal, Op::Equal])
        );
        Ok(())
    }

    #[test]
    fn max_d_is_enforced() -> TestResult {
        let a: Vec<String> = (0..50).map(|i| format!("a{i}")).collect();
        let b: Vec<String> = (0..50).map(|i| format!("b{i}")).collect();
        assert!(myers(&a, &b, 40).is_none());
        assert!(myers(&a, &b, 100).is_some());
        Ok(())
    }

    #[test]
    fn unified_output_matches_gnu_diff_shape() -> TestResult {
        let a = [
            "one\n", "two\n", "three\n", "four\n", "five\n", "six\n", "seven\n", "eight\n",
        ];
        let b = [
            "one\n", "TWO\n", "three\n", "four\n", "five\n", "six\n", "seven\n", "eight\n",
            "nine\n",
        ];
        let ops = diff_lines(&a, &b, 100).ok_or(crate::test_support::TestError::Missing("ops"))?;
        let (text, stats) = unified("a.txt", "b.txt", &a, &b, &ops, 1);
        let expected = "--- a.txt\n+++ b.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n@@ -8 +8,2 @@\n eight\n+nine\n";
        assert_eq!(text, expected);
        assert_eq!(
            stats,
            Stats {
                additions: 2,
                deletions: 1,
                hunks: 2
            }
        );
        Ok(())
    }

    #[test]
    fn identical_files_produce_no_output() -> TestResult {
        let a = ["x\n"];
        let ops = diff_lines(&a, &a, 10).ok_or(crate::test_support::TestError::Missing("ops"))?;
        let (text, stats) = unified("a", "b", &a, &a, &ops, 3);
        assert!(text.is_empty());
        assert_eq!(stats.hunks, 0);
        Ok(())
    }

    #[test]
    fn missing_newline_is_marked() -> TestResult {
        let a = ["a\n", "b"];
        let b = ["a\n", "b\n"];
        let ops = diff_lines(&a, &b, 10).ok_or(crate::test_support::TestError::Missing("ops"))?;
        let (text, _) = unified("a", "b", &a, &b, &ops, 1);
        assert!(
            text.contains("-b\n\\ No newline at end of file\n+b\n"),
            "{text}"
        );
        Ok(())
    }

    #[test]
    fn context_zero_and_distant_changes_make_separate_hunks() -> TestResult {
        let a: Vec<String> = (0..20).map(|i| format!("l{i}\n")).collect();
        let mut b = a.clone();
        b[2] = "X\n".to_owned();
        b[17] = "Y\n".to_owned();
        let ar: Vec<&str> = a.iter().map(String::as_str).collect();
        let br: Vec<&str> = b.iter().map(String::as_str).collect();
        let ops =
            diff_lines(&ar, &br, 100).ok_or(crate::test_support::TestError::Missing("ops"))?;
        let (_, stats) = unified("a", "b", &ar, &br, &ops, 3);
        assert_eq!(stats.hunks, 2);
        let (text, _) = unified("a", "b", &ar, &br, &ops, 0);
        assert!(text.contains("@@ -3 +3 @@\n-l2\n+X\n"), "{text}");
        Ok(())
    }

    #[test]
    fn randomized_scripts_always_reconstruct() -> TestResult {
        // Einfacher deterministischer Generator (kein Zufalls-Crate nötig).
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..200 {
            let la = usize::try_from(next() % 12).unwrap_or(0);
            let lb = usize::try_from(next() % 12).unwrap_or(0);
            let a: Vec<String> = (0..la).map(|_| format!("{}", next() % 4)).collect();
            let b: Vec<String> = (0..lb).map(|_| format!("{}", next() % 4)).collect();
            let ar: Vec<&str> = a.iter().map(String::as_str).collect();
            let br: Vec<&str> = b.iter().map(String::as_str).collect();
            let ops =
                diff_lines(&ar, &br, 100).ok_or(crate::test_support::TestError::Missing("ops"))?;
            assert_eq!(apply(&ar, &br, &ops), b, "a={a:?} b={b:?}");
        }
        Ok(())
    }
}
