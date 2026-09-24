//! Sichere Dateisystem-Grundbausteine für den `harw`-Harness.
//!
//! Dieses Crate bündelt die symlinkfesten Dateioperationen, die mehrere
//! Crates (`harw-session-store`, `harw-memory`, `harw-secrets`,
//! `harw-tool-fs`, `harw-home`, `harw-provider-http`) bisher je für sich mit
//! hartkodierten `O_NOFOLLOW`-Zahlen nachgebaut haben. Die Zahl ist
//! architekturabhängig (x86_64 `0o400000`, aarch64 `0o100000` — dort ist
//! `0o400000` `O_LARGEFILE`); hier kommen alle Flags ausschließlich aus
//! [`rustix::fs::OFlags`].
//!
//! - **Öffnen** ([`open`]): [`open_nofollow`], [`open_dir_nofollow`] und
//!   [`open_beneath`] (`openat2` mit `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS |
//!   RESOLVE_NO_MAGICLINKS`, komponentenweiser Fallback). `open_nofollow` und
//!   `open_beneath` öffnen mit `O_NONBLOCK` und geben nur reguläre Dateien
//!   und Verzeichnisse (danach blockierend) zurück, FIFOs und Geräte nie.
//! - **Atomares Schreiben** ([`atomic`]): [`write_atomic`] — Tempdatei im
//!   Zielverzeichnis, `fsync`, `rename`; ein Ziel-Symlink wird ersetzt, nie
//!   gefolgt. Setzt ein nicht von Dritten beschreibbares Zielverzeichnis
//!   voraus.
//! - **Rechteprüfung** ([`perm`]): [`ensure_private_regular`].
//! - **Begrenzter Walk** ([`walk`]): [`walk_beneath`] folgt nie Symlinks,
//!   hat Tiefen-, Anzahl- und Zeitgrenzen und eine deterministische
//!   Reihenfolge.
//!
//! # Plattform
//! Das Crate ist Unix-only. Unter Linux nutzt [`open_beneath`] `openat2`;
//! auf anderen Unix-Systemen (und bei `ENOSYS` unter Linux) greift der
//! komponentenweise Fallback mit `openat(O_NOFOLLOW)`.
//!
//! # Concurrency
//! Alle Funktionen sind zustandslos und `Send + Sync`. Die Sicherheit gegen
//! Symlink-Tausch beruht auf dirfd-relativen Syscalls, nicht auf vorherigen
//! Prüfungen (kein TOCTOU zwischen Prüfen und Öffnen).
//!
//! # Errors
//! Alle fallierbaren Operationen liefern [`std::io::Error`]; Syscall-Fehler
//! behalten ihren `errno` ([`std::io::Error::raw_os_error`]). Ungültige
//! Argumente (z. B. `..` in [`open_beneath`]) liefern
//! [`std::io::ErrorKind::InvalidInput`].
//!
//! # Examples
//! ```rust,no_run
//! use harw_fsutil::{
//!     AtomicWriteOptions, OpenMode, ensure_private_regular, open_nofollow, write_atomic,
//! };
//! use std::path::Path;
//!
//! let path = Path::new("/home/user/.harw/auth.toml");
//! write_atomic(path, b"token = \"...\"\n", AtomicWriteOptions::private())?;
//! let file = open_nofollow(path, OpenMode::read_only())?;
//! ensure_private_regular(&file)?;
//! # Ok::<(), std::io::Error>(())
//! ```

#![forbid(unsafe_code)]

pub mod atomic;
pub mod open;
pub mod perm;
pub mod walk;

#[cfg(test)]
mod test_support;

pub use atomic::{AtomicWriteOptions, write_atomic};
pub use open::{OpenMode, is_symlink_loop, open_beneath, open_dir_nofollow, open_nofollow};
pub use perm::ensure_private_regular;
pub use walk::{EntryType, WalkBeneath, WalkEntry, WalkLimits, WalkStop, walk_beneath};
