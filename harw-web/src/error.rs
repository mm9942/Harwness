//! Fehlertypen für `harw-web`.
//!
//! Definiert [`WebError`], den zentralen Fehler-Enum dieser Crate, über
//! `#[derive(HarwError)]` (dieselbe hausinterne Ableitung wie
//! `harw_sentinel::ipc::IpcError` — kein `anyhow`, kein `thiserror`).
//! `#[derive(HarwError)]` erzeugt `Display`, `impl std::error::Error` und
//! den `WebResult<T>`-Typalias; `Debug` wird — wie von der Ableitung
//! gefordert — hier von Hand über `#[derive(HarwError)]`
//! mitgeliefert, nicht von `HarwError` selbst erzeugt.
//!
//! Nur [`WebError::EventEncode`] trägt `#[from]`: es ist die einzige
//! Variante, die einen Fremdfehler (`serde_json::Error`) unverändert
//! weiterreicht, wenn [`crate::events::WebEvent`] beim Aufbau eines
//! SSE-Rahmens serialisiert wird. Alle anderen Varianten verwerfen die
//! Betriebssystem-Fehlerursache bewusst (dasselbe Muster wie
//! `harw_sentinel::ipc::IpcError::Bind`), damit keine interne Fehlerkennung
//! den Prozess über eine HTTP-Antwort verlässt.

use harw_macros::HarwError;

/// Fehler, der beim Aufbau oder Betrieb der `harw-web`-Transportfläche
/// auftreten kann.
///
/// # Varianten
/// - [`WebError::Bind`]: Der Unix-Socket-Listener konnte nicht gebunden werden.
/// - [`WebError::SocketInUse`]: Unter dem Socket-Pfad lauscht bereits ein
///   anderer Prozess (oder das Gegenteil ist nicht belegbar).
/// - [`WebError::SocketPathOccupied`]: Unter dem Socket-Pfad liegt etwas
///   anderes als ein Socket (reguläre Datei, Symlink, Verzeichnis, …).
/// - [`WebError::Accept`]: `accept()` scheiterte fatal (Listener unbrauchbar).
/// - [`WebError::PeerCredentialsUnavailable`]: `SO_PEERCRED` einer angenommenen
///   Verbindung war nicht lesbar.
/// - [`WebError::DuplicateRoute`]: Zwei Operationen deklarieren denselben
///   `Surface::Web`-Pfad.
/// - [`WebError::RouteMethodUndeclared`]: Für eine Route ließ sich keine
///   `Surface::Web { method, .. }`-Deklaration finden (fail-closed, F-031).
/// - [`WebError::InvalidEventCapacity`]: Eine [`crate::events::WebEventBus`]
///   wurde mit Kapazität `0` angefordert.
/// - [`WebError::EventEncode`]: Ein [`crate::events::WebEvent`] ließ sich nicht
///   als JSON kodieren.
///
/// # Beispiel
/// ```rust
/// use harw_web::error::WebError;
///
/// let err = WebError::InvalidEventCapacity;
/// assert!(err.to_string().contains("Kapazität"));
/// ```
#[derive(HarwError)]
pub enum WebError {
    /// Der Unix-Socket-Listener konnte unter `path` nicht gebunden werden.
    #[msg("Web-Listener konnte unter '{path}' nicht gebunden werden")]
    Bind {
        /// Der Pfad, an dem das Binden fehlschlug.
        path: String,
    },

    /// Unter `path` liegt ein Socket, dessen Lebendigkeit nicht widerlegt
    /// werden konnte (ein `connect` gelang, lief in die Frist oder scheiterte
    /// anders als mit `ECONNREFUSED`). Die Datei wurde **nicht** entfernt.
    #[msg("Web-Socket '{path}' ist noch in Benutzung — wird nicht ersetzt")]
    SocketInUse {
        /// Der belegte Socket-Pfad.
        path: String,
    },

    /// Unter `path` liegt kein Socket (reguläre Datei, Symlink, Verzeichnis,
    /// …). Die Datei wurde **nicht** angefasst.
    #[msg(
        "Web-Socket-Pfad '{path}' ist durch eine Nicht-Socket-Datei belegt — wird nicht entfernt"
    )]
    SocketPathOccupied {
        /// Der belegte Pfad.
        path: String,
    },

    /// `accept()` scheiterte fatal — der Listener selbst ist unbrauchbar
    /// (vorübergehende Fehler wie `EMFILE` führen nicht hierher, siehe
    /// `crate::server`-Moduldoku).
    #[msg("Web-Listener unbrauchbar: accept() scheiterte fatal")]
    Accept,

    /// Die Peer-Identität (`SO_PEERCRED`) einer angenommenen Verbindung
    /// konnte nicht gelesen werden.
    #[msg("Peer-Credentials (SO_PEERCRED) einer Web-Verbindung nicht lesbar")]
    PeerCredentialsUnavailable,

    /// Zwei Operationen deklarieren denselben `Surface::Web`-Pfad.
    #[msg("doppelte Web-Route '{path}': beansprucht von '{first_owner}' und '{second_owner}'")]
    DuplicateRoute {
        /// Der kollidierende Pfad.
        path: String,
        /// Name der Operation, die den Pfad zuerst beanspruchte.
        first_owner: String,
        /// Name der Operation, die denselben Pfad erneut beanspruchte.
        second_owner: String,
    },

    /// Für die Route `path` der Operation `operation` ließ sich keine
    /// `Surface::Web`-Deklaration mit HTTP-Methode finden. Die Routentabelle
    /// wird dann nicht gebaut — eine Route ohne deklarierte Methode wird nie
    /// bedient (F-031: keine Ableitung, kein Standardwert).
    #[msg("Web-Route '{path}' der Operation '{operation}' hat keine deklarierte HTTP-Methode")]
    RouteMethodUndeclared {
        /// Der betroffene Routenpfad.
        path: String,
        /// Name der Operation, die die Route bereitstellt.
        operation: String,
    },

    /// Eine [`crate::events::WebEventBus`] wurde mit Kapazität `0` angefordert.
    #[msg("Kapazität des Web-Ereignisbusses muss größer als null sein")]
    InvalidEventCapacity,

    /// Ein [`crate::events::WebEvent`] ließ sich nicht als JSON kodieren.
    ///
    /// # Arguments
    /// - `0` (`serde_json::Error`): die zugrunde liegende Serde-Ursache.
    #[msg("Web-Ereignis ließ sich nicht als JSON kodieren: {0}")]
    #[from]
    EventEncode(serde_json::Error),
}

/// `Debug` delegiert an `Display` -- eine Implementierung, keine Doppelung.
///
/// Das ist die Hauskonvention für Fehlertypen (siehe `error-enum-design`):
/// ein abgeleitetes `Debug` druckt den Variantennamen (`Accept`), während
/// `Display` die Meldung trägt. Zwei Darstellungen desselben Fehlers laufen
/// auseinander; hier gibt es nur eine.
impl ::core::fmt::Debug for WebError {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        ::core::fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::WebError;
    use crate::test_support::{TestError, TestResult};
    use std::error::Error;

    #[test]
    fn test_display_bind_contains_path() {
        let err = WebError::Bind {
            path: "/run/harw/web.sock".to_owned(),
        };
        assert!(err.to_string().contains("/run/harw/web.sock"));
    }

    #[test]
    fn test_display_socket_in_use_contains_path() {
        let err = WebError::SocketInUse {
            path: "/run/harw/web.sock".to_owned(),
        };
        let text = err.to_string();
        assert!(text.contains("/run/harw/web.sock"));
        assert!(text.contains("in Benutzung"));
    }

    #[test]
    fn test_display_socket_path_occupied_contains_path() {
        let err = WebError::SocketPathOccupied {
            path: "/tmp/fremd".to_owned(),
        };
        let text = err.to_string();
        assert!(text.contains("/tmp/fremd"));
        assert!(text.contains("Nicht-Socket"));
    }

    #[test]
    fn test_socket_variants_are_distinct_from_bind() {
        let path = "/p".to_owned();
        let in_use = WebError::SocketInUse { path: path.clone() }.to_string();
        let occupied = WebError::SocketPathOccupied { path: path.clone() }.to_string();
        let bind = WebError::Bind { path }.to_string();
        assert_ne!(in_use, occupied);
        assert_ne!(in_use, bind);
        assert_ne!(occupied, bind);
    }

    #[test]
    fn test_display_duplicate_route_contains_all_names() {
        let err = WebError::DuplicateRoute {
            path: "/api/x".to_owned(),
            first_owner: "op-a".to_owned(),
            second_owner: "op-b".to_owned(),
        };
        let text = err.to_string();
        assert!(text.contains("/api/x"));
        assert!(text.contains("op-a"));
        assert!(text.contains("op-b"));
    }

    #[test]
    fn test_display_route_method_undeclared_contains_path_and_operation() {
        let err = WebError::RouteMethodUndeclared {
            path: "/api/analyze".to_owned(),
            operation: "analyze".to_owned(),
        };
        let text = err.to_string();
        assert!(text.contains("/api/analyze"));
        assert!(text.contains("analyze"));
        assert!(text.contains("HTTP-Methode"));
        assert!(err.source().is_none());
    }

    #[test]
    fn test_debug_matches_display_for_accept() {
        let err = WebError::Accept;
        assert_eq!(format!("{err:?}"), format!("{err}"));
    }

    #[test]
    fn test_source_is_none_for_content_free_variants() {
        assert!(WebError::Accept.source().is_none());
        assert!(
            WebError::SocketInUse {
                path: String::new()
            }
            .source()
            .is_none()
        );
        assert!(
            WebError::SocketPathOccupied {
                path: String::new()
            }
            .source()
            .is_none()
        );
        assert!(WebError::PeerCredentialsUnavailable.source().is_none());
        assert!(WebError::InvalidEventCapacity.source().is_none());
    }

    #[test]
    fn test_from_serde_json_error_wires_source() -> TestResult {
        let Err(json_err) = serde_json::from_str::<serde_json::Value>("{not json") else {
            return Err(TestError::Unexpected(
                "malformed JSON must fail to parse".into(),
            ));
        };
        let err: WebError = json_err.into();
        assert!(matches!(err, WebError::EventEncode(_)));
        assert!(err.source().is_some());
        Ok(())
    }

    #[test]
    fn test_op_error_usable_as_dyn_error() {
        let err: &dyn Error = &WebError::Accept;
        assert!(!err.to_string().is_empty());
    }
}
