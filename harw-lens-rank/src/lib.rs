//! Reine Rangfunktionen.
//!
//! # Verantwortungsbereich
//! `rrf_fuse` ([`fuse`]), `mmr` ([`mmr`]), `collapse` ([`collapse`]) und
//! `pack` ([`pack`]) — ohne I/O, ohne Systemzeit, ohne Zufallsquelle. `pack`
//! ist der einzige echte Vertrag zwischen dem Retrieval-Teilsystem (Lens)
//! und der Kontextmontage: AW6-07 prüft ausdrücklich, dass die Montage
//! dieselbe Funktion aufruft wie Lens. Zwei Teilsysteme, die dasselbe
//! Budget unterschiedlich füllen, drifteten sonst unweigerlich auseinander.
//!
//! # Nebenläufigkeit
//! Alle vier Funktionen sind zustandslos und frei von innerer
//! Veränderlichkeit; sie sind aus jedem Thread parallel aufrufbar, solange
//! die Eingaben nicht gleichzeitig verändert werden.
//!
//! # Fehler
//! Keine — alle vier Funktionen sind total und geben nie `Result` zurück.
//! Ein außerhalb `[0, 1]` liegendes `lambda` in [`mmr::mmr`] wird geklemmt,
//! ein einzelner Kandidat über Budget in [`pack::pack`] wird übersprungen;
//! nichts davon ist ein Fehlerfall.
//!
//! # Stand
//! Inhalt aus Knoten **AW0-09**, nach `docs/aw-contract-master.md`
//! Abschnitt D. Siehe die Moduldokumentation von [`collapse::collapse`] für
//! eine Vertragslücke gegenüber Abschnitt C (`EdgeIndex`-Methoden).
//!
//! # Examples
//! ```rust
//! use harw_lens_rank::{collapse, mmr, pack, rrf_fuse};
//! use harw_lens_types::{BudgetSpec, BytesOverFour, CollapsePolicy, EdgeIndex};
//!
//! let fused = rrf_fuse(&[], 60.0);
//! let diverse = mmr(&fused, 0.5, 10);
//! let deduped = collapse(&diverse, CollapsePolicy::ByDigest, &EdgeIndex::default());
//! let packed = pack(&deduped, &BytesOverFour, &BudgetSpec { total: 4096 });
//! assert!(packed.selected.is_empty());
//! ```

mod collapse;
mod fuse;
mod mmr;
mod pack;

pub use collapse::collapse;
pub use fuse::rrf_fuse;
pub use mmr::mmr;
pub use pack::pack;
