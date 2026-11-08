//! Kanonische, längenpräfixierte Bytefolgen (geteilt mit dem Signierteil).
//!
//! # Verantwortungsbereich
//! Die Bausteine, aus denen [`crate::action::WardenAction::binding_digest`]
//! und [`crate::target::CgroupTarget::put_canonical`] ihre Bytes bilden und
//! aus denen `harw-dod-warden-proto` die MAC-Eingabe von Proof v2 baut. Jedes
//! Feld wird als `u64` Big-Endian-Länge gefolgt von den Rohbytes geschrieben
//! — dadurch ist die Zerlegung eindeutig (keine zwei verschiedenen
//! Feldfolgen ergeben dieselben Bytes), unabhängig davon, was ein Feld
//! enthält. Serde/JSON wird hier bewusst **nicht** verwendet: die Bindung darf
//! nicht davon abhängen, wie eine Serialisierungsbibliothek Felder anordnet
//! oder Zeichen maskiert.
//!
//! # Herkunft (PL-100 WP 3.0)
//! Bis zur Aufteilung lagen diese Funktionen crate-intern in
//! `harw-dod-warden-proto/src/canonical.rs`. Sie sind hierher gewandert, weil
//! Aktionsbindung und Ziel-Tripel zum signierfreien Vokabular gehören; der
//! Signierteil importiert sie von hier. Die erzeugten Bytes sind unverändert
//! (siehe Tests). Nur signierspezifische Teile bleiben dort: der
//! Domain-Separator der MAC-Eingabe, Wahrheitswerte, die optionale
//! Haltedauer und die Hex-Kodierung von Nonce und MAC.
//!
//! # Zusammengesetzte Felder
//! Werte mit mehreren Teilen (z. B. das Ziel-Tripel) werden zuerst in einen
//! eigenen Puffer aus längenpräfixierten Teilfeldern geschrieben und dieser
//! Puffer dann als **ein** Feld angehängt ([`put_nested`]). Damit bleibt
//! jedes Feld genau ein längenpräfixierter Block, gleich wie viele Teile es
//! hat.
//!
//! # Nebenläufigkeit
//! Reine Funktionen ohne Zustand.
//!
//! # Examples
//! ```rust
//! use harw_dod_warden_types::canonical::{put_field, put_u64};
//!
//! let mut buf = Vec::new();
//! put_field(&mut buf, b"ab");
//! assert_eq!(buf, [0, 0, 0, 0, 0, 0, 0, 2, b'a', b'b']);
//!
//! let mut number = Vec::new();
//! put_u64(&mut number, 1);
//! assert_eq!(number.len(), 8 + 8);
//! ```

/// Domain-Separator der kanonischen Aktionsbindung (siehe
/// [`crate::action::WardenAction::binding_digest`]).
///
/// # Description
/// Fester Wert seit Proof v2; `harw-dod-warden-proto` re-exportiert ihn unter
/// `harw_dod_warden_proto::canonical::ACTION_DOMAIN`.
pub const ACTION_DOMAIN: &[u8] = b"harw:warden-action:v2\0";

/// Appends one field as a `u64` big-endian length followed by the raw bytes.
///
/// # Arguments
/// - `buf` (`&mut Vec<u8>`): der Zielpuffer.
/// - `bytes` (`&[u8]`): der Feldinhalt.
///
/// # Concurrency
/// Reine Funktion.
///
/// # Examples
/// ```rust
/// use harw_dod_warden_types::canonical::put_field;
///
/// let mut buf = Vec::new();
/// put_field(&mut buf, b"x");
/// assert_eq!(buf, [0, 0, 0, 0, 0, 0, 0, 1, b'x']);
/// ```
pub fn put_field(buf: &mut Vec<u8>, bytes: &[u8]) {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(bytes);
}

/// Builds a composite field in its own buffer and appends it as one field.
///
/// # Arguments
/// - `buf` (`&mut Vec<u8>`): der Zielpuffer.
/// - `build` (`impl FnOnce(&mut Vec<u8>)`): schreibt die Teilfelder in einen
///   frischen inneren Puffer.
///
/// # Concurrency
/// Reine Funktion.
///
/// # Examples
/// ```rust
/// use harw_dod_warden_types::canonical::{put_field, put_nested};
///
/// let mut buf = Vec::new();
/// put_nested(&mut buf, |inner| put_field(inner, b"a"));
/// assert_eq!(buf, [0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 1, b'a']);
/// ```
pub fn put_nested(buf: &mut Vec<u8>, build: impl FnOnce(&mut Vec<u8>)) {
    let mut inner = Vec::new();
    build(&mut inner);
    put_field(buf, &inner);
}

/// Appends a `u64` as one field of eight big-endian bytes.
///
/// # Arguments
/// - `buf` (`&mut Vec<u8>`): der Zielpuffer.
/// - `value` (`u64`): der Wert.
///
/// # Concurrency
/// Reine Funktion.
pub fn put_u64(buf: &mut Vec<u8>, value: u64) {
    put_field(buf, &value.to_be_bytes());
}

/// Appends a timestamp as one field: `i64` BE seconds followed by `i32` BE nanoseconds.
///
/// # Arguments
/// - `buf` (`&mut Vec<u8>`): der Zielpuffer.
/// - `value` (`jiff::Timestamp`): der Zeitpunkt (12 Bytes Inhalt).
///
/// # Concurrency
/// Reine Funktion.
pub fn put_timestamp(buf: &mut Vec<u8>, value: jiff::Timestamp) {
    let mut bytes = Vec::with_capacity(12);
    bytes.extend_from_slice(&value.as_second().to_be_bytes());
    bytes.extend_from_slice(&value.subsec_nanosecond().to_be_bytes());
    put_field(buf, &bytes);
}

#[cfg(test)]
mod tests {
    use super::{ACTION_DOMAIN, put_field, put_nested, put_timestamp, put_u64};
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_action_domain_is_unchanged() {
        assert_eq!(ACTION_DOMAIN, b"harw:warden-action:v2\0");
    }

    #[test]
    fn test_put_u64_writes_eight_big_endian_bytes() {
        let mut buf = Vec::new();
        put_u64(&mut buf, 0x0102);
        assert_eq!(buf, [0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 1, 2]);
    }

    #[test]
    fn test_put_timestamp_covers_seconds_and_nanoseconds() -> TestResult {
        let mut whole = Vec::new();
        put_timestamp(
            &mut whole,
            jiff::Timestamp::new(7, 0).map_err(ctx("timestamp"))?,
        );
        let mut fraction = Vec::new();
        put_timestamp(
            &mut fraction,
            jiff::Timestamp::new(7, 1).map_err(ctx("timestamp"))?,
        );
        assert_ne!(whole, fraction);
        assert_eq!(whole.len(), 8 + 12);
        assert_eq!(
            whole,
            [0, 0, 0, 0, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0]
        );
        Ok(())
    }

    #[test]
    fn test_put_nested_wraps_subfields_into_one_field() {
        let mut nested = Vec::new();
        put_nested(&mut nested, |inner| {
            put_field(inner, b"a");
            put_field(inner, b"bc");
        });
        let mut flat = Vec::new();
        put_field(&mut flat, b"a");
        put_field(&mut flat, b"bc");
        // Ein Feld: Längenpräfix der inneren Bytes, gefolgt von genau diesen.
        let mut expected = Vec::new();
        put_field(&mut expected, &flat);
        assert_eq!(nested, expected);
        assert_ne!(nested, flat);
    }

    #[test]
    fn test_put_field_prefixes_big_endian_length() {
        let mut buf = Vec::new();
        put_field(&mut buf, b"ab");
        assert_eq!(buf, [0, 0, 0, 0, 0, 0, 0, 2, b'a', b'b']);
    }

    #[test]
    fn test_put_field_is_unambiguous_across_field_boundaries() {
        let mut left = Vec::new();
        put_field(&mut left, b"ab");
        put_field(&mut left, b"c");
        let mut right = Vec::new();
        put_field(&mut right, b"a");
        put_field(&mut right, b"bc");
        assert_ne!(left, right);
    }
}
