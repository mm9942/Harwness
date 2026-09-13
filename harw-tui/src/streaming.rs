#![allow(dead_code)]
//! Streaming-Delta-Akkumulator für LLM-Ausgaben (Spec §2.9 + Cluster D §1).
//!
//! Aktuell nicht verwendet — der Chat-Loop übergibt vollständige Antworten in
//! einem Zug an die TUI. Modul bleibt als Baustein für echtes Provider-seitiges
//! Token-Streaming erhalten.
//!
//! # Verantwortlichkeit
//! Dieses Modul stellt [`StreamCollector`] bereit — einen reinen, TTY-freien
//! Puffer, der eingehende LLM-Token-Deltas akkumuliert und vollständige
//! Zeilen (bis zum letzten `\n`) zeilengenau freigibt.
//!
//! # Schlüsseltypen
//! - [`StreamCollector`] — einziger öffentlicher Typ dieses Moduls
//!
//! # Nebenläufigkeitsmodell
//! Der Collector ist nicht `Send`/`Sync`; er wird ausschließlich im
//! TUI-Event-Thread besessen. Kein `Arc`, kein `Mutex`.
//!
//! # Fehlertypen
//! Keine — alle Methoden sind infallibel.
//!
//! # Beispiel
//! ```ignore
//! use harw_tui::streaming::StreamCollector;
//!
//! let mut col = StreamCollector::new();
//! col.push_delta("Hallo ");
//! col.push_delta("Welt\n");
//! assert_eq!(col.commit_complete_lines(), Some("Hallo Welt\n".to_owned()));
//! let rest = col.finalize();
//! assert!(rest.is_empty());
//! ```

/// Akkumuliert LLM-Streaming-Deltas und gibt vollständige Zeilen frei.
///
/// # Beschreibung
/// `StreamCollector` implementiert den **Newline-Gate**-Mechanismus aus dem
/// Codex-TUI-Referenzdesign (Cluster D §1, `MarkdownStreamCollector`):
/// Partielle Zeilen werden nie freigegeben — erst wenn mindestens ein `\n`
/// im noch nicht committeten Bereich liegt, liefert [`commit_complete_lines`]
/// einen Textblock zurück.
///
/// `buffer` wächst monoton bis [`finalize`] aufgerufen wird.
/// `committed_len` ist stets ein gültiger Byte-Offset innerhalb von `buffer`.
///
/// # Invarianten
/// - `committed_len <= buffer.len()`
/// - Der Offset `committed_len` liegt immer auf einer UTF-8-Zeichengrenze
///   (garantiert, weil Schnitte nur an `\n`-Bytes erfolgen, die 1 Byte breit sind).
///
/// [`commit_complete_lines`]: StreamCollector::commit_complete_lines
/// [`finalize`]: StreamCollector::finalize
#[derive(Debug, Default, Clone)]
pub(crate) struct StreamCollector {
    /// Vollständiger akkumulierter Text seit Beginn des aktuellen Turns.
    buffer: String,
    /// Byte-Offset: alles vor diesem Index wurde bereits per
    /// [`commit_complete_lines`] oder [`finalize`] ausgeliefert.
    ///
    /// [`commit_complete_lines`]: StreamCollector::commit_complete_lines
    committed_len: usize,
}

impl StreamCollector {
    /// Erzeugt einen leeren Collector.
    ///
    /// # Beschreibung
    /// Äquivalent zu `StreamCollector::default()`.
    ///
    /// # Rückgabe
    /// Neuer, leerer [`StreamCollector`].
    ///
    /// # Beispiel
    /// ```ignore
    /// use harw_tui::streaming::StreamCollector;
    /// let col = StreamCollector::new();
    /// assert!(col.is_empty());
    /// ```
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Hängt ein eingehendes LLM-Token-Delta an den internen Puffer an.
    ///
    /// # Beschreibung
    /// Die Methode ist die einzige Schreiboperation am Puffer. Sie ist
    /// O(delta.len()) und führt keine Allokation durch, solange die
    /// Kapazität des Puffers ausreicht.
    ///
    /// # Argumente
    /// - `delta` (`&str`): Neues Token-Fragment vom LLM. Kann leer, ein
    ///   einzelnes Zeichen oder mehrere Zeilen auf einmal enthalten.
    ///
    /// # Nebenläufigkeit
    /// Nicht thread-sicher — Caller muss sicherstellen, dass der Collector
    /// nicht aus mehreren Threads gleichzeitig benutzt wird.
    ///
    /// # Beispiel
    /// ```ignore
    /// use harw_tui::streaming::StreamCollector;
    /// let mut col = StreamCollector::new();
    /// col.push_delta("Token ");
    /// col.push_delta("two\n");
    /// ```
    pub(crate) fn push_delta(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    /// Gibt alle vollständigen Zeilen seit dem letzten Commit zurück.
    ///
    /// # Beschreibung
    /// Sucht im noch nicht ausgelieferten Bereich (`buffer[committed_len..]`)
    /// nach dem **letzten** `\n`. Ist eines vorhanden, wird der Textabschnitt
    /// von `committed_len` bis einschließlich dieses `\n` zurückgegeben und
    /// `committed_len` entsprechend vorgerückt.
    ///
    /// Mehrere Zeilen innerhalb eines einzigen Deltas werden in einem einzigen
    /// Aufruf vollständig freigegeben.
    ///
    /// # Rückgabe
    /// - `Some(String)` — Textblock mit einer oder mehreren vollständigen Zeilen.
    /// - `None` — kein `\n` im uncommitteten Bereich; keine Ausgabe.
    ///
    /// # Beispiel
    /// ```ignore
    /// use harw_tui::streaming::StreamCollector;
    /// let mut col = StreamCollector::new();
    /// col.push_delta("Teil");
    /// assert_eq!(col.commit_complete_lines(), None); // kein \n
    /// col.push_delta(" eins\nZeile zwei\n");
    /// assert!(col.commit_complete_lines().is_some());
    /// ```
    pub(crate) fn commit_complete_lines(&mut self) -> Option<String> {
        let unseen = &self.buffer[self.committed_len..];
        // Letzten \n im uncommitteten Bereich suchen.
        let last_newline_rel = unseen.rfind('\n')?;
        // +1: schließt das \n-Byte selbst mit ein.
        let end = self.committed_len + last_newline_rel + 1;
        let chunk = self.buffer[self.committed_len..end].to_owned();
        self.committed_len = end;
        Some(chunk)
    }

    /// Gibt den verbleibenden uncommitteten Rest zurück und schließt den Puffer ab.
    ///
    /// # Beschreibung
    /// Wird am Ende eines LLM-Turns aufgerufen, um auch eine letzte unvollständige
    /// Zeile (ohne abschließendes `\n`) auszuliefern. Nach dem Aufruf zeigt
    /// `committed_len` auf das Ende von `buffer`.
    ///
    /// Der zurückgegebene Rest kann leer sein, wenn seit dem letzten
    /// [`commit_complete_lines`]-Aufruf keine weiteren Bytes eingetroffen sind.
    ///
    /// # Rückgabe
    /// Verbleibender uncommitteter Text (kann `""` sein).
    ///
    /// # Beispiel
    /// ```ignore
    /// use harw_tui::streaming::StreamCollector;
    /// let mut col = StreamCollector::new();
    /// col.push_delta("kein Newline am Ende");
    /// let rest = col.finalize();
    /// assert_eq!(rest, "kein Newline am Ende");
    /// ```
    ///
    /// [`commit_complete_lines`]: StreamCollector::commit_complete_lines
    pub(crate) fn finalize(&mut self) -> String {
        let rest = self.buffer[self.committed_len..].to_owned();
        self.committed_len = self.buffer.len();
        rest
    }

    /// Gibt `true` zurück, wenn der gesamte Puffer leer ist.
    ///
    /// # Beschreibung
    /// Prüft, ob seit der Erzeugung (oder dem letzten Reset-äquivalenten Zustand)
    /// überhaupt Bytes angekommen sind. Nützlich, um eine leere Antwort vom
    /// Modell erkennen zu können.
    ///
    /// # Rückgabe
    /// `true` wenn `buffer` leer ist, sonst `false`.
    ///
    /// # Beispiel
    /// ```ignore
    /// use harw_tui::streaming::StreamCollector;
    /// let mut col = StreamCollector::new();
    /// assert!(col.is_empty());
    /// col.push_delta("x");
    /// assert!(!col.is_empty());
    /// ```
    pub(crate) fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::StreamCollector;

    /// Kein `\n` im Delta → `commit_complete_lines` muss `None` zurückgeben.
    #[test]
    fn test_commit_no_newline_returns_none() {
        let mut col = StreamCollector::new();
        col.push_delta("partielle Zeile ohne Newline");
        assert_eq!(col.commit_complete_lines(), None);
    }

    /// Genau ein `\n` → exakt diese eine Zeile wird zurückgegeben.
    #[test]
    fn test_commit_single_newline_returns_line() {
        let mut col = StreamCollector::new();
        col.push_delta("Hallo Welt\n");
        let result = col.commit_complete_lines();
        assert_eq!(result, Some("Hallo Welt\n".to_owned()));
        // Zweiter Aufruf: nichts mehr committed → None.
        assert_eq!(col.commit_complete_lines(), None);
    }

    /// Zwei Deltas bilden zusammen eine Zeile (Newline erst im zweiten Delta).
    #[test]
    fn test_commit_two_deltas_form_one_line() {
        let mut col = StreamCollector::new();
        col.push_delta("Hälfte ");
        assert_eq!(col.commit_complete_lines(), None);
        col.push_delta("zwei\n");
        let result = col.commit_complete_lines();
        assert_eq!(result, Some("Hälfte zwei\n".to_owned()));
    }

    /// `finalize` gibt den Rest ohne `\n` zurück und setzt `committed_len` auf Ende.
    #[test]
    fn test_finalize_flushes_rest_without_newline() {
        let mut col = StreamCollector::new();
        col.push_delta("Zeile eins\n");
        // Erste Zeile committen.
        assert!(col.commit_complete_lines().is_some());
        // Rest ohne Newline.
        col.push_delta("kein Newline");
        let rest = col.finalize();
        assert_eq!(rest, "kein Newline");
        // Nach finalize: commit gibt None, finalize gibt leeren String.
        assert_eq!(col.commit_complete_lines(), None);
        assert_eq!(col.finalize(), "");
    }

    /// Mehrere Zeilen in einem einzigen Delta → alle auf einmal freigegeben.
    #[test]
    fn test_commit_multiple_lines_in_one_delta() {
        let mut col = StreamCollector::new();
        col.push_delta("Zeile A\nZeile B\nZeile C\n");
        let result = col.commit_complete_lines();
        assert_eq!(result, Some("Zeile A\nZeile B\nZeile C\n".to_owned()));
        assert_eq!(col.commit_complete_lines(), None);
    }

    /// Letztes `\n` entscheidet: partielle letzte Zeile bleibt zurück.
    #[test]
    fn test_commit_partial_last_line_stays() {
        let mut col = StreamCollector::new();
        col.push_delta("komplett\npartial");
        let result = col.commit_complete_lines();
        // Nur bis einschließlich erstem \n zurückgegeben.
        assert_eq!(result, Some("komplett\n".to_owned()));
        // "partial" bleibt im Puffer.
        assert_eq!(col.commit_complete_lines(), None);
        let rest = col.finalize();
        assert_eq!(rest, "partial");
    }

    /// `is_empty` verhält sich korrekt vor und nach `push_delta`.
    #[test]
    fn test_is_empty() {
        let mut col = StreamCollector::new();
        assert!(col.is_empty());
        col.push_delta("x");
        assert!(!col.is_empty());
    }
}
