//! Systemzwischenablage für den `/export`-Befehl mit OSC-52-Fallback.
//!
//! Spec-Quelle: `harw-scopes-contract.md` Slice E1 und
//! `nope-permissions-gibt-es-wild-lobster.md` (UI-Stil der Dialoge).
//!
//! # Verantwortung
//! Kopiert Text in die Systemzwischenablage über externe Werkzeuge
//! (`wl-copy`, `xclip`/`xsel`, `pbcopy`) und stellt einen OSC-52-Fallback für
//! den Fall bereit, dass kein Werkzeug erreichbar ist (z. B. über SSH ohne
//! Zwischenablagen-Weiterleitung). Base64 wird ohne zusätzliche Abhängigkeit
//! selbst implementiert.
//!
//! # Schlüsseltypen
//! - [`ClipboardTarget`] — welches Ziel den Text letztlich erhalten hat.
//! - [`copy_to_clipboard`] — versucht ausschließlich Systemwerkzeuge.
//! - [`osc52_sequence`] — baut die rohe OSC-52-Escape-Sequenz.
//! - [`copy_or_sequence`] — versucht zuerst [`copy_to_clipboard`], fällt dann
//!   auf [`osc52_sequence`] zurück.
//!
//! # Nebenläufigkeit
//! Jeder Aufruf spawnt höchstens einen Kindprozess synchron über
//! `std::process::Command` (kein Tokio) und wartet mit einem 5-Sekunden-
//! Zeitlimit per Polling. Kein Shared State zwischen Aufrufen.
//!
//! # Fehlertypen
//! [`crate::export::ExportError::NoClipboard`] bei fehlgeschlagenem Kopieren
//! und wenn der Text für OSC-52 zu groß ist.
//!
//! # Wichtig
//! Der zu kopierende Text wird **niemals** geloggt (kein `tracing`-Aufruf in
//! diesem Modul), da er beliebige sensible Gesprächsinhalte enthalten kann.

use std::env;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::export::ExportError;

/// Zeitlimit, bis ein Zwischenablage-Werkzeug als nicht funktionsfähig gilt.
const CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll-Intervall beim Warten auf das Prozessende.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Obergrenze für OSC-52-Nutzlast in Bytes (viele Terminals begrenzen die
/// Sequenzlänge; oberhalb dieser Grenze wird der Fallback abgelehnt).
const OSC52_MAX_BYTES: usize = 100_000;

/// Alphabet der Standard-Base64-Kodierung (RFC 4648, mit `+`/`/` und `=`-Padding).
const BASE64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Welches Ziel den Text letztlich erhalten hat.
///
/// # Beschreibung
/// Rückgabewert von [`copy_to_clipboard`] und [`copy_or_sequence`], damit der
/// Aufrufer eine passende Rückmeldung anzeigen kann (z. B. „In die
/// Zwischenablage kopiert (Wayland)“).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardTarget {
    /// Über `wl-copy` (Wayland-Zwischenablage) kopiert.
    Wayland,
    /// Über `xclip`/`xsel` (X11-Zwischenablage) kopiert.
    X11,
    /// Über eine OSC-52-Escape-Sequenz an das Terminal übergeben.
    Osc52,
    /// Über `pbcopy` (macOS-Zwischenablage) kopiert.
    MacOs,
}

/// Spawnt `program` mit `args`, schreibt `text` auf `stdin` und wartet mit
/// Zeitlimit auf einen erfolgreichen Exit-Code.
///
/// # Beschreibung
/// Ein fehlgeschlagener `spawn` (z. B. Binärdatei nicht gefunden) gilt als
/// „nicht verfügbar“ und liefert `false`, damit der Aufrufer den nächsten
/// Kandidaten versucht. Nach [`CLIPBOARD_TIMEOUT`] ohne Prozessende wird der
/// Kindprozess beendet und `false` zurückgegeben.
///
/// # Argumente
/// - `program` (`&str`): Name des Binärprogramms.
/// - `args` (`&[&str]`): Kommandozeilenargumente.
/// - `text` (`&str`): auf `stdin` zu schreibender Text.
///
/// # Rückgabe
/// `true` bei erfolgreichem Exit-Code `0`, sonst `false`.
///
/// # Nebenläufigkeit
/// Synchron; blockiert den aufrufenden Thread höchstens [`CLIPBOARD_TIMEOUT`].
fn spawn_and_write(program: &str, args: &[&str], text: &str) -> bool {
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(text.as_bytes()).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        // `stdin` wird hier gedroppt und schließt damit die Pipe, sodass das
        // Zwischenablage-Werkzeug EOF sieht und beendet.
    }

    let deadline = Instant::now() + CLIPBOARD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => return false,
        }
    }
}

/// Kopiert `text` über ein verfügbares Systemwerkzeug in die Zwischenablage.
///
/// # Beschreibung
/// Versucht der Reihe nach: `wl-copy` (nur wenn `WAYLAND_DISPLAY` gesetzt
/// ist), `xclip -selection clipboard` und `xsel --clipboard --input` (nur
/// wenn `DISPLAY` gesetzt ist), dann `pbcopy` auf macOS. Ein nicht
/// gefundenes Binärprogramm oder ein Exit-Code ≠ 0 führt zum nächsten
/// Kandidaten. Enthält **keinen** OSC-52-Fallback — dafür [`copy_or_sequence`]
/// verwenden.
///
/// # Argumente
/// - `text` (`&str`): der zu kopierende Text.
///
/// # Rückgabe
/// Das erfolgreiche [`ClipboardTarget`].
///
/// # Fehler
/// - [`ExportError::NoClipboard`]: kein Kandidat war erfolgreich.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::clipboard::copy_to_clipboard;
/// let _ = copy_to_clipboard("Beispieltext");
/// ```
pub fn copy_to_clipboard(text: &str) -> Result<ClipboardTarget, ExportError> {
    if env::var_os("WAYLAND_DISPLAY").is_some() && spawn_and_write("wl-copy", &[], text) {
        return Ok(ClipboardTarget::Wayland);
    }

    if env::var_os("DISPLAY").is_some() {
        if spawn_and_write("xclip", &["-selection", "clipboard"], text) {
            return Ok(ClipboardTarget::X11);
        }
        if spawn_and_write("xsel", &["--clipboard", "--input"], text) {
            return Ok(ClipboardTarget::X11);
        }
    }

    if cfg!(target_os = "macos") && spawn_and_write("pbcopy", &[], text) {
        return Ok(ClipboardTarget::MacOs);
    }

    Err(ExportError::NoClipboard)
}

/// Kodiert `data` als Standard-Base64 (RFC 4648) ohne Zeilenumbrüche.
///
/// # Beschreibung
/// Handgeschriebene Referenzimplementierung, um keine neue Abhängigkeit
/// einzuführen. Verarbeitet `data` in 3-Byte-Blöcken und füllt den letzten
/// Block bei Bedarf mit `=`-Padding auf.
///
/// # Argumente
/// - `data` (`&[u8]`): die zu kodierenden Rohbytes.
///
/// # Rückgabe
/// Base64-kodierter Text als `String`, leer wenn `data` leer ist.
fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);

        let idx0 = (b0 >> 2) as usize;
        let idx1 = (((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize;
        let idx2 = (((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize;
        let idx3 = (b2 & 0b0011_1111) as usize;

        out.push(BASE64_ALPHABET[idx0] as char);
        out.push(BASE64_ALPHABET[idx1] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[idx2] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[idx3] as char
        } else {
            '='
        });
    }

    out
}

/// Baut die rohe OSC-52-Escape-Sequenz, die ein Terminal anweist, `text` in
/// die Zwischenablage zu übernehmen.
///
/// # Beschreibung
/// Format `\x1b]52;c;<base64>\x07` (`c` = `CLIPBOARD`-Selektion). Die
/// Basis64-Kodierung erfolgt über [`base64_encode`]. Aus Sicherheits- und
/// Kompatibilitätsgründen wird die Nutzlast auf [`OSC52_MAX_BYTES`] Bytes
/// begrenzt — viele Terminals verwerfen oder kappen längere Sequenzen
/// stillschweigend, was ohne diese Grenze zu unbemerktem Datenverlust führen
/// würde.
///
/// # Argumente
/// - `text` (`&str`): der einzubettende Text.
///
/// # Rückgabe
/// Die vollständige Escape-Sequenz als `String`.
///
/// # Fehler
/// - [`ExportError::NoClipboard`]: `text` überschreitet [`OSC52_MAX_BYTES`] Bytes.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::clipboard::osc52_sequence;
/// let seq = osc52_sequence("hi").expect("kurzer Text passt");
/// assert!(seq.starts_with("\x1b]52;c;"));
/// ```
pub fn osc52_sequence(text: &str) -> Result<String, ExportError> {
    if text.len() > OSC52_MAX_BYTES {
        return Err(ExportError::NoClipboard);
    }
    let encoded = base64_encode(text.as_bytes());
    Ok(format!("\x1b]52;c;{encoded}\x07"))
}

/// Kopiert `text` über ein Systemwerkzeug oder liefert eine OSC-52-Sequenz
/// zur Ausgabe durch den Aufrufer.
///
/// # Beschreibung
/// Versucht zuerst [`copy_to_clipboard`]. Scheitert das ausschließlich, weil
/// kein Werkzeug erreichbar war, wird [`osc52_sequence`] als Fallback
/// gebaut; der Aufrufer ist dafür verantwortlich, die zurückgegebene
/// Sequenz roh auf das Terminal zu schreiben (dieses Modul schreibt selbst
/// nichts auf ein Terminal).
///
/// # Argumente
/// - `text` (`&str`): der zu kopierende Text.
///
/// # Rückgabe
/// Tupel aus dem erreichten [`ClipboardTarget`] und, im OSC-52-Fall, der zu
/// schreibenden Sequenz (`Some`); bei einem Systemwerkzeug ist es `None`,
/// da das Werkzeug den Text bereits selbst übernommen hat.
///
/// # Fehler
/// - [`ExportError::NoClipboard`]: weder ein Systemwerkzeug noch OSC-52 (zu groß) verfügbar.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::clipboard::copy_or_sequence;
/// let (_target, maybe_sequence) = copy_or_sequence("Beispieltext")?;
/// # Ok::<(), harw_tui::export::ExportError>(())
/// ```
pub fn copy_or_sequence(text: &str) -> Result<(ClipboardTarget, Option<String>), ExportError> {
    match copy_to_clipboard(text) {
        Ok(target) => Ok((target, None)),
        Err(ExportError::NoClipboard) => {
            let sequence = osc52_sequence(text)?;
            Ok((ClipboardTarget::Osc52, Some(sequence)))
        }
        Err(other) => Err(other),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Bekannte Base64-Testvektoren (RFC 4648 §10) müssen exakt stimmen.
    #[test]
    fn test_base64_encode_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }

    /// Ein Umlaut (Mehrbyte-UTF-8) wird korrekt kodiert.
    #[test]
    fn test_base64_encode_umlaut() {
        // "ü" = 0xC3 0xBC in UTF-8.
        assert_eq!(base64_encode("ü".as_bytes()), "w7w=");
    }

    /// Die OSC-52-Sequenz umschließt die Base64-Nutzlast mit dem korrekten Rahmen.
    #[test]
    fn test_osc52_sequence_framing() {
        let seq = osc52_sequence("ab").expect("kurzer Text passt");
        assert_eq!(seq, "\x1b]52;c;YWI=\x07");
        assert!(seq.starts_with("\x1b]52;c;"));
        assert!(seq.ends_with('\x07'));
    }

    /// Leerer Text ergibt eine leere Base64-Nutzlast, aber einen gültigen Rahmen.
    #[test]
    fn test_osc52_sequence_empty_text() {
        let seq = osc52_sequence("").expect("leerer Text passt");
        assert_eq!(seq, "\x1b]52;c;\x07");
    }

    /// Text oberhalb der Obergrenze wird abgelehnt statt eine überlange Sequenz zu bauen.
    #[test]
    fn test_osc52_sequence_size_cap_rejects_oversized_text() {
        let oversized = "a".repeat(OSC52_MAX_BYTES + 1);
        let err = osc52_sequence(&oversized).expect_err("zu groß muss abgelehnt werden");
        assert!(matches!(err, ExportError::NoClipboard));
    }

    /// Text exakt an der Obergrenze wird noch akzeptiert.
    #[test]
    fn test_osc52_sequence_size_cap_boundary_accepted() {
        let exact = "a".repeat(OSC52_MAX_BYTES);
        assert!(osc52_sequence(&exact).is_ok());
    }
}
