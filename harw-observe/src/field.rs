//! Validierte Feldnamen und geschlossene Feldwerte für Metrik-Labels und
//! Span-Felder.
//!
//! # Verantwortungsbereich
//! Trägt [`FieldName`], [`FieldValue`] und das Makro [`field!`] (Vertrag
//! A.1, `docs/aw-contract-master.md`). `FieldName` ist ausschließlich über
//! [`field!`] konstruierbar: die Prüfung (nicht leer, keine Steuerzeichen)
//! läuft in einer `const`-Deklaration zur Compile-Zeit, nicht als
//! Laufzeit-`Result`.
//!
//! # Nebenläufigkeit
//! `FieldName` ist `Copy` (eine `&'static str`-Referenz); `FieldValue` ist
//! `Clone`. Beide ohne Interior Mutability, beliebig zwischen Threads
//! teilbar.
//!
//! # Fehler
//! Keine zur Laufzeit. Ein ungültiger [`field!`]-Aufruf ist ein
//! Compilefehler, kein `Result`.
//!
//! # Examples
//! ```
//! use harw_observe::FieldValue;
//!
//! let name = harw_observe::field!("queue_depth");
//! let value = FieldValue::U64(3);
//! assert_eq!(name.as_str(), "queue_depth");
//! assert_eq!(value, FieldValue::U64(3));
//! ```

/// Ein validierter Feldname für Metrik-Labels und Span-Felder.
///
/// Nur über [`field!`] konstruierbar: der Makro-Weg prüft zur Compile-Zeit,
/// was `try_new` erst zur Laufzeit prüfen könnte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldName(&'static str);

impl FieldName {
    /// Baut einen Feldnamen aus einem statischen String.
    ///
    /// # Description
    /// Nicht als gepflegte API vorgesehen: der einzige unterstützte
    /// Konstruktionsweg ist [`field!`], das diese Funktion erst nach einer
    /// Compile-Zeit-Prüfung über [`assert_valid_field_name`] aufruft.
    /// Bleibt `pub`, weil das vom Makro erzeugte Aufrufer-Crate sie
    /// erreichen muss; `#[doc(hidden)]` markiert sie als nicht Teil der
    /// gepflegten Oberfläche.
    ///
    /// # Arguments
    /// - `name` (`&'static str`): der ungeprüft übernommene Name.
    ///
    /// # Returns
    /// Den `FieldName`-Wert, der `name` unverändert trägt.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_static_unchecked(name: &'static str) -> Self {
        Self(name)
    }

    /// Der Name als Zeichenkette.
    ///
    /// # Returns
    /// Die zugrunde liegende `&'static str`.
    ///
    /// # Examples
    /// ```
    /// let name = harw_observe::field!("latency");
    /// assert_eq!(name.as_str(), "latency");
    /// ```
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for FieldName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Prüft einen Feldnamen zur Compile-Zeit; nicht Teil der gepflegten API.
///
/// # Description
/// Wird ausschließlich aus dem Makro [`field!`] heraus in einer
/// `const`-Bindung ausgewertet. Ein `assert!`, das in einem
/// `const`-Kontext fehlschlägt, wird zu einem Compilefehler — daher kein
/// `Result`- oder `bool`-Rückgabewert, der ignoriert werden könnte.
///
/// # Arguments
/// - `name` (`&str`): der zu prüfende Feldname.
///
/// # Panics
/// Wenn `name` leer ist oder ein Steuerzeichen (Byte `< 0x20` oder `0x7F`)
/// enthält. In einem `const`-Kontext (dem einzigen vorgesehenen
/// Aufrufort) bricht das den Build; zur Laufzeit aufgerufen panikt es wie
/// jede andere `assert!`-Prüfung.
#[doc(hidden)]
pub const fn assert_valid_field_name(name: &str) {
    let bytes = name.as_bytes();
    assert!(
        !bytes.is_empty(),
        "harw-observe: field name must not be empty"
    );
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        assert!(
            !(b < 0x20 || b == 0x7F),
            "harw-observe: field name must not contain control characters"
        );
        i += 1;
    }
}

/// Baut einen [`FieldName`] mit Compile-Zeit-Prüfung.
///
/// # Description
/// Einziger vorgesehener Konstruktionsweg (Vertrag A.1). Erzeugt eine
/// `const`-Bindung, die [`assert_valid_field_name`] auswertet, bevor der
/// eigentliche Wert über [`FieldName::from_static_unchecked`] entsteht. Ein
/// leerer oder ungültiger Name bricht damit den Build, statt zur Laufzeit
/// eine Panik oder ein `Result` zu erzeugen.
///
/// # Examples
/// ```
/// let name = harw_observe::field!("http_status");
/// assert_eq!(name.as_str(), "http_status");
/// ```
#[macro_export]
macro_rules! field {
    ($name:expr) => {{
        const _: () = $crate::field::assert_valid_field_name($name);
        $crate::field::FieldName::from_static_unchecked($name)
    }};
}

/// Ein Feldwert. Bewusst klein gehalten: Telemetrie trägt Zahlen und
/// geschlossene Aufzählungen, keine Inhalte.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// Statischer String, z. B. ein Label-Wert aus einer geschlossenen
    /// Menge.
    Str(&'static str),
    /// Zur Laufzeit gebauter String.
    Owned(String),
    /// Vorzeichenbehaftete Ganzzahl.
    I64(i64),
    /// Vorzeichenlose Ganzzahl.
    U64(u64),
    /// Gleitkommazahl.
    F64(f64),
    /// Bool'scher Wert.
    Bool(bool),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_field_macro_builds_valid_name() {
        let name = field!("cpu_percent");
        assert_eq!(name.as_str(), "cpu_percent");
    }

    #[test]
    fn test_field_name_display_writes_inner_str() {
        let name = field!("mem_bytes");
        assert_eq!(name.to_string(), "mem_bytes");
    }

    #[test]
    fn test_field_name_from_static_unchecked_roundtrip() {
        let name = FieldName::from_static_unchecked("raw");
        assert_eq!(name.as_str(), "raw");
    }

    #[test]
    fn test_field_name_ordering_is_lexicographic() {
        let a = field!("a_field");
        let b = field!("b_field");
        assert!(a < b);
    }

    #[test]
    fn test_field_name_hash_and_eq_consistent_for_equal_values() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(field!("dup"));
        set.insert(field!("dup"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn test_field_value_equality() {
        assert_eq!(FieldValue::I64(-1), FieldValue::I64(-1));
        assert_ne!(FieldValue::I64(1), FieldValue::U64(1));
    }

    #[test]
    fn test_field_value_clone_produces_equal_value() {
        let original = FieldValue::Owned("x".to_owned());
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn test_assert_valid_field_name_accepts_normal_text() {
        // Läuft hier zusätzlich zur Laufzeit; die eigentliche Prüfung für
        // `field!`-Aufrufe passiert bereits zur Compile-Zeit oben.
        assert_valid_field_name("ok_name");
    }

    #[test]
    #[should_panic(expected = "must not be empty")]
    fn test_assert_valid_field_name_rejects_empty() {
        assert_valid_field_name("");
    }

    #[test]
    #[should_panic(expected = "control characters")]
    fn test_assert_valid_field_name_rejects_control_character() {
        assert_valid_field_name("bad\nname");
    }
}
