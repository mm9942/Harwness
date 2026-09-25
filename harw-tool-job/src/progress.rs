//! Fortschritts- und Fehlererkennung in Ausgabezeilen — reine Funktionen.
//!
//! # Verantwortungsbereich
//! - [`detect_progress`]: erkennt eine Fortschrittsangabe in **einer** Zeile
//!   (ninja `[n/m]`, cmake `[ NN%]`, cargo `Compiling`/`Checking`/`Finished`,
//!   vcpkg `Installing n/m`/Phasen, npm/pip-Phasen, generisch `NN%`).
//! - [`detect_severity`]: erkennt Fehler- (`error:`, `error[E…]`,
//!   `fatal error`, `FAILED`, `panicked`, `npm ERR!`) und Warnzeilen
//!   (`warning:`, `npm WARN`).
//! - [`ProgressTracker`]: fasst Zeilen zu einem [`ProgressSnapshot`]
//!   zusammen (zählt z. B. cargo-Schritte ohne bekannte Gesamtzahl).
//!
//! Alles ist „best effort": eine nicht erkannte Zeile ist nie ein Fehler.
//!
//! # Nebenläufigkeit
//! Keine geteilten Zustände; [`ProgressTracker`] gehört genau einem
//! Überwachungs-Task.

use serde::{Deserialize, Serialize};

/// Höchstlänge einer gespeicherten Phasenbeschreibung (Zeichen).
const MAX_PHASE_CHARS: usize = 96;

/// Werkzeug, aus dessen Ausgabe ein Fortschritt stammt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressSource {
    /// ninja `[n/m]`.
    Ninja,
    /// cargo `Compiling …`/`Finished …`.
    Cargo,
    /// cmake/make `[ NN%]`.
    Cmake,
    /// vcpkg `Installing n/m …` und Phasen.
    Vcpkg,
    /// npm-Phasen.
    Npm,
    /// pip-Phasen.
    Pip,
    /// Irgendein `NN%` in der Zeile.
    Generic,
}

/// Eine in einer einzelnen Zeile erkannte Fortschrittsangabe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressUpdate {
    /// `done` von `total` Schritten.
    Fraction {
        /// Quelle.
        source: ProgressSource,
        /// Erledigte Schritte.
        done: u64,
        /// Gesamtzahl.
        total: u64,
        /// Beschreibung des aktuellen Schritts.
        phase: Option<String>,
    },
    /// Prozentangabe 0–100.
    Percent {
        /// Quelle.
        source: ProgressSource,
        /// Prozent (abgerundet).
        percent: u8,
        /// Beschreibung des aktuellen Schritts.
        phase: Option<String>,
    },
    /// Ein gezählter Schritt ohne bekannte Gesamtzahl (cargo `Compiling x`).
    Step {
        /// Quelle.
        source: ProgressSource,
        /// Beschreibung.
        phase: String,
    },
    /// Phasenwechsel ohne Zählung (cargo `Finished`, pip `Successfully …`).
    Phase {
        /// Quelle.
        source: ProgressSource,
        /// Beschreibung.
        phase: String,
    },
}

/// Schweregrad einer Zeile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Fehlerzeile.
    Error,
    /// Warnzeile.
    Warning,
}

/// Zusammengefasster Fortschritt eines Jobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressSnapshot {
    /// Quelle der letzten Angabe.
    pub source: ProgressSource,
    /// Prozent, falls bekannt oder aus `done/total` berechenbar.
    #[serde(default)]
    pub percent: Option<u8>,
    /// Erledigte Schritte (bei [`ProgressUpdate::Step`] die bisherige Zahl).
    #[serde(default)]
    pub done: Option<u64>,
    /// Gesamtzahl, falls bekannt.
    #[serde(default)]
    pub total: Option<u64>,
    /// Aktueller Schritt bzw. aktuelle Phase.
    #[serde(default)]
    pub phase: Option<String>,
}

impl ProgressSnapshot {
    /// Kurzform für Meldungen, z. B. `ninja 12/340 (3%) — Building CXX …`.
    #[must_use]
    pub fn render(&self) -> String {
        let source = match self.source {
            ProgressSource::Ninja => "ninja",
            ProgressSource::Cargo => "cargo",
            ProgressSource::Cmake => "cmake",
            ProgressSource::Vcpkg => "vcpkg",
            ProgressSource::Npm => "npm",
            ProgressSource::Pip => "pip",
            ProgressSource::Generic => "progress",
        };
        let mut out = source.to_owned();
        match (self.done, self.total) {
            (Some(done), Some(total)) => out.push_str(&format!(" {done}/{total}")),
            (Some(done), None) => out.push_str(&format!(" {done} steps")),
            _ => {}
        }
        if let Some(percent) = self.percent {
            out.push_str(&format!(" ({percent}%)"));
        }
        if let Some(phase) = &self.phase {
            out.push_str(" — ");
            out.push_str(phase);
        }
        out
    }
}

/// Fasst erkannte Angaben zu einem [`ProgressSnapshot`] zusammen.
///
/// # Description
/// Spezifische Quellen (ninja, cargo, cmake, vcpkg, npm, pip) überschreiben
/// immer; eine generische `NN%`-Angabe nur, solange noch keine spezifische
/// Quelle gesehen wurde — sonst würde z. B. eine Testausgabe „50%" den
/// ninja-Zähler überschreiben.
///
/// # Examples
/// ```rust
/// use harw_tool_job::ProgressTracker;
///
/// let mut tracker = ProgressTracker::default();
/// assert!(tracker.observe("[3/12] Building CXX object a.o"));
/// let percent = tracker.snapshot().and_then(|snapshot| snapshot.percent);
/// assert_eq!(percent, Some(25));
/// ```
#[derive(Debug, Default, Clone)]
pub struct ProgressTracker {
    snapshot: Option<ProgressSnapshot>,
    steps: u64,
}

impl ProgressTracker {
    /// Wertet eine Zeile aus.
    ///
    /// # Returns
    /// `true`, wenn sich der zusammengefasste Fortschritt geändert hat.
    pub fn observe(&mut self, line: &str) -> bool {
        let Some(update) = detect_progress(line) else {
            return false;
        };
        let specific_seen = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.source != ProgressSource::Generic);
        let next = match update {
            ProgressUpdate::Fraction {
                source,
                done,
                total,
                phase,
            } => ProgressSnapshot {
                source,
                percent: percent_of(done, total),
                done: Some(done),
                total: Some(total),
                phase,
            },
            ProgressUpdate::Percent {
                source,
                percent,
                phase,
            } => {
                if source == ProgressSource::Generic && specific_seen {
                    return false;
                }
                ProgressSnapshot {
                    source,
                    percent: Some(percent),
                    done: None,
                    total: None,
                    phase,
                }
            }
            ProgressUpdate::Step { source, phase } => {
                self.steps = self.steps.saturating_add(1);
                ProgressSnapshot {
                    source,
                    percent: None,
                    done: Some(self.steps),
                    total: None,
                    phase: Some(phase),
                }
            }
            ProgressUpdate::Phase { source, phase } => {
                let previous = self.snapshot.as_ref().filter(|s| s.source == source);
                ProgressSnapshot {
                    source,
                    percent: previous.and_then(|s| s.percent),
                    done: previous.and_then(|s| s.done),
                    total: previous.and_then(|s| s.total),
                    phase: Some(phase),
                }
            }
        };
        if self.snapshot.as_ref() == Some(&next) {
            return false;
        }
        self.snapshot = Some(next);
        true
    }

    /// Der aktuelle Stand, falls schon etwas erkannt wurde.
    #[must_use]
    pub fn snapshot(&self) -> Option<&ProgressSnapshot> {
        self.snapshot.as_ref()
    }
}

/// `done/total` in ganzen Prozent (abgerundet, höchstens 100).
fn percent_of(done: u64, total: u64) -> Option<u8> {
    if total == 0 {
        return None;
    }
    let pct = done.min(total).saturating_mul(100) / total;
    u8::try_from(pct).ok()
}

/// Kürzt eine Phasenbeschreibung auf [`MAX_PHASE_CHARS`] Zeichen.
fn phase_text(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_PHASE_CHARS {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX_PHASE_CHARS).collect();
    out.push('…');
    out
}

/// Liest führende ASCII-Ziffern als Zahl; gibt Zahl und Rest zurück.
fn leading_number(text: &str) -> Option<(u64, &str)> {
    let end = text
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(text.len());
    if end == 0 {
        return None;
    }
    let number = text[..end].parse().ok()?;
    Some((number, &text[end..]))
}

/// Erkennt eine Fortschrittsangabe in einer Zeile.
///
/// # Returns
/// `None`, wenn die Zeile keine erkennbare Angabe trägt.
///
/// # Examples
/// ```rust
/// use harw_tool_job::{ProgressSource, ProgressUpdate, detect_progress};
///
/// assert_eq!(
///     detect_progress("[ 45%] Building CXX object foo.o"),
///     Some(ProgressUpdate::Percent {
///         source: ProgressSource::Cmake,
///         percent: 45,
///         phase: Some("Building CXX object foo.o".into()),
///     })
/// );
/// assert_eq!(detect_progress("hello"), None);
/// ```
#[must_use]
pub fn detect_progress(line: &str) -> Option<ProgressUpdate> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    ninja(trimmed)
        .or_else(|| cmake(trimmed))
        .or_else(|| cargo(trimmed))
        .or_else(|| vcpkg(trimmed))
        .or_else(|| npm(trimmed))
        .or_else(|| pip(trimmed))
        .or_else(|| generic_percent(trimmed))
}

/// ninja: `[12/345] Building CXX object …`.
fn ninja(line: &str) -> Option<ProgressUpdate> {
    let rest = line.strip_prefix('[')?;
    let (done, rest) = leading_number(rest)?;
    let rest = rest.strip_prefix('/')?;
    let (total, rest) = leading_number(rest)?;
    let rest = rest.strip_prefix(']')?;
    if total == 0 || done > total {
        return None;
    }
    let phase = Some(phase_text(rest)).filter(|phase| !phase.is_empty());
    Some(ProgressUpdate::Fraction {
        source: ProgressSource::Ninja,
        done,
        total,
        phase,
    })
}

/// cmake/make: `[ 45%] Building …`, `[100%] Built target x`.
fn cmake(line: &str) -> Option<ProgressUpdate> {
    let rest = line.strip_prefix('[')?.trim_start();
    let (percent, rest) = leading_number(rest)?;
    let rest = rest.strip_prefix("%]")?;
    let percent = u8::try_from(percent).ok().filter(|pct| *pct <= 100)?;
    let phase = Some(phase_text(rest)).filter(|phase| !phase.is_empty());
    Some(ProgressUpdate::Percent {
        source: ProgressSource::Cmake,
        percent,
        phase,
    })
}

/// cargo: `Compiling foo v1.2.3 (…)`, `Checking …`, `Documenting …`,
/// `Finished …`, `Running …`.
fn cargo(line: &str) -> Option<ProgressUpdate> {
    for verb in ["Compiling", "Checking", "Documenting"] {
        if let Some(rest) = line.strip_prefix(verb).and_then(|r| r.strip_prefix(' ')) {
            let mut words = rest.split_whitespace();
            let krate = words.next()?;
            let version = words.next()?;
            let looks_like_version = version
                .strip_prefix('v')
                .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()));
            if !looks_like_version {
                return None;
            }
            return Some(ProgressUpdate::Step {
                source: ProgressSource::Cargo,
                phase: phase_text(&format!("{} {krate} {version}", verb.to_lowercase())),
            });
        }
    }
    if let Some(rest) = line.strip_prefix("Finished ") {
        let looks_like_cargo = rest.contains("profile") || rest.contains("target(s)");
        if looks_like_cargo {
            return Some(ProgressUpdate::Phase {
                source: ProgressSource::Cargo,
                phase: phase_text(&format!("finished {rest}")),
            });
        }
    }
    if let Some(rest) = line.strip_prefix("Running ") {
        if rest.starts_with("unittests") || rest.starts_with('`') || rest.contains("target/") {
            return Some(ProgressUpdate::Phase {
                source: ProgressSource::Cargo,
                phase: phase_text(&format!("running {rest}")),
            });
        }
    }
    None
}

/// vcpkg: `Installing 3/42 zlib:x64-linux@1.3.1...`, `Building zlib:x64-linux...`,
/// `Computing installation plan...`, `Restored 12 package(s) …`.
fn vcpkg(line: &str) -> Option<ProgressUpdate> {
    if let Some(rest) = line.strip_prefix("Installing ") {
        if let Some((done, after)) = leading_number(rest) {
            if let Some(after) = after.strip_prefix('/') {
                if let Some((total, after)) = leading_number(after) {
                    if total > 0 && done <= total {
                        return Some(ProgressUpdate::Fraction {
                            source: ProgressSource::Vcpkg,
                            done,
                            total,
                            phase: Some(phase_text(&format!("installing{after}"))),
                        });
                    }
                }
            }
        }
    }
    if let Some(rest) = line.strip_prefix("Building ") {
        let package = rest.split_whitespace().next().unwrap_or_default();
        if package.contains(':') && !package.contains("::") {
            return Some(ProgressUpdate::Phase {
                source: ProgressSource::Vcpkg,
                phase: phase_text(&format!("building {}", package.trim_end_matches('.'))),
            });
        }
    }
    for marker in [
        "Computing installation plan",
        "Detecting compiler hash",
        "Restored ",
        "Elapsed time to handle",
        "All requested installations completed successfully",
    ] {
        if line.starts_with(marker) {
            return Some(ProgressUpdate::Phase {
                source: ProgressSource::Vcpkg,
                phase: phase_text(line),
            });
        }
    }
    None
}

/// npm: `added 123 packages …`, `up to date, audited …`, `removed …`.
fn npm(line: &str) -> Option<ProgressUpdate> {
    let is_summary = ((line.starts_with("added ") || line.starts_with("removed "))
        && line.contains(" package"))
        || line.starts_with("up to date, audited ");
    is_summary.then(|| ProgressUpdate::Phase {
        source: ProgressSource::Npm,
        phase: phase_text(line),
    })
}

/// pip: `Collecting x`, `Downloading x`, `Building wheel for x`,
/// `Installing collected packages: …`, `Successfully installed …`.
fn pip(line: &str) -> Option<ProgressUpdate> {
    if let Some(rest) = line.strip_prefix("Collecting ") {
        return Some(ProgressUpdate::Step {
            source: ProgressSource::Pip,
            phase: phase_text(&format!("collecting {rest}")),
        });
    }
    for marker in [
        "Building wheel for ",
        "Installing collected packages",
        "Successfully installed ",
        "Successfully built ",
    ] {
        if line.starts_with(marker) {
            return Some(ProgressUpdate::Phase {
                source: ProgressSource::Pip,
                phase: phase_text(line),
            });
        }
    }
    None
}

/// Generisch: die **letzte** Angabe `NN%` bzw. `NN.N%` (0–100) in der Zeile,
/// vor der kein Buchstabe/keine Ziffer steht.
fn generic_percent(line: &str) -> Option<ProgressUpdate> {
    let bytes = line.as_bytes();
    let mut found = None;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'%' {
            continue;
        }
        // Rückwärts über `NN` bzw. `NN.N`.
        let mut start = index;
        while start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
            start -= 1;
        }
        if start == index {
            continue;
        }
        if start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let number = &line[start..index];
        let whole = number.split('.').next().unwrap_or_default();
        if whole.is_empty() || number.matches('.').count() > 1 || number.ends_with('.') {
            continue;
        }
        if let Some(percent) = whole.parse::<u8>().ok().filter(|pct| *pct <= 100) {
            found = Some(percent);
        }
    }
    found.map(|percent| ProgressUpdate::Percent {
        source: ProgressSource::Generic,
        percent,
        phase: None,
    })
}

/// Erkennt Fehler- und Warnzeilen.
///
/// # Description
/// Fehler: `error:` (jede Schreibweise, z. B. gcc `a.c:1:2: error:`,
/// vcpkg `error:`, Python `ERROR:`), rustc `error[E…]`, `fatal error`,
/// ninja/Tests `FAILED`, Rust `panicked`, `npm ERR!`. Warnungen:
/// `warning:` (jede Schreibweise), `npm WARN`. Fehler gewinnen.
///
/// # Examples
/// ```rust
/// use harw_tool_job::{Severity, detect_severity};
///
/// assert_eq!(detect_severity("src/a.c:3:1: error: expected ';'"), Some(Severity::Error));
/// assert_eq!(detect_severity("warning: unused variable `x`"), Some(Severity::Warning));
/// assert_eq!(detect_severity("-Werror=format is set"), None);
/// ```
#[must_use]
pub fn detect_severity(line: &str) -> Option<Severity> {
    let lower = line.to_ascii_lowercase();
    let is_error = lower.contains("error:")
        || lower.contains("error[e")
        || lower.contains("fatal error")
        || line.contains("FAILED")
        || lower.contains("panicked")
        || line.contains("npm ERR!");
    if is_error {
        return Some(Severity::Error);
    }
    let is_warning = lower.contains("warning:") || line.contains("npm WARN");
    is_warning.then_some(Severity::Warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fraction(
        source: ProgressSource,
        done: u64,
        total: u64,
        phase: Option<&str>,
    ) -> ProgressUpdate {
        ProgressUpdate::Fraction {
            source,
            done,
            total,
            phase: phase.map(str::to_owned),
        }
    }

    fn percent(source: ProgressSource, pct: u8, phase: Option<&str>) -> ProgressUpdate {
        ProgressUpdate::Percent {
            source,
            percent: pct,
            phase: phase.map(str::to_owned),
        }
    }

    #[test]
    fn test_detect_progress_table() {
        use ProgressSource::*;
        let cases: Vec<(&str, Option<ProgressUpdate>)> = vec![
            // ninja
            (
                "[12/345] Building CXX object src/a.o",
                Some(fraction(
                    Ninja,
                    12,
                    345,
                    Some("Building CXX object src/a.o"),
                )),
            ),
            (
                "[1/1] Linking CXX executable app",
                Some(fraction(Ninja, 1, 1, Some("Linking CXX executable app"))),
            ),
            ("[5/3] impossible", None),
            ("[0/0] nothing", None),
            // cmake / make
            (
                "[ 45%] Building CXX object foo.o",
                Some(percent(Cmake, 45, Some("Building CXX object foo.o"))),
            ),
            (
                "[100%] Built target app",
                Some(percent(Cmake, 100, Some("Built target app"))),
            ),
            (
                "[  3%] Linking C static library libz.a",
                Some(percent(Cmake, 3, Some("Linking C static library libz.a"))),
            ),
            // cargo
            (
                "   Compiling serde v1.0.228",
                Some(ProgressUpdate::Step {
                    source: Cargo,
                    phase: "compiling serde v1.0.228".into(),
                }),
            ),
            (
                "    Checking harw-tool-job v0.3.0 (/src/harw-tool-job)",
                Some(ProgressUpdate::Step {
                    source: Cargo,
                    phase: "checking harw-tool-job v0.3.0".into(),
                }),
            ),
            (
                "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.21s",
                Some(ProgressUpdate::Phase {
                    source: Cargo,
                    phase: "finished `dev` profile [unoptimized + debuginfo] target(s) in 3.21s"
                        .into(),
                }),
            ),
            ("Compiling the documentation now", None),
            // vcpkg
            (
                "Installing 3/42 zlib:x64-linux@1.3.1...",
                Some(fraction(
                    Vcpkg,
                    3,
                    42,
                    Some("installing zlib:x64-linux@1.3.1..."),
                )),
            ),
            (
                "Building zlib:x64-linux@1.3.1...",
                Some(ProgressUpdate::Phase {
                    source: Vcpkg,
                    phase: "building zlib:x64-linux@1.3.1".into(),
                }),
            ),
            (
                "Computing installation plan...",
                Some(ProgressUpdate::Phase {
                    source: Vcpkg,
                    phase: "Computing installation plan...".into(),
                }),
            ),
            // npm
            (
                "added 123 packages, and audited 124 packages in 3s",
                Some(ProgressUpdate::Phase {
                    source: Npm,
                    phase: "added 123 packages, and audited 124 packages in 3s".into(),
                }),
            ),
            // pip
            (
                "Collecting requests",
                Some(ProgressUpdate::Step {
                    source: Pip,
                    phase: "collecting requests".into(),
                }),
            ),
            (
                "Successfully installed requests-2.32.0",
                Some(ProgressUpdate::Phase {
                    source: Pip,
                    phase: "Successfully installed requests-2.32.0".into(),
                }),
            ),
            // generisch
            ("Downloading... 37% done", Some(percent(Generic, 37, None))),
            ("progress 12.5% then 40%", Some(percent(Generic, 40, None))),
            ("x100% not a percentage", None),
            ("value is 250%", None),
            ("", None),
            ("plain output line", None),
        ];
        for (line, expected) in cases {
            assert_eq!(detect_progress(line), expected, "line: {line:?}");
        }
    }

    #[test]
    fn test_detect_severity_table() {
        let cases: &[(&str, Option<Severity>)] = &[
            ("error: could not compile `foo`", Some(Severity::Error)),
            ("error[E0308]: mismatched types", Some(Severity::Error)),
            ("src/a.c:3:1: error: expected ';'", Some(Severity::Error)),
            (
                "a.cpp:1:10: fatal error: foo.h: No such file or directory",
                Some(Severity::Error),
            ),
            ("FAILED: src/a.o", Some(Severity::Error)),
            ("test tests::it_works ... FAILED", Some(Severity::Error)),
            (
                "thread 'main' panicked at src/main.rs:2:5:",
                Some(Severity::Error),
            ),
            ("npm ERR! code ENOENT", Some(Severity::Error)),
            ("ERROR: Could not find a version", Some(Severity::Error)),
            ("warning: unused variable `x`", Some(Severity::Warning)),
            ("CMake Warning: something", Some(Severity::Warning)),
            ("npm WARN deprecated foo", Some(Severity::Warning)),
            ("-Werror=format is set", None),
            ("test error_handling ... ok", None),
            ("0 errors, 0 failures", None),
            ("all good", None),
        ];
        for (line, expected) in cases {
            assert_eq!(detect_severity(line), *expected, "line: {line:?}");
        }
    }

    #[test]
    fn test_tracker_counts_cargo_steps_and_keeps_counts_on_finish() {
        let mut tracker = ProgressTracker::default();
        assert!(tracker.observe("   Compiling a v0.1.0"));
        assert!(tracker.observe("   Compiling b v0.2.0"));
        let snapshot = tracker.snapshot().cloned();
        assert_eq!(snapshot.as_ref().and_then(|s| s.done), Some(2));
        assert!(tracker.observe("    Finished `release` profile [optimized] target(s) in 1.0s"));
        let snapshot = tracker.snapshot().cloned();
        assert_eq!(snapshot.as_ref().and_then(|s| s.done), Some(2));
        assert!(
            snapshot
                .and_then(|s| s.phase)
                .is_some_and(|phase| phase.starts_with("finished"))
        );
    }

    #[test]
    fn test_tracker_generic_percent_does_not_override_specific_source() {
        let mut tracker = ProgressTracker::default();
        assert!(tracker.observe("[10/20] Building a.o"));
        assert!(!tracker.observe("test output says 99%"));
        assert_eq!(tracker.snapshot().and_then(|s| s.percent), Some(50));
        assert!(!tracker.observe("unrelated line"));
        // gleiche Angabe zweimal: keine Änderung
        assert!(!tracker.observe("[10/20] Building a.o"));
    }

    #[test]
    fn test_tracker_generic_percent_applies_without_specific_source() {
        let mut tracker = ProgressTracker::default();
        assert!(tracker.observe("fetching 10%"));
        assert!(tracker.observe("fetching 20%"));
        assert_eq!(tracker.snapshot().and_then(|s| s.percent), Some(20));
    }

    #[test]
    fn test_snapshot_render() {
        let snapshot = ProgressSnapshot {
            source: ProgressSource::Ninja,
            percent: Some(3),
            done: Some(12),
            total: Some(340),
            phase: Some("Building a.o".into()),
        };
        assert_eq!(snapshot.render(), "ninja 12/340 (3%) — Building a.o");
    }
}
