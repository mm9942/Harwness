//! Braille-Spinner für den „denkt…"-Zustand der TUI.
//!
//! # Zweck
//! Dieses Modul stellt einen animierten Braille-Spinner bereit, der während
//! eines laufenden Modell-Turns angezeigt wird und dem Benutzer signalisiert,
//! dass das Modell gerade eine Antwort generiert.
//!
//! # Verantwortung
//! - Verwaltung des aktuellen Animationsframes (Index in `BRAILLE_FRAMES`).
//! - Steuerung des Aktivierungszustands (`active`).
//! - Liefern des aktuellen Glyphs zur Darstellung durch den Aufrufer.
//!
//! Dieses Modul enthält keinerlei Rendering-Logik; der Aufrufer ruft
//! `glyph()` ab und platziert das Zeichen selbst im Layout.
//!
//! # Nebenläufigkeit
//! `Spinner` ist weder `Send` noch `Sync`. Er ist ausschließlich für
//! Single-Thread-Nutzung im TUI-Render-Loop vorgesehen. Keine Locks,
//! keine Channels, keine Arc-Nutzung.
//!
//! # Fehlertypen
//! Keine – alle Operationen sind unfehlbar.

/// Die zehn Braille-Animationsframes für den Spinner.
const BRAILLE_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Animierter Braille-Spinner für den „denkt…"-Zustand.
///
/// # Beschreibung
/// Hält den internen Zustand des Spinners: welche Frames verwendet werden,
/// welcher Frame gerade aktiv ist und ob der Spinner überhaupt läuft.
/// Durch regelmäßiges Aufrufen von [`tick`](Spinner::tick) im Render-Takt
/// schreitet die Animation weiter.
pub(crate) struct Spinner {
    /// Slice mit den Braille-Zeichen-Frames; zeigt auf `BRAILLE_FRAMES`.
    frames: &'static [&'static str],
    /// Index des aktuell angezeigten Frames.
    idx: usize,
    /// Gibt an, ob der Spinner gerade läuft.
    active: bool,
}

impl Spinner {
    /// Erstellt einen neuen, inaktiven Spinner mit Braille-Frames.
    ///
    /// # Beschreibung
    /// Der Spinner startet im Zustand `active = false` mit `idx = 0`.
    /// Er zeigt den ersten Braille-Frame, bewegt sich aber erst nach
    /// einem Aufruf von [`start`](Spinner::start) weiter.
    ///
    /// # Rückgabe
    /// Ein neuer `Spinner`-Wert.
    pub(crate) fn new() -> Self {
        Self {
            frames: BRAILLE_FRAMES,
            idx: 0,
            active: false,
        }
    }

    /// Aktiviert den Spinner und setzt die Animation auf den ersten Frame zurück.
    ///
    /// # Beschreibung
    /// Setzt `active = true` und `idx = 0`, sodass die nächste Darstellung
    /// immer mit dem ersten Braille-Frame beginnt.
    pub(crate) fn start(&mut self) {
        self.active = true;
        self.idx = 0;
    }

    /// Deaktiviert den Spinner.
    ///
    /// # Beschreibung
    /// Setzt `active = false`. Der aktuelle Frame-Index bleibt erhalten;
    /// `glyph()` liefert weiterhin den zuletzt angezeigten Frame.
    pub(crate) fn stop(&mut self) {
        self.active = false;
    }

    /// Schaltet einen Animationsframe weiter, sofern der Spinner aktiv ist.
    ///
    /// # Beschreibung
    /// Inkrementiert `idx` um eins und wraps am Ende der Frame-Liste zurück
    /// zum Anfang (`idx = (idx + 1) % frames.len()`).
    /// Ist der Spinner nicht aktiv, passiert nichts.
    pub(crate) fn tick(&mut self) {
        if self.active && !self.frames.is_empty() {
            self.idx = (self.idx + 1) % self.frames.len();
        }
    }

    /// Gibt an, ob der Spinner momentan aktiv ist.
    ///
    /// # Rückgabe
    /// `true` wenn der Spinner läuft, `false` sonst.
    #[must_use]
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    /// Gibt das Braille-Zeichen des aktuellen Animationsframes zurück.
    ///
    /// # Beschreibung
    /// Liest `frames[idx]` aus. Falls `frames` leer wäre (was bei
    /// `BRAILLE_FRAMES` nicht eintreten kann), wird ein Leerzeichen
    /// zurückgegeben, um einen Panic zu vermeiden.
    ///
    /// # Rückgabe
    /// Ein statischer String-Slice mit dem aktuellen Braille-Zeichen
    /// (z. B. `"⠋"`) oder `" "` als Fallback bei leerer Frame-Liste.
    #[must_use]
    pub(crate) fn glyph(&self) -> &'static str {
        self.frames.get(self.idx).copied().unwrap_or(" ")
    }
}

impl Default for Spinner {
    /// Erstellt einen Standard-Spinner; delegiert an [`Spinner::new`].
    ///
    /// # Rückgabe
    /// Ein inaktiver `Spinner` mit Braille-Frames.
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_inactive() {
        let s = Spinner::new();
        assert!(!s.is_active(), "frischer Spinner muss inaktiv sein");
        assert_eq!(s.glyph(), "⠋", "frischer Spinner muss ersten Frame zeigen");
    }

    #[test]
    fn test_start_activates() {
        let mut s = Spinner::new();
        s.start();
        assert!(s.is_active(), "nach start() muss is_active() true sein");
    }

    #[test]
    fn test_stop_deactivates() {
        let mut s = Spinner::new();
        s.start();
        s.stop();
        assert!(!s.is_active(), "nach stop() muss is_active() false sein");
    }

    #[test]
    fn test_tick_advances_when_active() {
        let mut s = Spinner::new();
        s.start();
        s.tick();
        assert_eq!(s.glyph(), "⠙", "nach einem tick() muss Frame 1 aktiv sein");
    }

    #[test]
    fn test_tick_noop_when_inactive() {
        let mut s = Spinner::new();
        // kein start() — Spinner ist inaktiv
        s.tick();
        assert_eq!(
            s.glyph(),
            "⠋",
            "tick() ohne start() darf den Frame nicht verändern"
        );
    }

    #[test]
    fn test_tick_wraps_around() {
        let mut s = Spinner::new();
        s.start();
        // BRAILLE_FRAMES hat 10 Einträge; 10 ticks → wieder bei Index 0
        for _ in 0..10 {
            s.tick();
        }
        assert_eq!(
            s.glyph(),
            "⠋",
            "nach 10 ticks muss der Spinner wieder bei Frame 0 sein"
        );
    }

    #[test]
    fn test_glyph_returns_current_frame() {
        let mut s = Spinner::new();
        s.start();
        s.tick(); // → "⠙"
        s.tick(); // → "⠹"
        s.tick(); // → "⠸"
        assert_eq!(s.glyph(), "⠸", "nach 3 ticks muss Frame 3 aktiv sein");
    }

    #[test]
    fn test_default_equals_new() {
        let a = Spinner::new();
        let b = Spinner::default();
        assert_eq!(a.is_active(), b.is_active());
        assert_eq!(a.glyph(), b.glyph());
    }
}
