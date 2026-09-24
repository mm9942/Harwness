//! Embedding-Katalog: welches Modell wofür, wo es rechnen darf, und wie ein
//! Text vor dem Einbetten präfixiert wird.
//!
//! # Verantwortungsbereich
//! Diese Crate trennt vier Schichten, die sonst leicht verwechselt werden:
//!
//! 1. [`EmbeddingSpec`] -- was ein Modell *ist*: Name, Dimensionszahl,
//!    Metrik, maximale Eingabelänge. Unveränderlich, aus der eingebetteten
//!    `embeddings.toml`.
//! 2. [`EmbeddingDescriptor`] -- wie man es *benutzt*: Präfix für Dokumente,
//!    Präfix für Abfragen, ob Normalisierung nötig ist. Ebenfalls aus der
//!    TOML.
//! 3. [`RuntimeProfile`] -- was zur Laufzeit *gilt*: welches Backend, welche
//!    [`harw_lens_types::Locality`], welche Stapelgröße.
//! 4. [`ObservedBehavior`] -- was *gemessen* wurde: Trefferqualität auf
//!    einem Goldkorpus, Latenz. Anfangs leer.
//!
//! **Warum die Trennung:** ein Modell, das laut Spezifikation z. B. 768
//! Dimensionen hat, aber im Betrieb über ein Backend läuft, das tatsächlich
//! nur 512 liefert, ist ein Fehler, den man nur sieht, wenn Spezifikation
//! und Laufzeitprofil getrennte Werte sind. Würden sie in einem Struct
//! vermischt, überschriebe eine Zuweisung die andere, und die Abweichung
//! verschwände spurlos. Ebenso trennt [`ObservedBehavior`] eine *Erwartung*
//! (Spec, Deskriptor, Profil) von einem *Messwert*: ein Messwert kann eine
//! Erwartung widerlegen, aber niemals dieselbe Größe sein wie sie.
//!
//! Präfixe verdienen eine eigene Begründung: viele Embedding-Modelle
//! verlangen unterschiedliche Präfixe für Dokument und Abfrage (`"passage: "`
//! gegen `"query: "`). Vertauschte Präfixe verschlechtern die
//! Trefferqualität, ohne einen Fehler zu erzeugen -- deshalb kommen sie
//! ausschließlich aus [`EmbeddingDescriptor`] und werden über
//! [`prepare_document`]/[`prepare_query`] angewendet, nie am Aufrufort von
//! Hand geschrieben.
//!
//! Der Rollen-Router [`route`] wählt für eine [`EmbeddingRole`] das
//! zuständige [`RuntimeProfile`]. Für [`EmbeddingRole::Confidential`]
//! filtert er **vor** der Auswahl auf [`harw_lens_types::Locality::Local`]
//! und schlägt fehl (statt auf ein entferntes Profil auszuweichen), wenn
//! kein lokales Modell registriert ist -- Fail-Closed, kein Ausweichen.
//!
//! [`Embedder`] ist ein reines Trait ohne ML-Abhängigkeit;
//! [`DeterministicEmbedder`] erzeugt reproduzierbare Testvektoren aus einem
//! `blake3`-Hash, und [`DimensionCheckedEmbedder`] prüft jeden gelieferten
//! Vektor gegen die behauptete Dimensionszahl. Seit Knoten **AW7-06**
//! ([`http_backend`]) trägt diese Crate außerdem einen produktionsreifen,
//! echten Transport: [`HttpEmbedBackend`] verschickt Texte tatsächlich über
//! HTTP -- siehe dessen Moduldokumentation für die Entscheidung (entfernt
//! über HTTP statt eines lokalen `candle`/`ort`-Modells) und dafür, was diese
//! Entscheidung nicht kann (`Confidential` bleibt weiterhin ohne
//! produktionsreifen Einbetter).
//!
//! # Die vierte Einbettungsschicht: [`RemoteEmbedder`] (AW7-05) und ihr erster echter Transport (AW7-06)
//! Die vier oben genannten Schichten (`Spec`/`Descriptor`/`Runtime`/
//! `Observed`) beschreiben ein Modell als Daten -- ob es lokal oder entfernt
//! rechnet, ist bei ihnen nur ein Feld ([`harw_lens_types::Locality`]).
//! [`RemoteEmbedder`] ist die einzige Stelle dieser Crate, die tatsächlich
//! *Code* ist, der den Host verlässt, wenn er läuft -- und deshalb anders
//! behandelt wird als jede der vier Schichten: sein
//! [`Embedder::locality`] liefert hart `Locality::Remote`, nicht
//! konfigurierbar, nicht vom Backend ableitbar. Siehe [`remote`]s
//! Moduldokumentation für die volle Begründung ("unwiderruflich, sobald
//! abgeschickt"). [`RemoteEmbedBackend`] ist die Transportabstraktion, über
//! die [`http_backend::HttpEmbedBackend`] seit AW7-06 die erste echte
//! Anbindung einhängt -- eigenständig in dieser Crate, **nicht** über
//! `harw-provider`/`harw-provider-http`, weil jene Crate zum Zeitpunkt von
//! AW7-06 keinen Einbettungs-Aufruf anbietet (siehe [`http_backend`]s
//! Moduldokumentation, Abschnitt „Warum diese Crate nicht
//! `harw-provider-http` selbst aufruft").
//!
//! - [`spec`] — [`EmbeddingSpec`].
//! - [`descriptor`]: [`EmbeddingDescriptor`], [`prepare_document`],
//!   [`prepare_query`].
//! - [`runtime`] — [`RuntimeProfile`].
//! - [`observed`] — [`ObservedBehavior`].
//! - [`catalog`]: [`EmbeddingCatalog`], [`ModelEntry`], [`EmbeddingRole`],
//!   [`route`].
//! - [`embedder`]: [`Embedder`], [`DeterministicEmbedder`],
//!   [`DimensionCheckedEmbedder`].
//! - [`remote`]: [`RemoteEmbedder`], [`RemoteEmbedBackend`] -- die vierte
//!   Einbettungsschicht.
//! - [`http_backend`]: [`HttpEmbedBackend`] -- ihr erster echter Transport.
//! - [`error`]: der eine Fehlertyp dieser Crate.
//!
//! `#![forbid(unsafe_code)]` kommt bereits aus `[workspace.lints]`
//! (`docs/aw-contract-master.md`) und wird hier nicht erneut gesetzt.
//!
//! # Nebenläufigkeit
//! Alle Datentypen sind reine Daten ohne interne Veränderlichkeit.
//! [`Embedder`] ist `Send + Sync` und wird über `&dyn Embedder` aus mehreren
//! Threads aufgerufen.
//!
//! # Fehler
//! [`EmbedError`] (Typalias [`EmbedResult`]) ist der einzige Fehlertyp
//! dieser Crate.
//!
//! # Examples
//! ```rust
//! use harw_lens_embed::{
//!     prepare_document, route, DeterministicEmbedder, Embedder, EmbeddingCatalog, EmbeddingRole,
//! };
//!
//! let catalog = EmbeddingCatalog::load_default().expect("embeddings.toml parses");
//! let entry = catalog
//!     .entry_for_role(EmbeddingRole::Confidential)
//!     .expect("a local model handles confidential content");
//! let text = prepare_document(&entry.descriptor, "geheimer Quelltext");
//!
//! let embedder = DeterministicEmbedder::new(entry.spec.dimensions);
//! let vectors = embedder
//!     .embed(&[text])
//!     .expect("deterministic embedder never fails");
//! assert_eq!(vectors[0].len(), entry.spec.dimensions);
//!
//! let profile = route(&catalog, EmbeddingRole::Confidential).expect("routes to local profile");
//! assert_eq!(profile.locality, harw_lens_types::Locality::Local);
//! ```
//!
//! # Stand
//! Gerüst aus Knoten AW0-00 (Workspace-Fundament). Der Inhalt entstand in
//! Knoten **AW4-07**; Ebene **L3** im Zielgraphen. Die vierte
//! Einbettungsschicht ([`remote`]) kam in Knoten **AW7-05** hinzu, zusammen
//! mit dem Nullzähler `lens_remote_embed_on_operator_only` in
//! `harw-lens-source` (dessen Guard dieses Crate über
//! [`Embedder::locality`] befragt). Knoten **AW7-06** füllte
//! [`RemoteEmbedBackend`] mit dem ersten echten Transport
//! ([`http_backend::HttpEmbedBackend`]) und aktualisierte die eingebettete
//! `embeddings.toml`, damit ihr entferntes Modell einen echten Modellnamen
//! (`text-embedding-3-large`) statt eines generischen Platzhalters trägt --
//! siehe den Abschlussbericht dieses Knotens dafür, warum
//! `harw-tool-lens/src/provenance.rs` trotzdem weiterhin
//! [`DeterministicEmbedder`] verwendet (die kuratierte Fassade `harw-lens`
//! reicht [`HttpEmbedBackend`]/[`RemoteEmbedder`] noch nicht durch).

pub mod catalog;
pub mod descriptor;
pub mod embedder;
pub mod error;
pub mod http_backend;
pub mod observed;
pub mod remote;
pub mod runtime;
pub mod spec;

pub use catalog::{EmbeddingCatalog, EmbeddingRole, ModelEntry, route};
pub use descriptor::{EmbeddingDescriptor, prepare_document, prepare_query};
pub use embedder::{DeterministicEmbedder, DimensionCheckedEmbedder, Embedder};
pub use error::{EmbedError, EmbedResult};
pub use http_backend::HttpEmbedBackend;
pub use observed::ObservedBehavior;
pub use remote::{RemoteEmbedBackend, RemoteEmbedder};
pub use runtime::RuntimeProfile;
pub use spec::EmbeddingSpec;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
