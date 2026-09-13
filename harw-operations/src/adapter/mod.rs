//! Adapter, die eine [`crate::operation::Operation`] auf eine konkrete
//! Expositionsfläche abbilden (Command-Zeile, Modell-Tool, HTTP-Route, …).
//!
//! # Verantwortungsbereich
//! - Wandeln [`crate::operation::Surface`]-Deklarationen einer Operation in
//!   Adapter-Instanzen um, die je Fläche eigene Metadaten (Sichtbarkeit,
//!   Approval, Read-Only-Flag) tragen.
//! - Rufen `Operation::run` mit dem korrekt aufgebauten [`crate::operation::OpInput`] auf.
//! - Kennen keine Rendering- oder Registry-Details — das bleibt Sache der
//!   höheren Crates (`harw-tui`, `harw-core`).

pub mod command;
pub mod model_tool;
pub mod web;

pub use command::CommandAdapter;
pub use model_tool::{ModelToolAdapter, ModelToolProvider};
pub use web::WebAdapter;

// Hinweis: `AgentToolAdapter` ist in Strang 3 in `harw-core-bridge` ausgelagert,
// weil er als einziger Adapter direkt gegen `harw-core` (ManagedAgentSpawner,
// StateStore, TurnInput/TurnOutcome) linkt.
//
// `WebAdapter` (Knoten UI-00) exponiert `Surface::Web` für `harw-web` nach
// demselben Muster wie `CommandAdapter`/`ModelToolAdapter` — siehe
// `web`-Moduldoku für die Feldbegründung und die Kardinalität.
