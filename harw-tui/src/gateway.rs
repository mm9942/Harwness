//! `ChatGateway` — Abstraktion zwischen TUI und Runtime.
//!
//! # Verantwortungsbereich
//! Definiert die Naht zwischen der TUI-Schleife und der Agent-Runtime. Heute
//! ist der einzige Implementor [`LocalGateway`], der `AgentSession`, einen
//! injizierten [`harw_core::StateStore`] und `ModelProvider` in einem
//! gemeinsamen Besitz bündelt.
//! Der Trait exponiert diese Bausteine über schmale Getter — damit läuft der
//! bestehende Turn-Loop unverändert, aber die Konstruktion ist hinter einer
//! einzigen Fabrikstelle versammelt.
//!
//! # Warum Getter statt "drive_turn"?
//! Der bestehende `run_turn_streaming`-Pfad in [`crate::app`] pflegt einen
//! feinkörnigen `run_turn`-Future-Poll-Zyklus, um Tipp-Animation und
//! Live-Tool-Events zu synchronisieren. Ein Kollaps zu `async fn drive_turn`
//! würde diese Steuerung verlieren. Der Trait zieht deshalb bewusst die
//! **Konstruktions-Grenze** — Session/Store/Provider werden hier gebaut und
//! kollektiv gehalten — und lässt die Turn-Antrieb-Logik dort, wo sie ihren
//! wertvollen Kontext hat.
//!
//! # Nächste Etappe
//! Ein `RemoteGateway` kann `LocalGateway` ersetzen, indem er intern zu einem
//! Gateway-Prozess spricht (RPC/HTTP/tokio-tcp). Solange er dieselben
//! `session_mut()`/`store()`/`model()`-Getter liefert, muss `run_chat_tui`
//! nichts anderes wissen. Dann verschieben wir schrittweise die Turn-Logik
//! ebenfalls hinter den Trait, sobald das Streaming-Protokoll klarer wird.

use harw_core::{AgentSession, ModelProvider, StateStore};

/// Abstrakte Runtime-Schnittstelle, gegen die die TUI-Schleife arbeitet.
///
/// # Beschreibung
/// Bündelt die drei Bausteine, die ein Turn braucht: Session (Historie +
/// Registry + Event-Sinks), Store (Persistenz) und Provider (Modell-Zugang).
/// Der Trait verzichtet bewusst auf `async fn drive_turn` — siehe Modul-Doku.
///
/// # Nebenläufigkeit
/// `Send` reicht (das TUI läuft single-threaded auf `current_thread`-Tokio).
pub trait ChatGateway: Send {
    /// Mutable Referenz auf die aktive `AgentSession`.
    fn session_mut(&mut self) -> &mut AgentSession;

    /// Lesend auf den State-Store zugreifen.
    fn store(&self) -> &dyn StateStore;

    /// Lesend auf den Modell-Provider zugreifen.
    fn model(&self) -> &dyn ModelProvider;

    /// Split-Borrow: die drei disjunkten Bausteine gleichzeitig ausleihen.
    ///
    /// # Beschreibung
    /// Der Turn-Loop (`run_turn(session: &mut, model: &, store: &, …)`) braucht
    /// alle drei Referenzen gleichzeitig. Ohne diese Methode müsste der
    /// Aufrufer entweder `unsafe` verwenden oder die drei Getter in einer
    /// nichtssagenden Reihenfolge kombinieren. Ein `&mut self`-Aufruf, der ein
    /// Triple aus einer mutablen und zwei immutablen Referenzen zurückgibt,
    /// ist im Owner-Impl trivial in sicherem Rust ausdrückbar (disjunkte
    /// Felder). Der Trait-Default-Impl kommt bewusst nicht — jeder Backend
    /// muss die Reihenfolge selbst offenlegen, damit klar ist, dass alle drei
    /// Referenzen aus **derselben** Instanz stammen.
    fn borrow_turn_ctx(&mut self) -> (&mut AgentSession, &dyn StateStore, &dyn ModelProvider);
}

/// Lokale Runtime-Implementierung — heute die einzige Option.
///
/// # Beschreibung
/// Bündelt `AgentSession`, einen injizierten State-Store und einen Box'd
/// `ModelProvider`, sodass `run_chat_tui` nur noch **einen** Konstruktor
/// aufruft statt drei separate Werte zusammenzufügen. Die konkreten Felder
/// bleiben privat; nur die Getter des [`ChatGateway`]-Trait sind öffentlich.
pub struct LocalGateway {
    session: AgentSession,
    store: Box<dyn StateStore>,
    model: Box<dyn ModelProvider>,
}

impl LocalGateway {
    /// Baut ein lokales Gateway aus den drei Kern-Bausteinen.
    ///
    /// # Argumente
    /// - `session` — bereits konstruierte, ggf. mit `with_turn_event_sink`
    ///   dekorierte `AgentSession`.
    /// - `store` — Turn-Store; die konkrete Persistenz bleibt außerhalb der
    ///   TUI-Gateway-Grenze konfigurierbar.
    /// - `model` — Modell-Provider (echt oder Echo-Fallback).
    #[must_use]
    pub fn new(
        session: AgentSession,
        store: Box<dyn StateStore>,
        model: Box<dyn ModelProvider>,
    ) -> Self {
        Self {
            session,
            store,
            model,
        }
    }
}

impl ChatGateway for LocalGateway {
    fn session_mut(&mut self) -> &mut AgentSession {
        &mut self.session
    }

    fn store(&self) -> &dyn StateStore {
        &*self.store
    }

    fn model(&self) -> &dyn ModelProvider {
        &*self.model
    }

    fn borrow_turn_ctx(&mut self) -> (&mut AgentSession, &dyn StateStore, &dyn ModelProvider) {
        // Disjunkte Felder-Zugriffe: der Compiler erlaubt einen `&mut` und zwei
        // `&` aus derselben Struktur, solange die referenzierten Felder
        // voneinander getrennt sind. Kein `unsafe` nötig.
        (&mut self.session, &*self.store, &*self.model)
    }
}
