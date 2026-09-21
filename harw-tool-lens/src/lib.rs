//! `harw-tool-lens` — Lens als Werkzeug für Agenten (Knoten AW6-10).
//!
//! # Verantwortungsbereich
//! Ohne diese Crate wird Lens über elf Crates gebaut (`harw-lens-types`,
//! `-rank`, `-chunk`, `-store`, `-index`, `-embed`, `-source`, `-query`,
//! `-federation`, `harw-lens` selbst und diese Crate) und erreicht **nie**
//! ein Modell -- kein Agent kann irgendeine dieser zehn inneren Crates
//! aufrufen, weil keine von ihnen Teil der Werkzeugoberfläche ist, die einem
//! Agenten präsentiert wird. Diese Crate besitzt genau einen Namen dafür:
//! [`LensToolProvider`] mit dem einen Werkzeug `lens.ask`.
//!
//! # Woher der `ReadScope` kommt -- und warum nicht vom Aufrufer
//! **Nicht vom Aufrufer.** [`crate::ask_tool::LensAskArgs`] trägt kein
//! `scope`-/`visibility`-Feld; ein solches Feld würde einem Agenten erlauben,
//! seine eigene Berechtigung zu wählen, statt sie zu erhalten. Stattdessen
//! leitet [`crate::scope::derive_read_scope`] den Lesebereich aus dem
//! vertrauten [`harw_tools::ToolExecutionContext`] ab -- demselben Muster,
//! das `harw-tools/src/sandbox_guard.rs` für jede andere Grenze verwendet.
//!
//! **Der Befund, den dieser Knoten mitbringt:** zum Zeitpunkt dieses Knotens
//! kennt [`harw_authority::SandboxSpec`] keinen Operator-Begriff -- es gibt
//! keine Berechtigung und kein Feld, aus dem sich ableiten ließe, ob ein
//! Aufrufer `"operator-only"`-Sichtbarkeit sehen darf. [`derive_read_scope`]
//! liefert deshalb **immer** genau [`harw_lens::DEFAULT_VISIBILITY`] --
//! fail-closed, aber ausdrücklich eine Näherung, keine vollständige
//! Autorisierungsentscheidung. Siehe `scope.rs`s `//!`-Block für den
//! vollständigen Befund und einen Vorschlag (eine
//! `Permission::ReadOperatorOnlyLensIndex`-Variante, analog zu
//! `Permission::ReadCargoRegistry`).
//!
//! # Woher die Provenienz kommt
//! [`harw_lens_query::QueryProvenance`] darf laut K43 nicht aus dem
//! durchsuchten Index abgeleitet werden (das machte die Kompatibilitätsprüfung
//! tautologisch) und ist dem Aufrufer (dem Modell) unbekannt -- es übergibt
//! nur eine Textfrage. Diese Crate leitet sie deshalb aus **dem Einbetter, den
//! sie selbst benutzt**, ab: [`crate::provenance::ask_provenance`] baut die
//! Provenienz aus dem tatsächlich gewählten Modellnamen plus
//! [`harw_lens::CHUNKER_VERSION`]. Seit `harw-lens` um
//! [`harw_lens::RemoteEmbedder`]/[`harw_lens::HttpEmbedBackend`] erweitert
//! wurde, wählt [`crate::provenance::ask_embedder`] zwischen zwei Pfaden:
//! ohne konfigurierten Endpunkt unverändert
//! [`harw_lens::DeterministicEmbedder`] (Modellname
//! [`crate::provenance::ASK_EMBEDDING_MODEL`],
//! `"deterministic-placeholder-32"`, trägt den Platzhalter-Charakter
//! ausdrücklich im Namen); mit konfiguriertem Endpunkt ein über
//! [`harw_lens::HttpEmbedBackend`] angebundenes entferntes Modell, dessen
//! echter Katalogname (z. B. `"text-embedding-3-large"`) dann in die
//! Provenienz eingetragen wird. [`crate::provenance::ask_provenance`]
//! wiederholt exakt dieselbe Auswahl, damit die gemeldete Provenienz nie vom
//! tatsächlich benutzten Embedder abweicht. Siehe `provenance.rs`s
//! `//!`-Block für die vollständige Begründung, insbesondere warum die
//! Vorgabe ohne konfigurierten Endpunkt unverändert bleibt und warum
//! `lens_ask` (siehe `crate::ask_tool`) den synchronen Aufruf deshalb über
//! `tokio::task::spawn_blocking` führt.
//!
//! # Wie übersprungene Indizes sichtbar bleiben
//! `lens.ask` befragt intern **alle** Indizes im abgeleiteten `ReadScope`
//! über [`harw_lens_federation::federated_query`] und verschmilzt die
//! Treffer über RRF. Ein inkompatibler Index (abweichendes Modell oder
//! abweichende Zerlegungsfassung gegenüber der Provenienz) wird nicht
//! mitfusioniert, aber auch nicht stillschweigend weggelassen:
//! [`harw_lens_federation::FederatedOutcome::skipped`] wird unverändert in
//! die JSON-Antwort von `lens.ask` durchgereicht (Feld `"skipped"`, neben
//! `"hits"` und `"queried"`) -- siehe `ask_tool.rs`s `//!`-Block, Abschnitt
//! „Übersprungene Indizes bleiben sichtbar".
//!
//! # Welche Werkzeuge es gibt -- und welches ausdrücklich nicht
//! **Genau ein Werkzeug: `lens.ask`.** Kein `lens.list_indices` daneben --
//! siehe `ask_tool.rs`s `//!`-Block für die Begründung, warum eine Auflistung
//! die Wahlfreiheit (welcher Index, also implizit welche Sichtbarkeit)
//! wieder beim Aufrufer ablegen würde, die dieses Werkzeug ihm bewusst
//! entzieht.
//!
//! # Weiterer Befund: `federated_query` bricht bei jedem Auflösungsfehler ab,
//! nicht nur bei Sichtbarkeitsverstößen
//! [`harw_lens_federation::federated_query`] ruft `resolve_index(...)?` je
//! Selektor auf -- das `?` propagiert **jeden** Fehler, den `resolve_index`
//! liefert, nicht nur [`harw_lens_query::QueryError::IndexNotVisible`]. Ein
//! Katalogeintrag in [`scope::KNOWN_SELECTORS`], dessen Index schlicht noch
//! nicht gebaut wurde (statt verboten zu sein), bricht damit **die gesamte**
//! `lens.ask`-Anfrage ab -- nicht nur den einen betroffenen Index, wie es
//! [`harw_lens_federation::SkipReason::IncompatibleManifest`] für einen
//! Modell-/Zerlegungs-Konflikt bereits vorsieht. Aus Sicht dieses ersten
//! echten Konsumenten ist das eine Lücke: ein Werkzeug mit einem statischen
//! Katalog (wie `lens.ask`) muss entweder vorab wissen, welche Indizes
//! bereits existieren (wofür es aktuell keine Introspektionsfunktion in
//! `harw_lens`/`harw_lens_query` gibt -- siehe die Begründung in `harw-lens`s
//! `//!`-Block, warum `IndexManifest` nicht Teil der Fassadenfläche ist),
//! oder `federated_query` müsste „Index existiert nicht" ebenfalls als
//! `SkipReason` statt als abbrechenden Fehler behandeln. Dieser Knoten
//! schließt diese Lücke nicht selbst (das wäre eine Änderung an
//! `harw-lens-federation`, außerhalb des Schreibbereichs dieses Knotens),
//! sondern hält sie hier fest; siehe
//! `ask_tool.rs`s Test `test_agent_reaches_lens_hits_through_the_tool` für
//! den Beleg (er muss **beide** bekannten `DEFAULT_VISIBILITY`-Indizes bauen,
//! sonst schlägt die gesamte Anfrage fehl, obwohl nur einer der beiden für
//! die gestellte Frage relevant ist).
//!
//! **Ausdrücklich kein schreibendes Werkzeug.** Diese Crate ruft
//! [`harw_lens::build`] nirgends auf. Der Schreibpfad ist
//! `harw-lens-source`, aufgerufen von einer eigenen Bau-Pipeline, nicht von
//! einem Modell auf Zuruf -- siehe `provider.rs`s Test
//! `test_provider_exposes_no_write_capable_tool` für den Beleg.
//!
//! # Schlüsseltypen
//! - [`LensToolProvider`] -- der `ToolProvider` mit `lens.ask`.
//! - [`ask_tool::LensAskArgs`], [`ask_tool::LensAskTool`] -- Argumente und
//!   generierter Executor.
//! - [`scope::derive_read_scope`], [`scope::selectors_in_scope`],
//!   [`scope::KNOWN_SELECTORS`] -- die Sichtbarkeitsgrenze.
//! - [`provenance::ask_provenance`], [`provenance::ask_embedder`],
//!   [`provenance::ask_descriptor`] -- die Provenienzquelle.
//!
//! # Fehler
//! Kein eigener Fehlertyp: alle Ablehnungen (fehlende Berechtigung, leerer
//! Lesebereich, Föderationsfehler, nicht auflösbarer Root-Space) münden in
//! `Ok(harw_tools::ToolOutput::Error)`, dasselbe Muster wie jeder andere
//! Executor in `harw-tools`/`harw-tool-deps`. `Err(harw_tools::ToolsError)`
//! nur bei nicht deserialisierbaren Rohargumenten (z. B. ein unbekanntes
//! Feld, siehe K19).
//!
//! # Nebenläufigkeit
//! [`LensToolProvider`] ist eine zustandslose Unit-Struktur, `Send + Sync +
//! Copy`. `lens.ask` selbst ist **nicht** `parallel_safe` -- siehe
//! `provider.rs`s `//!`-Block für die Begründung (fehlende Lese-Sperre
//! gegenüber einem laufenden `harw_lens::build` desselben Index).
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_extension_api::contributors::ToolProvider;
//! use harw_tool_lens::LensToolProvider;
//!
//! let provider = LensToolProvider::new();
//! for spec in provider.tools() {
//!     println!("{}", spec.name());
//! }
//! ```
//!
//! # Stand
//! Inhalt aus Knoten **AW6-10**; Ebene **L5** im Zielgraphen. Abhängigkeiten:
//! `harw-lens`, `harw-lens-federation`, `harw-home`, `harw-tools`,
//! `harw-sandbox`, `harw-macros` (alle vorgelagert gelandet).

#![forbid(unsafe_code)]

pub mod ask_tool;
pub mod provenance;
pub mod provider;
pub mod scope;

pub use ask_tool::{LensAskArgs, LensAskTool};
pub use provider::LensToolProvider;
