//! Gemeinsamer Argument-Parser der Wissens-Ops (`/workbench`, `/kanban`,
//! `/diary`, `/palace`, `/dream`).
//!
//! # Beschreibung
//! Ersetzt die fünf Kopien „rohe Tokens + `split_flag`“. Eine Op beschreibt
//! ihre Flags als [`FlagSpec`]-Liste; [`KnowledgeArgs::parse`] trennt sie von
//! den Positionalen. Erkannt werden:
//! - `--name=wert` und `--name wert` für Wert-Flags ([`FlagKind::Value`],
//!   erste Angabe gilt) und wiederholbare Flags ([`FlagKind::Repeated`]),
//! - `--name` für Schalter ([`FlagKind::Switch`]).
//!
//! Nicht deklarierte `--…`-Tokens bleiben Positionale (etwa in Notiztext),
//! genau wie früher bei `split_flag`. Flags dürfen überall stehen, auch vor
//! dem Subcommand.
//!
//! # Gestaffeltes Parsen
//! Subcommand-spezifische Flags parst die Op auf [`KnowledgeArgs::rest`]
//! erneut mit eigener Spezifikation. So bleibt z. B. `--all` in einem
//! `/kanban create`-Titel Text, wie vor der Umstellung.
//!
//! # Fehler
//! [`OpError::InvalidArguments`] für ein Wert-Flag ohne Wert (`--scope` als
//! letztes Token) oder einen Schalter mit Wert (`--all=ja`).

use harw_operations::OpError;

/// Art eines deklarierten Flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagKind {
    /// Einmaliger Wert; bei Wiederholung gilt die erste Angabe.
    Value,
    /// Wiederholbarer Wert (alle Angaben in Reihenfolge).
    Repeated,
    /// Schalter ohne Wert.
    Switch,
}

/// Ein deklariertes Flag: Name ohne führende `--` plus Art.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlagSpec {
    /// Name ohne `--`, z. B. `scope`.
    pub name: &'static str,
    /// Art des Flags.
    pub kind: FlagKind,
}

impl FlagSpec {
    /// Wert-Flag `--name=<wert>` bzw. `--name <wert>`.
    #[must_use]
    pub const fn value(name: &'static str) -> Self {
        Self {
            name,
            kind: FlagKind::Value,
        }
    }

    /// Wiederholbares Wert-Flag.
    #[must_use]
    pub const fn repeated(name: &'static str) -> Self {
        Self {
            name,
            kind: FlagKind::Repeated,
        }
    }

    /// Schalter `--name`.
    #[must_use]
    pub const fn switch(name: &'static str) -> Self {
        Self {
            name,
            kind: FlagKind::Switch,
        }
    }
}

/// Ergebnis von [`KnowledgeArgs::parse`]: Positionale plus Flag-Werte.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnowledgeArgs {
    positionals: Vec<String>,
    values: Vec<(&'static str, String)>,
    switches: Vec<&'static str>,
}

impl KnowledgeArgs {
    /// Zerlegt `tokens` gemäß `spec`.
    ///
    /// # Fehler
    /// [`OpError::InvalidArguments`] für ein Wert-Flag ohne Wert oder einen
    /// Schalter mit `=wert`.
    pub fn parse(tokens: &[String], spec: &[FlagSpec]) -> Result<Self, OpError> {
        let mut parsed = Self::default();
        let mut iter = tokens.iter();
        while let Some(token) = iter.next() {
            let Some(body) = token.strip_prefix("--") else {
                parsed.positionals.push(token.clone());
                continue;
            };
            let (name, inline) = match body.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (body, None),
            };
            let Some(flag) = spec.iter().find(|flag| flag.name == name) else {
                parsed.positionals.push(token.clone());
                continue;
            };
            match (flag.kind, inline) {
                (FlagKind::Switch, None) => {
                    if !parsed.switches.contains(&flag.name) {
                        parsed.switches.push(flag.name);
                    }
                }
                (FlagKind::Switch, Some(_)) => {
                    return Err(OpError::InvalidArguments(format!(
                        "--{name} ist ein Schalter und nimmt keinen Wert"
                    )));
                }
                (FlagKind::Value | FlagKind::Repeated, Some(value)) => {
                    parsed.values.push((flag.name, value.to_owned()));
                }
                (FlagKind::Value | FlagKind::Repeated, None) => {
                    let value = iter.next().ok_or_else(|| {
                        OpError::InvalidArguments(format!("--{name} erwartet einen Wert"))
                    })?;
                    parsed.values.push((flag.name, value.clone()));
                }
            }
        }
        Ok(parsed)
    }

    /// Alle Positionale in Eingabereihenfolge.
    #[must_use]
    pub fn positionals(&self) -> &[String] {
        &self.positionals
    }

    /// Das erste Positional (Subcommand), falls vorhanden.
    #[must_use]
    pub fn subcommand(&self) -> Option<&str> {
        self.positionals.first().map(String::as_str)
    }

    /// Die Positionale nach dem Subcommand.
    #[must_use]
    pub fn rest(&self) -> &[String] {
        self.positionals.get(1..).unwrap_or_default()
    }

    /// [`Self::rest`] mit Leerzeichen verbunden (Freitext).
    #[must_use]
    pub fn rest_text(&self) -> String {
        self.rest().join(" ")
    }

    /// Erster Wert eines Wert-Flags.
    #[must_use]
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(flag, _)| *flag == name)
            .map(|(_, value)| value.as_str())
    }

    /// Alle Werte eines (wiederholbaren) Flags in Reihenfolge.
    #[must_use]
    pub fn values(&self, name: &str) -> Vec<String> {
        self.values
            .iter()
            .filter(|(flag, _)| *flag == name)
            .map(|(_, value)| value.clone())
            .collect()
    }

    /// `true`, wenn der Schalter gesetzt ist.
    #[must_use]
    pub fn switch(&self, name: &str) -> bool {
        self.switches.contains(&name)
    }
}

#[cfg(test)]
mod tests {
    use super::{FlagSpec, KnowledgeArgs};
    use crate::test_support::{TestResult, ctx};
    use crate::testutil::toks;
    use harw_operations::OpError;

    const SPEC: &[FlagSpec] = &[
        FlagSpec::value("scope"),
        FlagSpec::repeated("tag"),
        FlagSpec::switch("all"),
    ];

    #[test]
    fn flags_are_found_anywhere_and_positionals_keep_their_order() -> TestResult {
        let args = KnowledgeArgs::parse(
            &toks(&[
                "--scope=project:x",
                "note",
                "a",
                "--tag",
                "t1",
                "b",
                "--tag=t2",
                "--all",
            ]),
            SPEC,
        )
        .map_err(ctx("parse"))?;
        assert_eq!(args.subcommand(), Some("note"));
        assert_eq!(args.rest(), toks(&["a", "b"]).as_slice());
        assert_eq!(args.rest_text(), "a b");
        assert_eq!(args.value("scope"), Some("project:x"));
        assert_eq!(args.values("tag"), toks(&["t1", "t2"]));
        assert!(args.switch("all"));
        Ok(())
    }

    #[test]
    fn the_first_value_wins_and_unknown_flags_stay_positional() -> TestResult {
        let args = KnowledgeArgs::parse(
            &toks(&["a", "--scope=1", "--scope", "2", "--other=x", "--"]),
            SPEC,
        )
        .map_err(ctx("parse"))?;
        assert_eq!(args.value("scope"), Some("1"));
        assert_eq!(
            args.positionals(),
            toks(&["a", "--other=x", "--"]).as_slice()
        );
        assert!(!args.switch("all"));
        assert_eq!(args.value("missing"), None);
        Ok(())
    }

    #[test]
    fn empty_input_has_no_subcommand() -> TestResult {
        let args = KnowledgeArgs::parse(&[], SPEC).map_err(ctx("parse"))?;
        assert_eq!(args.subcommand(), None);
        assert!(args.rest().is_empty());
        Ok(())
    }

    #[test]
    fn malformed_flags_are_invalid_arguments() {
        for tokens in [vec!["--scope"], vec!["x", "--all=yes"]] {
            assert!(
                matches!(
                    KnowledgeArgs::parse(&toks(&tokens), SPEC),
                    Err(OpError::InvalidArguments(_))
                ),
                "{tokens:?}"
            );
        }
    }
}
