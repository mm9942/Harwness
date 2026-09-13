//! `DetailMode::References`: ein Verweis auf Inhalt statt des Inhalts selbst.
//!
//! # Verantwortungsbereich
//! Besitzt [`FragmentReference`] und [`DigestStatus`] — das Vokabular für
//! „ein Fragment ist da, aber nicht mitgeschleppt worden". Diese Datei
//! entscheidet **nicht**, welche Fragmente tatsächlich zu einem Verweis
//! werden (das bleibt die Montage in `harw-core`, Knoten AW1-03/AW5-07);
//! sie definiert nur, *wie ein Verweis aussieht*, wenn die Montage sich dafür
//! entscheidet.
//!
//! # Warum es die Kappe überhaupt braucht (Knoten AW5-07)
//! Ein Kontextbudget zwingt heute zu einer Alles-oder-nichts-Entscheidung:
//! ein Fragment kommt vollständig hinein oder gar nicht. Bei knappem Budget
//! fallen dann die längsten Fragmente heraus — und die längsten sind oft die
//! wichtigsten. Ein Verweis kostet spürbar weniger als der volle Inhalt und
//! lässt die Nachlade-Entscheidung beim Agenten, der die Frage kennt.
//!
//! Der Preis dafür ist der eigentliche Gegenstand dieses Knotens: ein
//! nachladbarer Verweis ist ein **Weg, mehr in den Kontext zu holen, als das
//! Budget ursprünglich vorsah**. Diese Datei liefert dafür ausschließlich das
//! Vokabular (den Verweis selbst und die Digest-Prüfung); die Kappe, die den
//! Umgehungsweg schließt, lebt bei `context.load` in `harw-tools`
//! (`harw_tools::context_load`), weil nur dort — am tatsächlichen
//! Nachladepunkt — durchgesetzt werden kann, wie viel ein Turn insgesamt
//! nachladen darf.
//!
//! # Wie ein Verweis aussieht, und warum genau so
//! Ein Verweis, der nur eine Kennung trägt (Sektion + Label), ist wertlos:
//! der Agent kann nicht beurteilen, ob sich das Nachladen lohnt, und lädt
//! entweder blind alles oder gar nichts. Ein Verweis, der eine
//! Zusammenfassung des Inhalts trägt, ist dagegen kein Verweis mehr — er hat
//! exakt das Kostenproblem, das er lösen sollte, nur eine Stufe kleiner.
//!
//! Die Mitte, für die sich dieser Knoten entscheidet: **ausschließlich
//! Felder, die [`crate::fragment::Fragment`] bereits trägt**, unverändert
//! übernommen, nie neu erfunden oder aus dem Rumpf abgeleitet:
//! - [`FragmentReference::label`] + [`FragmentReference::section`]: der
//!   Nachlade-Schlüssel — ohne ihn gibt es nichts, das `context.load`
//!   überhaupt anfordern könnte.
//! - [`FragmentReference::trust`]: der Agent (und jede spätere
//!   Deckenprüfung) muss wissen, in welchen Block ein nachgeladenes Fragment
//!   überhaupt gehören dürfte, *bevor* es nachgeladen wird.
//! - [`FragmentReference::stability`]: ein Signal für die Verfallsfrist des
//!   Inhalts — `Volatile` legt nahe, dass sich ein Nachladen schnell wieder
//!   erübrigt, `Pinned`/`Stable` das Gegenteil.
//! - [`FragmentReference::origin`]: wer den Inhalt wann geliefert hat — oft
//!   genug, um Relevanz einzuschätzen, ohne den Inhalt gelesen zu haben
//!   („das ist von vor drei Turns von einem Lint-Tool" braucht keinen
//!   Fließtext).
//! - [`FragmentReference::full_cost`]: das Preisschild. Das ist die
//!   Information, die die Alles-oder-nichts-Entscheidung überhaupt erst in
//!   eine informierte Abwägung verwandelt — genau das, was ein Verweis ohne
//!   dieses Feld nicht leisten könnte.
//! - [`FragmentReference::digest`]: siehe unten.
//!
//! Bewusst **nicht** enthalten: der Rumpf (`body`), auch nicht gekürzt. Jede
//! Kürzung des Rumpfes ist eine Zusammenfassung mit anderem Namen und trägt
//! deren Kostenproblem (Informationsverlust ohne Kontrolle des Agenten
//! darüber, welche Information verloren geht) unverändert weiter.
//!
//! # Fixkosten eines Verweises — und wann er sich nicht lohnt
//! [`FragmentReference`]s `Display`-Darstellung ist bewusst knapp gehalten,
//! aber nicht kostenlos: Label und Digest allein tragen bereits eine
//! UUID (36 Zeichen) und einen 64-Zeichen-Hex-Digest — Fixkosten, die jeder
//! Verweis trägt, unabhängig von der Länge des Inhalts, den er ersetzt. Für
//! ein sehr kurzes Fragment (eine Zwei-Wort-Nachricht) kann ein Verweis
//! deshalb **teurer** sein als der Inhalt selbst. Diese Datei behauptet
//! nirgends, ein Verweis sei immer billiger — das ist eine Eigenschaft des
//! *Aufrufers* (`harw-core::history_tail::render_history_tail` vergleicht
//! beide Kosten und verkürzt nur, wenn der Verweis tatsächlich billiger ist),
//! nicht dieses Typs. Siehe dessen Moduldoku für die Umsetzung.
//!
//! # `ContentDigest` und Veralterung
//! Ein Verweis entsteht zum Renderzeitpunkt. Zwischen Rendern und
//! tatsächlichem Nachladen kann sich der referenzierte Inhalt geändert
//! haben — eine neue Fassung eines Dateiinhalts, ein aktualisiertes
//! Werkzeugergebnis. [`FragmentReference::digest`] hält fest, welchen Inhalt
//! der Verweis *versprochen* hat ([`harw_types::ContentDigest`], derselbe
//! Typ wie [`crate::fragment::Fragment::digest`] — kein zweiter,
//! verwechselbarer 32-Byte-Digest). [`FragmentReference::check_digest`]
//! vergleicht diesen versprochenen Digest gegen den beim Nachladen tatsächlich
//! vorgefundenen und macht eine Abweichung als [`DigestStatus::Changed`]
//! sichtbar, statt sie zu verschweigen — sonst arbeitet der Agent mit etwas
//! anderem, als der Verweis ihm zugesagt hat.
//!
//! # Exportierte Typen
//! [`FragmentReference`], [`DigestStatus`].
//!
//! # Nebenläufigkeit
//! Reine, unveränderliche Werttypen: `Send + Sync`, kein `Rc`/`RefCell`.
//!
//! # Fehler
//! Keine — [`FragmentReference::from_fragment`] ist eine totale Projektion
//! (jedes Feld existiert bereits auf [`crate::fragment::Fragment`]);
//! [`FragmentReference::check_digest`] ist ein reiner Vergleich.
//!
//! # Examples
//! ```rust
//! use harw_context::{
//!     Fragment, FragmentLabel, FragmentOrigin, FragmentReference, SectionName, Stability,
//!     TrustClass,
//! };
//! use harw_context::reference::DigestStatus;
//! use harw_lens_types::CostEstimate;
//! use harw_types::ContentDigest;
//!
//! let fragment = Fragment {
//!     label: FragmentLabel::try_new("turn-7").unwrap(),
//!     section: SectionName::try_new("history.tail").unwrap(),
//!     trust: TrustClass::Evidence,
//!     stability: Stability::Stable,
//!     origin: FragmentOrigin {
//!         provider: "harw-core::history".to_owned(),
//!         namespace: "default".to_owned(),
//!         produced_at: jiff::Timestamp::UNIX_EPOCH,
//!     },
//!     cost: CostEstimate(2_000),
//!     digest: ContentDigest::of(b"a very long tool transcript"),
//!     body: "a very long tool transcript".repeat(50),
//! };
//!
//! let reference = FragmentReference::from_fragment(&fragment);
//! assert_eq!(reference.full_cost, fragment.cost);
//! assert_eq!(reference.check_digest(fragment.digest), DigestStatus::Unchanged);
//!
//! let changed = ContentDigest::of(b"different content now");
//! assert!(matches!(reference.check_digest(changed), DigestStatus::Changed { .. }));
//! ```

use crate::fragment::{Fragment, FragmentLabel, FragmentOrigin, SectionName, Stability, TrustClass};

/// Ein Verweis auf ein Fragment, statt des Fragments selbst.
///
/// # Description
/// Wird über [`Self::from_fragment`] aus einem vollständigen
/// [`crate::fragment::Fragment`] projiziert: jedes Feld existiert dort
/// bereits identisch, außer [`crate::fragment::Fragment::body`], das absichtlich
/// **nicht** übernommen wird (siehe Moduldoku, Abschnitt „Wie ein Verweis
/// aussieht"). `full_cost` trägt die Kosten des *vollständigen* Fragments —
/// das Preisschild für die Nachlade-Entscheidung — nicht die (viel
/// kleineren) Kosten des gerenderten Verweistextes selbst; wie teuer der
/// Verweistext ist, entscheidet der Renderer, der ihn erzeugt (`harw-core`),
/// über seinen eigenen `harw_lens_types::CostEstimator`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FragmentReference {
    /// Nachlade-Schlüssel, Teil 1: das Fragment-Label innerhalb der Sektion.
    pub label: FragmentLabel,
    /// Nachlade-Schlüssel, Teil 2: die Sektion des referenzierten Fragments.
    pub section: SectionName,
    /// Die Vertrauensklasse des referenzierten Fragments — unverändert aus
    /// [`crate::fragment::Fragment::trust`]. Bestimmt, in welchen Block ein
    /// nachgeladenes Fragment überhaupt gehören dürfte.
    pub trust: TrustClass,
    /// Die Beständigkeit des referenzierten Fragments — ein Signal dafür,
    /// wie wahrscheinlich sich ein Nachladen bald wieder erübrigt.
    pub stability: Stability,
    /// Wer den referenzierten Inhalt wann geliefert hat.
    pub origin: FragmentOrigin,
    /// Die Kosten des *vollständigen* Fragments — das Preisschild, das die
    /// Nachlade-Entscheidung überhaupt erst informiert trifft.
    pub full_cost: harw_lens_types::CostEstimate,
    /// Der Inhalts-Digest des referenzierten Fragments zum Renderzeitpunkt.
    /// Siehe [`Self::check_digest`] für die Veralterungsprüfung.
    pub digest: harw_types::ContentDigest,
}

impl FragmentReference {
    /// Projiziert ein vollständiges Fragment auf seinen Verweis.
    ///
    /// # Description
    /// Totale, verlustbehaftete Projektion: übernimmt jedes Feld von
    /// `fragment` außer `body` unverändert (`cost` wird zu `full_cost`,
    /// unter neuem Namen, aber mit identischem Wert). Siehe die Moduldoku
    /// für die Begründung, warum genau diese Feldmenge — nicht mehr, nicht
    /// weniger.
    ///
    /// # Arguments
    /// - `fragment` (`&Fragment`): das vollständige Fragment, das ersetzt
    ///   werden soll.
    ///
    /// # Returns
    /// Ein `FragmentReference`, das `fragment` beschreibt, aber dessen
    /// Rumpf nicht trägt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::{Fragment, FragmentLabel, FragmentOrigin, FragmentReference, SectionName, Stability, TrustClass};
    /// use harw_lens_types::CostEstimate;
    /// use harw_types::ContentDigest;
    ///
    /// let fragment = Fragment {
    ///     label: FragmentLabel::try_new("turn-1").unwrap(),
    ///     section: SectionName::try_new("history.tail").unwrap(),
    ///     trust: TrustClass::Evidence,
    ///     stability: Stability::Stable,
    ///     origin: FragmentOrigin {
    ///         provider: "harw-core::history".to_owned(),
    ///         namespace: "default".to_owned(),
    ///         produced_at: jiff::Timestamp::UNIX_EPOCH,
    ///     },
    ///     cost: CostEstimate(4),
    ///     digest: ContentDigest::of(b"hi"),
    ///     body: "hi".to_owned(),
    /// };
    /// let reference = FragmentReference::from_fragment(&fragment);
    /// assert_eq!(reference.label, fragment.label);
    /// assert_eq!(reference.full_cost, fragment.cost);
    /// ```
    #[must_use]
    pub fn from_fragment(fragment: &Fragment) -> Self {
        Self {
            label: fragment.label.clone(),
            section: fragment.section.clone(),
            trust: fragment.trust,
            stability: fragment.stability,
            origin: fragment.origin.clone(),
            full_cost: fragment.cost,
            digest: fragment.digest,
        }
    }

    /// Vergleicht den beim Nachladen vorgefundenen Digest gegen den beim
    /// Rendern versprochenen.
    ///
    /// # Description
    /// Reiner Vergleich, keine Bewertung: eine Abweichung ist weder
    /// automatisch ein Fehler noch automatisch harmlos — das entscheidet der
    /// Aufrufer (typischerweise `context.load`, das den Status im
    /// Werkzeugergebnis sichtbar macht, statt ihn zu verschweigen).
    ///
    /// # Arguments
    /// - `found` (`harw_types::ContentDigest`): der Digest des beim
    ///   Nachladen tatsächlich vorgefundenen Inhalts.
    ///
    /// # Returns
    /// [`DigestStatus::Unchanged`], wenn `found` mit [`Self::digest`]
    /// übereinstimmt; sonst [`DigestStatus::Changed`] mit beiden Werten.
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::reference::DigestStatus;
    /// use harw_context::{Fragment, FragmentLabel, FragmentOrigin, FragmentReference, SectionName, Stability, TrustClass};
    /// use harw_lens_types::CostEstimate;
    /// use harw_types::ContentDigest;
    ///
    /// let fragment = Fragment {
    ///     label: FragmentLabel::try_new("turn-1").unwrap(),
    ///     section: SectionName::try_new("history.tail").unwrap(),
    ///     trust: TrustClass::Evidence,
    ///     stability: Stability::Stable,
    ///     origin: FragmentOrigin {
    ///         provider: "harw-core::history".to_owned(),
    ///         namespace: "default".to_owned(),
    ///         produced_at: jiff::Timestamp::UNIX_EPOCH,
    ///     },
    ///     cost: CostEstimate(4),
    ///     digest: ContentDigest::of(b"hi"),
    ///     body: "hi".to_owned(),
    /// };
    /// let reference = FragmentReference::from_fragment(&fragment);
    /// assert_eq!(reference.check_digest(ContentDigest::of(b"hi")), DigestStatus::Unchanged);
    /// assert!(matches!(
    ///     reference.check_digest(ContentDigest::of(b"changed")),
    ///     DigestStatus::Changed { .. }
    /// ));
    /// ```
    #[must_use]
    pub fn check_digest(&self, found: harw_types::ContentDigest) -> DigestStatus {
        if self.digest == found {
            DigestStatus::Unchanged
        } else {
            DigestStatus::Changed {
                expected: self.digest,
                found,
            }
        }
    }
}

/// Formatiert den Verweis als einzeilige, menschen- und modellenlesbare
/// Beschreibung — das, was tatsächlich anstelle des Rumpfes in einer
/// `DetailMode::References`-Sektion gerendert wird.
///
/// # Description
/// Nennt genau die Felder aus der Moduldoku (Nachlade-Schlüssel, Vertrauen,
/// Beständigkeit, Herkunft, Preisschild, Digest) plus einen Hinweis auf das
/// Werkzeug, mit dem sich der Verweis auflösen lässt. Deterministisch: zwei
/// Aufrufe auf demselben Wert erzeugen byteweise identischen Text — keine
/// Systemuhr, keine Zufallszahl.
impl std::fmt::Display for FragmentReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Bewusst knapp gehalten: jedes zusätzliche Zeichen hier ist
        // Fixkosten, die *jeder* Verweis trägt, unabhängig davon, wie lang
        // der Inhalt ist, den er ersetzt (siehe Moduldoku und
        // `crate::reference::tests::test_reference_render_costs_less_than_full_body_with_same_estimator`).
        // Trotzdem vollständig: jedes in der Moduldoku begründete Feld bleibt
        // vertreten, nur ohne erklärenden Fließtext drumherum.
        write!(
            f,
            "[ref] section=\"{section}\" label=\"{label}\" trust={trust:?} stability={stability:?} \
             origin=\"{provider}\"@{produced_at} cost={full_cost:?} digest={digest} \
             (load via context.load)",
            section = self.section.as_str(),
            label = self.label.as_str(),
            trust = self.trust,
            stability = self.stability,
            provider = self.origin.provider,
            produced_at = self.origin.produced_at,
            full_cost = self.full_cost,
            digest = self.digest,
        )
    }
}

/// Ob der beim Nachladen vorgefundene Inhalt noch dem entspricht, den der
/// Verweis versprochen hat.
///
/// # Description
/// Ergebnis von [`FragmentReference::check_digest`]. Kein Fehlertyp — eine
/// Abweichung ist eine Beobachtung, keine fehlgeschlagene Operation; der
/// Nachladevorgang selbst gelingt in beiden Fällen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestStatus {
    /// Der vorgefundene Digest stimmt mit dem versprochenen überein.
    Unchanged,
    /// Der vorgefundene Digest weicht vom versprochenen ab.
    Changed {
        /// Der beim Rendern des Verweises versprochene Digest.
        expected: harw_types::ContentDigest,
        /// Der beim Nachladen tatsächlich vorgefundene Digest.
        found: harw_types::ContentDigest,
    },
}

#[cfg(test)]
mod tests {
    use super::{DigestStatus, FragmentReference};
    use crate::fragment::{Fragment, FragmentLabel, FragmentOrigin, SectionName, Stability, TrustClass};
    use harw_lens_types::{BytesOverFour, CostEstimator};

    fn origin() -> FragmentOrigin {
        FragmentOrigin {
            provider: "harw-core::history".to_owned(),
            namespace: "default".to_owned(),
            produced_at: jiff::Timestamp::UNIX_EPOCH,
        }
    }

    fn long_fragment() -> Fragment {
        let body = "the quick brown fox jumps over the lazy dog. ".repeat(80);
        Fragment {
            label: FragmentLabel::try_new("turn-7").unwrap(),
            section: SectionName::try_new("history.tail").unwrap(),
            trust: TrustClass::Evidence,
            stability: Stability::Stable,
            origin: origin(),
            cost: BytesOverFour.estimate(&body),
            digest: harw_types::ContentDigest::of(body.as_bytes()),
            body,
        }
    }

    #[test]
    fn test_from_fragment_copies_every_field_except_body() {
        let fragment = long_fragment();
        let reference = FragmentReference::from_fragment(&fragment);

        assert_eq!(reference.label, fragment.label);
        assert_eq!(reference.section, fragment.section);
        assert_eq!(reference.trust, fragment.trust);
        assert_eq!(reference.stability, fragment.stability);
        assert_eq!(reference.origin, fragment.origin);
        assert_eq!(reference.full_cost, fragment.cost);
        assert_eq!(reference.digest, fragment.digest);
    }

    /// Kern-Zusage des Knotens: ein Verweis kostet nachweislich weniger als
    /// das vollständige Fragment — gemessen mit demselben Schätzer, nicht
    /// nur behauptet.
    #[test]
    fn test_reference_render_costs_less_than_full_body_with_same_estimator() {
        let fragment = long_fragment();
        let reference = FragmentReference::from_fragment(&fragment);

        let full_cost = BytesOverFour.estimate(&fragment.body);
        let reference_cost = BytesOverFour.estimate(&reference.to_string());

        assert!(
            reference_cost < full_cost,
            "reference ({reference_cost:?}) must cost less than the full body ({full_cost:?})"
        );
        // Das Preisschild selbst bleibt unverändert der volle Preis — sonst
        // wüsste der Agent nicht, wofür er sich entscheidet.
        assert_eq!(reference.full_cost, full_cost);
    }

    /// Der Verweis genügt zur Beurteilung: alle Felder, die die Moduldoku als
    /// notwendig begründet, tauchen im gerenderten Text auf.
    #[test]
    fn test_display_contains_every_field_needed_to_judge_whether_to_load() {
        let fragment = long_fragment();
        let reference = FragmentReference::from_fragment(&fragment);
        let rendered = reference.to_string();

        assert!(rendered.contains("history.tail"), "section missing: {rendered}");
        assert!(rendered.contains("turn-7"), "label missing: {rendered}");
        assert!(rendered.contains("Evidence"), "trust missing: {rendered}");
        assert!(rendered.contains("Stable"), "stability missing: {rendered}");
        assert!(rendered.contains("harw-core::history"), "origin missing: {rendered}");
        assert!(rendered.contains("context.load"), "load hint missing: {rendered}");
        assert!(
            rendered.contains(&fragment.cost.0.to_string()),
            "cost missing: {rendered}"
        );
        assert!(
            rendered.contains(&fragment.digest.to_string()),
            "digest missing: {rendered}"
        );
    }

    #[test]
    fn test_display_does_not_leak_the_body() {
        let fragment = long_fragment();
        let reference = FragmentReference::from_fragment(&fragment);
        let rendered = reference.to_string();

        assert!(
            !rendered.contains("quick brown fox"),
            "a reference must never leak the body it stands in for"
        );
    }

    #[test]
    fn test_check_digest_reports_unchanged_for_matching_digest() {
        let fragment = long_fragment();
        let reference = FragmentReference::from_fragment(&fragment);

        assert_eq!(reference.check_digest(fragment.digest), DigestStatus::Unchanged);
    }

    /// Ein Verweis auf geänderten Inhalt wird erkannt.
    #[test]
    fn test_check_digest_reports_changed_for_different_digest() {
        let fragment = long_fragment();
        let reference = FragmentReference::from_fragment(&fragment);
        let changed = harw_types::ContentDigest::of(b"a completely different body");

        match reference.check_digest(changed) {
            DigestStatus::Changed { expected, found } => {
                assert_eq!(expected, fragment.digest);
                assert_eq!(found, changed);
            }
            DigestStatus::Unchanged => panic!("expected a Changed status for a different digest"),
        }
    }

    /// Determinismus: zweimal aus demselben Fragment projiziert, byteweise
    /// identisches Ergebnis — keine Systemuhr, keine Zufallszahl.
    #[test]
    fn test_from_fragment_is_deterministic() {
        let fragment = long_fragment();
        let a = FragmentReference::from_fragment(&fragment);
        let b = FragmentReference::from_fragment(&fragment);

        assert_eq!(a, b);
        assert_eq!(a.to_string(), b.to_string());
    }

    #[test]
    fn test_fragment_reference_serde_roundtrip() {
        let fragment = long_fragment();
        let reference = FragmentReference::from_fragment(&fragment);

        let json = serde_json::to_string(&reference).expect("reference must serialize");
        let restored: FragmentReference =
            serde_json::from_str(&json).expect("reference must deserialize");
        assert_eq!(reference, restored);
    }

    #[test]
    fn test_fragment_reference_deserialize_rejects_unknown_field() {
        let fragment = long_fragment();
        let reference = FragmentReference::from_fragment(&fragment);
        let mut value =
            serde_json::to_value(&reference).expect("reference must serialize to value");
        value
            .as_object_mut()
            .expect("reference serializes to an object")
            .insert("unexpected".to_owned(), serde_json::json!(true));

        let result: Result<FragmentReference, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }
}
