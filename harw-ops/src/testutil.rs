//! Wiederverwendbare Test-Hilfsfunktionen für ops-Tests.
//!
//! Nur unter `#[cfg(test)]` sichtbar — nicht Teil der öffentlichen API.

/// Wandelt einen `&[&str]` in `Vec<String>` für Test-Fixtures.
///
/// # Description
/// Kompakter Helfer für Tests, die `Vec<String>`-Argumente an
/// `FromRawArgs::from_raw_args` und ähnliche Funktionen übergeben.
///
/// # Arguments
/// - `tokens` (`&[&str]`): String-Slice-Liste, jeweils per `to_owned` kopiert.
///
/// # Returns
/// Neue `Vec<String>` mit besitzenden Kopien.
///
/// # Examples
/// ```ignore
/// use crate::testutil::toks;
/// let v = toks(&["a", "b"]);
/// assert_eq!(v, vec!["a".to_owned(), "b".to_owned()]);
/// ```
#[cfg(test)]
pub(crate) fn toks(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|s| (*s).to_owned()).collect()
}
