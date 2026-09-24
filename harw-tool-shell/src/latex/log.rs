//! Auswertung eines TeX-Logs für `latex.build` (Runde 7, Teil T3).
//!
//! # Zweck
//! Der Build-Bericht soll mehr sagen als „ok“: Seitenzahl, Overfull-Boxen mit
//! Zeile und Überstand, die Zahl der Underfull-Boxen, fehlende Glyphen und
//! Warnungen zu fehlenden Trennmustern (die „englische Silbentrennung“ in
//! einem deutschen Text). [`parse_build_log`] liest das alles rein und ohne
//! Prozessstart aus dem Log-Text.
//!
//! # Grenzen
//! TeX bricht Log-Zeilen nach 79 Zeichen um. Die Seitenzahl sucht deshalb
//! über Zeilenumbrüche hinweg; Overfull-/Underfull-Kopfzeilen sind kurz
//! genug und werden zeilenweise erkannt.

use serde::Serialize;
use std::collections::BTreeSet;

/// Overfull-Boxen bis zu diesem Überstand (in pt) gelten als harmlos und
/// erscheinen nicht in [`LogReport::overfull`].
pub const OVERFULL_THRESHOLD_PT: f64 = 1.0;
/// Höchstens so viele Overfull-Einträge im Bericht.
const MAX_OVERFULL: usize = 50;
/// Höchstens so viele fehlende Zeichen im Bericht.
const MAX_MISSING_CHARS: usize = 20;
/// Höchstens so viele Sprachwarnungen im Bericht.
const MAX_LANGUAGE_WARNINGS: usize = 10;
/// Kontext einer Overfull-Box wird auf so viele Zeichen gekürzt.
const MAX_CONTEXT_CHARS: usize = 120;
/// So weit hinter „Output written on“ wird nach „(N pages“ gesucht.
const PAGES_SEARCH_WINDOW: usize = 600;

/// Eine Overfull-Box über der Schwelle.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OverfullBox {
    /// Quellzeile (erste Zeile von „at lines a--b“ bzw. „at line n“), falls
    /// das Log sie nennt.
    pub line: Option<u32>,
    /// Überstand in pt (auf zwei Nachkommastellen gerundet).
    pub pt: f64,
    /// Der Textausschnitt aus der Folgezeile des Logs (gekürzt), damit das
    /// Modell die Stelle auch ohne Zeilennummer findet.
    pub context: String,
}

/// Das Ergebnis von [`parse_build_log`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct LogReport {
    /// Seitenzahl aus „Output written on … (N pages …)“; `Some(0)` bei „No
    /// pages of output.“, `None`, wenn das Log nichts dazu sagt.
    pub pages: Option<u32>,
    /// Overfull-Boxen mit mehr als [`OVERFULL_THRESHOLD_PT`] Überstand, in
    /// Log-Reihenfolge (höchstens 50).
    pub overfull: Vec<OverfullBox>,
    /// Alle Overfull-Boxen, auch die kleinen.
    pub overfull_total: usize,
    /// Zahl der Underfull-Boxen (`\hbox` und `\vbox`).
    pub underfull_count: usize,
    /// Fehlende Glyphen, z. B. „ß (U+00DF) in font cmr10“, ohne Duplikate.
    pub missing_chars: Vec<String>,
    /// Warnungen zu fehlenden Trennmustern bzw. unbekannten Sprachen
    /// (babel/polyglossia), Fortsetzungszeilen zusammengefügt.
    pub language_warnings: Vec<String>,
}

impl LogReport {
    /// `true`, wenn der Build zwar durchlief, das Ergebnis aber sichtbare
    /// Mängel hat: eine Overfull-Box über der Schwelle, ein fehlendes Zeichen
    /// oder eine Trennmuster-/Sprachwarnung.
    #[must_use]
    pub fn has_warnings(&self) -> bool {
        !self.overfull.is_empty()
            || !self.missing_chars.is_empty()
            || !self.language_warnings.is_empty()
    }
}

/// Kürzt `text` auf `max` Zeichen (mit „…“).
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(max).collect();
    clipped.push('…');
    clipped
}

/// Liest die Seitenzahl aus dem letzten „Output written on“.
///
/// # Rückgabe
/// `Some(n)` für „(n pages“/„(n page“, `Some(0)` für „No pages of output“,
/// sonst `None`.
fn parse_pages(log: &str) -> Option<u32> {
    let Some(start) = log.rfind("Output written on") else {
        return log.contains("No pages of output").then_some(0);
    };
    let window: String = log[start..]
        .chars()
        .filter(|c| *c != '\n' && *c != '\r')
        .take(PAGES_SEARCH_WINDOW)
        .collect();
    // Dateinamen dürfen Klammern enthalten: jede „(“ prüfen, bis eine mit
    // „<ziffern> page“ weitergeht.
    for (index, _) in window.match_indices('(') {
        let rest = &window[index + 1..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            continue;
        }
        if rest[digits.len()..].starts_with(" page") {
            return digits.parse().ok();
        }
    }
    None
}

/// Zerlegt eine Overfull-Kopfzeile in (Überstand in pt, Zeile).
///
/// Beispiele: `Overfull \hbox (55.1234pt too wide) in paragraph at lines
/// 120--125`, `Overfull \hbox (3.0pt too wide) detected at line 42`,
/// `Overfull \vbox (12.0pt too high) has occurred while \output is active`.
fn parse_box_header(line: &str) -> Option<(f64, Option<u32>)> {
    let open = line.find('(')?;
    let rest = &line[open + 1..];
    let end = rest.find("pt")?;
    let pt: f64 = rest[..end].trim().parse().ok()?;
    let source_line = ["at lines ", "at line "].iter().find_map(|marker| {
        let position = line.find(marker)?;
        let digits: String = line[position + marker.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    });
    Some((pt, source_line))
}

/// Die Zeichenangabe einer „Missing character“-Zeile, z. B. „ß (U+00DF) in
/// font cmr10“.
fn parse_missing_char(line: &str) -> Option<String> {
    let marker = "There is no ";
    let start = line.find(marker)? + marker.len();
    let rest = line[start..].trim_end();
    let rest = rest.strip_suffix('!').unwrap_or(rest);
    let rest = rest.trim();
    (!rest.is_empty()).then(|| clip(rest, MAX_CONTEXT_CHARS))
}

/// `true` für den Beginn einer Sprachwarnung (fehlende Trennmuster,
/// unbekannte Sprache).
fn is_language_warning_start(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("hyphenation patterns")
        || (lower.contains("babel") && lower.contains("unknown language"))
        || (lower.contains("babel") && lower.contains("not defined"))
        || (lower.contains("polyglossia") && lower.contains("language"))
        || lower.contains("no hyphenation")
}

/// `true` für Fortsetzungszeilen einer Paketwarnung (`(babel)  …`).
fn is_continuation(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("(babel)") || trimmed.starts_with("(polyglossia)")
}

/// Entfernt das Präfix `(babel)` und fasst Leerraum zusammen.
fn continuation_text(line: &str) -> String {
    let trimmed = line.trim_start();
    let without = trimmed
        .strip_prefix("(babel)")
        .or_else(|| trimmed.strip_prefix("(polyglossia)"))
        .unwrap_or(trimmed);
    without.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Liest Seitenzahl, Overfull-/Underfull-Boxen, fehlende Zeichen und
/// Sprachwarnungen aus einem TeX-Log.
///
/// # Beschreibung
/// Rein, ohne I/O. Overfull-Boxen über [`OVERFULL_THRESHOLD_PT`] kommen mit
/// Zeile, Überstand und dem Textausschnitt der Folgezeile in den Bericht.
/// Mehrzeilige babel-Warnungen (`(babel)`-Fortsetzungszeilen) werden zu einer
/// Meldung zusammengefügt.
///
/// # Argumente
/// - `log` (`&str`): Inhalt der `.log`-Datei (oder ersatzweise die
///   Prozessausgabe).
///
/// # Rückgabe
/// Der [`LogReport`]; ein leeres Log ergibt einen leeren Bericht.
#[must_use]
pub fn parse_build_log(log: &str) -> LogReport {
    let lines: Vec<&str> = log.lines().collect();
    let mut report = LogReport {
        pages: parse_pages(log),
        ..LogReport::default()
    };
    let mut seen_missing: BTreeSet<String> = BTreeSet::new();
    let mut seen_language: BTreeSet<String> = BTreeSet::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim_end();
        if line.starts_with("Overfull \\hbox") || line.starts_with("Overfull \\vbox") {
            report.overfull_total += 1;
            if let Some((pt, source_line)) = parse_box_header(line)
                && pt > OVERFULL_THRESHOLD_PT
                && report.overfull.len() < MAX_OVERFULL
            {
                let context = lines
                    .iter()
                    .skip(index + 1)
                    .take(3)
                    .map(|candidate| candidate.trim())
                    .find(|candidate| !candidate.is_empty())
                    .map(|candidate| clip(candidate, MAX_CONTEXT_CHARS))
                    .unwrap_or_default();
                report.overfull.push(OverfullBox {
                    line: source_line,
                    pt: (pt * 100.0).round() / 100.0,
                    context,
                });
            }
        } else if line.starts_with("Underfull \\hbox") || line.starts_with("Underfull \\vbox") {
            report.underfull_count += 1;
        } else if line.contains("Missing character: There is no") {
            if let Some(missing) = parse_missing_char(line)
                && report.missing_chars.len() < MAX_MISSING_CHARS
                && seen_missing.insert(missing.clone())
            {
                report.missing_chars.push(missing);
            }
        } else if is_language_warning_start(line) {
            let mut message = line.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut next = index + 1;
            while next < lines.len() && is_continuation(lines[next]) {
                message.push(' ');
                message.push_str(&continuation_text(lines[next]));
                next += 1;
            }
            let message = clip(&message, 300);
            if report.language_warnings.len() < MAX_LANGUAGE_WARNINGS
                && seen_language.insert(message.clone())
            {
                report.language_warnings.push(message);
            }
            index = next;
            continue;
        }
        index += 1;
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    /// Ausschnitt eines echten XeLaTeX-Logs (gekürzt, Pfade neutralisiert):
    /// fehlende deutsche Trennmuster, drei Overfull-Boxen (eine unter 1 pt),
    /// zwei Underfull-Boxen, ein fehlendes Zeichen und ein umbrochenes
    /// „Output written on“.
    const SAMPLE_LOG: &str = "\
This is XeTeX, Version 3.141592653-2.6-0.999995 (TeX Live 2023/Debian) (preloaded format=xelatex 2024.1.1)  24 SEP 2026 10:00
entering extended mode
 restricted \\write18 enabled.
**bericht.tex
(./bericht.tex
LaTeX2e <2023-11-01> patch level 1
(/usr/share/texlive/texmf-dist/tex/generic/babel/babel.sty

Package babel Warning: No hyphenation patterns were preloaded for
(babel)                the language 'German' into the format.
(babel)                Please, configure your TeX system to add them and
(babel)                rebuild the format. Now I will use the patterns
(babel)                preloaded for \\language=0 instead on input line 12.

)
Overfull \\hbox (55.12345pt too wide) in paragraph at lines 120--125
[]\\TU/DejaVuSerif(0)/m/n/10.95 Die Konfigurationsdatei liegt unter
 []

Overfull \\hbox (0.4pt too wide) in paragraph at lines 130--131
[]\\TU/DejaVuSerif(0)/m/n/10.95 fast passend
 []

Underfull \\hbox (badness 10000) in paragraph at lines 140--142

Underfull \\vbox (badness 10000) has occurred while \\output is active []

Overfull \\hbox (3.5pt too wide) detected at line 212
\\TU/DejaVuSansMono(0)/m/n/9 cargo run --release --bin sehr-langer-name
 []

Missing character: There is no ✓ (U+2713) in font DejaVu Serif/OT:script=latn;language=dflt;!
Missing character: There is no ✓ (U+2713) in font DejaVu Serif/OT:script=latn;language=dflt;!
[1] [2] [3] [4] [5] [6] [7] [8] [9] [10] [11] [12] (./bericht.aux) )
Output written on /workspace/projekte/ein-sehr-langer-ordnername/bericht.pdf (1
2 pages).
";

    #[test]
    fn test_parse_build_log_reads_pages_overfull_underfull_and_warnings() -> TestResult {
        let report = parse_build_log(SAMPLE_LOG);
        assert_eq!(report.pages, Some(12), "umbrochene Seitenzahl");
        assert_eq!(report.overfull_total, 3);
        assert_eq!(report.overfull.len(), 2, "0.4 pt liegt unter der Schwelle");
        let first = report
            .overfull
            .first()
            .ok_or(TestError::Missing("overfull[0]"))?;
        assert_eq!(first.line, Some(120));
        assert!((first.pt - 55.12).abs() < 1e-9, "{}", first.pt);
        assert!(
            first.context.contains("Die Konfigurationsdatei"),
            "{}",
            first.context
        );
        let second = report
            .overfull
            .get(1)
            .ok_or(TestError::Missing("overfull[1]"))?;
        assert_eq!(second.line, Some(212));
        assert!((second.pt - 3.5).abs() < 1e-9);
        assert_eq!(report.underfull_count, 2);
        assert_eq!(
            report.missing_chars,
            ["✓ (U+2713) in font DejaVu Serif/OT:script=latn;language=dflt;"]
        );
        assert_eq!(report.language_warnings.len(), 1);
        let warning = &report.language_warnings[0];
        assert!(
            warning.starts_with("Package babel Warning: No hyphenation patterns"),
            "{warning}"
        );
        assert!(warning.contains("the language 'German'"), "{warning}");
        assert!(report.has_warnings());
        Ok(())
    }

    #[test]
    fn test_parse_build_log_clean_log_has_no_warnings() {
        let log = "This is XeTeX\n[1] [2]\nOutput written on main.pdf (2 pages).\n";
        let report = parse_build_log(log);
        assert_eq!(report.pages, Some(2));
        assert!(report.overfull.is_empty());
        assert_eq!(report.underfull_count, 0);
        assert!(!report.has_warnings());
    }

    #[test]
    fn test_parse_pages_variants() {
        assert_eq!(
            parse_pages("Output written on main.pdf (1 page, 1234 bytes)."),
            Some(1)
        );
        assert_eq!(
            parse_pages("Output written on bericht (final).pdf (7 pages)."),
            Some(7),
            "Klammern im Dateinamen"
        );
        assert_eq!(parse_pages("No pages of output."), Some(0));
        assert_eq!(parse_pages("! Emergency stop."), None);
    }

    #[test]
    fn test_small_overfull_boxes_do_not_raise_warnings() {
        let log = "Overfull \\hbox (0.9pt too wide) in paragraph at lines 3--4\n[]x\n";
        let report = parse_build_log(log);
        assert_eq!(report.overfull_total, 1);
        assert!(report.overfull.is_empty());
        assert!(!report.has_warnings());
    }
}
