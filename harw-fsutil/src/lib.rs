//! Sichere Dateisystem-Grundbausteine für den `harw`-Harness.
//!
//! Gerüst aus Welle W0a. Inhalt (symlinkfestes Öffnen, `write_atomic`,
//! begrenzter Walk ohne Symlink-Folge) liefert Agent W0B-03 gemäß
//! `docs/remediation/CONTRACTS.md` §fsutil.
#![forbid(unsafe_code)]
