//! `/image <pfad>`: Bilder für die nächste Nachricht vormerken.
//!
//! # Beschreibung
//! Der Befehl liest die Datei (höchstens das Limit des Medienspeichers),
//! lässt sie von [`MediaStore::put`] prüfen und von Metadaten befreien und
//! merkt die Referenz vor. Die nächste abgeschickte Nachricht trägt alle
//! vorgemerkten Bilder und leert die Liste. Die Bytes liegen im Medienspeicher
//! unter dem Home (`<home>/media`), nie im Verlauf.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_media::{MediaLimits, MediaStore};
use harw_protocol::MediaRef;

/// Wie viele Bilder höchstens an einer Nachricht hängen.
pub(crate) const MAX_PENDING_IMAGES: usize = 10;

/// Nutzungshinweis für `/image` ohne Argument.
pub(crate) const USAGE: &str = "Bitte einen Pfad angeben: /image <pfad> (PNG, JPEG, GIF oder WebP) — /image clear verwirft die vorgemerkten Bilder";

/// Vorgemerkte Bilder und der Speicher, in den sie gelegt werden.
#[derive(Default)]
pub(crate) struct ImageAttachments {
    store: Option<Arc<MediaStore>>,
    pending: Vec<MediaRef>,
}

impl ImageAttachments {
    /// Verarbeitet das Argument von `/image` und liefert die Meldung für den
    /// Nutzer. `home` ist das Harwness-Home, `base` das Projektverzeichnis
    /// für relative Pfade.
    pub(crate) fn command(&mut self, args: &str, home: Option<&Path>, base: &Path) -> String {
        let args = args.trim();
        if args.is_empty() {
            return USAGE.to_owned();
        }
        if args == "clear" {
            let dropped = std::mem::take(&mut self.pending).len();
            return format!("{dropped} vorgemerkte(s) Bild(er) verworfen.");
        }
        match self.attach(args, home, base) {
            Ok(note) => note,
            Err(reason) => format!("/image: {reason}"),
        }
    }

    /// Die vorgemerkten Bilder für den nächsten Turn; leert die Liste.
    pub(crate) fn take(&mut self) -> Vec<MediaRef> {
        std::mem::take(&mut self.pending)
    }

    /// Wie viele Bilder gerade vorgemerkt sind.
    pub(crate) fn len(&self) -> usize {
        self.pending.len()
    }

    fn attach(&mut self, args: &str, home: Option<&Path>, base: &Path) -> Result<String, String> {
        if self.pending.len() >= MAX_PENDING_IMAGES {
            return Err(format!(
                "höchstens {MAX_PENDING_IMAGES} Bilder pro Nachricht — erst absenden oder /image clear"
            ));
        }
        let path = resolve(args, base);
        let bytes = read_limited(&path)?;
        let store = self.store(home)?;
        let media = store.put(&bytes).map_err(|error| error.to_string())?;
        let note = format!(
            "Bild vorgemerkt: {} — geht mit der nächsten Nachricht mit ({} insgesamt)",
            media.describe(),
            self.pending.len() + 1
        );
        self.pending.push(media);
        Ok(note)
    }

    /// Öffnet den Speicher beim ersten Bild und hängt ihn für den Kern ein.
    fn store(&mut self, home: Option<&Path>) -> Result<Arc<MediaStore>, String> {
        if let Some(store) = &self.store {
            return Ok(Arc::clone(store));
        }
        let home =
            home.ok_or("kein Harwness-Home gefunden, Bilder können nicht abgelegt werden")?;
        let store = Arc::new(MediaStore::open(home.join("media")).map_err(|e| e.to_string())?);
        harw_core::install_media_source(Arc::clone(&store) as _);
        self.store = Some(Arc::clone(&store));
        Ok(store)
    }
}

/// Pfad aus dem Argument: umgebende Anführungszeichen weg, `~/` aufgelöst,
/// relative Pfade gegen das Projektverzeichnis.
fn resolve(args: &str, base: &Path) -> PathBuf {
    let trimmed = args
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_owned();
    let path = match trimmed.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(rest))
            .unwrap_or_else(|| PathBuf::from(&trimmed)),
        None => PathBuf::from(&trimmed),
    };
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

/// Liest eine reguläre Datei, höchstens Limit plus ein Byte.
fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    let limit = MediaLimits::default().max_bytes;
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let meta = file.metadata().map_err(|error| error.to_string())?;
    if !meta.is_file() {
        return Err(format!("{} ist keine Datei", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > limit) {
        return Err(format!("{} ist größer als {limit} Bytes", path.display()));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn png() -> Vec<u8> {
        fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
            let mut out = u32::try_from(data.len())
                .unwrap_or(0)
                .to_be_bytes()
                .to_vec();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0; 4]);
            out
        }
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = 3u32.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        out.extend_from_slice(&chunk(b"IHDR", &ihdr));
        out.extend_from_slice(&chunk(b"IDAT", b"pixel-data"));
        out.extend_from_slice(&chunk(b"IEND", b""));
        out
    }

    #[test]
    fn an_image_is_stored_remembered_and_taken_once() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home)?;
        std::fs::write(dir.path().join("a.png"), png())?;
        let mut images = ImageAttachments::default();

        let note = images.command("a.png", Some(&home), dir.path());
        assert!(note.contains("Bild vorgemerkt"), "{note}");
        assert!(note.contains("3x2"), "{note}");
        assert_eq!(images.len(), 1);
        assert!(home.join("media").is_dir());

        let taken = images.take();
        assert_eq!(taken.len(), 1);
        assert!(images.take().is_empty(), "the list is emptied");
        Ok(())
    }

    #[test]
    fn bad_input_is_a_message_and_nothing_is_remembered() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home)?;
        std::fs::write(dir.path().join("note.txt"), b"just text")?;
        let mut images = ImageAttachments::default();

        assert_eq!(images.command("", Some(&home), dir.path()), USAGE);
        for args in ["missing.png", "note.txt", "."] {
            let note = images.command(args, Some(&home), dir.path());
            assert!(note.starts_with("/image:"), "{args}: {note}");
        }
        let no_home = images.command("note.txt", None, dir.path());
        assert!(no_home.starts_with("/image:"), "{no_home}");
        assert_eq!(images.len(), 0);
        Ok(())
    }

    #[test]
    fn clear_discards_and_the_count_is_capped() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home)?;
        std::fs::write(dir.path().join("a.png"), png())?;
        let mut images = ImageAttachments::default();
        for _ in 0..MAX_PENDING_IMAGES {
            images.command("a.png", Some(&home), dir.path());
        }
        assert_eq!(images.len(), MAX_PENDING_IMAGES);
        let over = images.command("a.png", Some(&home), dir.path());
        assert!(over.starts_with("/image:"), "{over}");
        assert_eq!(images.len(), MAX_PENDING_IMAGES);

        let cleared = images.command("clear", Some(&home), dir.path());
        assert!(cleared.contains("10"), "{cleared}");
        assert_eq!(images.len(), 0);
        Ok(())
    }

    #[test]
    fn quotes_around_a_path_are_dropped() {
        let base = Path::new("/p");
        assert_eq!(resolve("\"a b.png\"", base), PathBuf::from("/p/a b.png"));
        assert_eq!(resolve("/abs/x.png", base), PathBuf::from("/abs/x.png"));
    }
}
