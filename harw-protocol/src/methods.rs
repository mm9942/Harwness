//! Bekannte Methoden- und Notification-Namen für den Wire-Transport.

pub const METHOD_TURN_SUBMIT: &str = "turn.submit";
pub const METHOD_TURN_INTERRUPT: &str = "turn.interrupt";
pub const METHOD_APPROVAL_RESP: &str = "approval.respond";
pub const METHOD_SESSION_CLOSE: &str = "session.close";
pub const METHOD_SESSION_HELLO: &str = "session.hello";
pub const METHOD_SESSION_LIST: &str = "session.list";
pub const METHOD_SESSION_CREATE: &str = "session.create";
pub const METHOD_SESSION_ATTACH: &str = "session.attach";
pub const METHOD_SESSION_DETACH: &str = "session.detach";
pub const METHOD_SESSION_HISTORY: &str = "session.history";
pub const METHOD_SESSION_RESUME: &str = "session.resume";
pub const METHOD_SESSION_SET_MODEL: &str = "session.set_model";
pub const METHOD_SESSION_SET_MODE: &str = "session.set_mode";
pub const METHOD_SESSION_SET_EFFORT: &str = "session.set_effort";

// R18 D-A: Werkzeugaufrufe über das Gateway, nur für Agent-Principals
// (Vertrag `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md`).
pub const METHOD_TOOL_LIST: &str = "tool.list";
pub const METHOD_TOOL_CALL: &str = "tool.call";
pub const METHOD_TOOL_CANCEL: &str = "tool.cancel";

// R18 D-B: Gateway lesen (`gateway_read`) und verwalten (`gateway_admin`).
pub const METHOD_GATEWAY_STATUS: &str = "gateway.status";
pub const METHOD_GATEWAY_CONNECTIONS_LIST: &str = "gateway.connections.list";
pub const METHOD_GATEWAY_SESSIONS_LIST: &str = "gateway.sessions.list";
pub const METHOD_GATEWAY_LISTENERS_LIST: &str = "gateway.listeners.list";
pub const METHOD_GATEWAY_TOOLS_LIST: &str = "gateway.tools.list";
pub const METHOD_GATEWAY_CONNECTIONS_REVOKE: &str = "gateway.connections.revoke";
pub const METHOD_GATEWAY_DRAIN: &str = "gateway.drain";
pub const METHOD_GATEWAY_LISTENERS_SET: &str = "gateway.listeners.set";
pub const METHOD_GATEWAY_TOOLS_GRANT: &str = "gateway.tools.grant";
pub const METHOD_GATEWAY_TOOLS_NARROW: &str = "gateway.tools.narrow";

// Feature `media`.
pub const METHOD_SESSION_MEDIA_PREVIEW: &str = "session.media.preview";
pub const METHOD_MEDIA_GET: &str = "media.get";

pub const METHOD_SESSION_PROJECTS: &str = "session.projects";
pub const METHOD_SESSION_PAGE: &str = "session.page";
pub const METHOD_SESSION_ADOPT: &str = "session.adopt";
pub const METHOD_SESSION_CREATE_IN: &str = "session.create_in";
pub const METHOD_SESSION_SET_PROJECT: &str = "session.set_project";
pub const METHOD_SESSION_SET_EXISTING_PROJECT: &str = "session.set_existing_project";
pub const METHOD_SESSION_RENAME: &str = "session.rename";
pub const METHOD_AUTO_TITLE_SEED: &str = "session.auto_title_seed";
pub const METHOD_AUTO_TITLE_PROBE: &str = "session.auto_title_probe";
pub const METHOD_AUTO_TITLE_PUBLISH: &str = "session.auto_title_publish";
pub const METHOD_SESSION_ARCHIVE: &str = "session.archive";
pub const METHOD_ARCHIVE_CHECKPOINT_ISSUE: &str = "session.revision_checkpoint.issue";
pub const METHOD_ARCHIVE_CHECKPOINT_DESCRIBE: &str = "session.revision_checkpoint.describe";
pub const METHOD_ARCHIVE_CHECKPOINT_PREVIOUS: &str = "session.revision_checkpoint.previous";
pub const METHOD_ARCHIVE_CHECKPOINT_PAGE: &str = "session.revision_checkpoint.page";
pub const METHOD_FORK_INCLUSIVE: &str = "session.fork_inclusive";
pub const METHOD_CLOUD_ASSISTANT_STATUS: &str = "cloud_assistant.status";
pub const METHOD_CLOUD_ASSISTANT_PROVISION: &str = "cloud_assistant.provision";
pub const METHOD_CLOUD_ASSISTANT_SUBMIT: &str = "cloud_assistant.submit";
pub const METHOD_CLOUD_ASSISTANT_SUBMIT_WITH_RECALL: &str = "cloud_assistant.submit_with_recall";
pub const METHOD_CLOUD_ASSISTANT_MODELS: &str = "cloud_assistant.models";
pub const METHOD_CLOUD_ASSISTANT_SUBMIT_MODEL: &str = "cloud_assistant.submit_model";
pub const METHOD_CLOUD_ASSISTANT_REVOKE: &str = "cloud_assistant.revoke";
pub const METHOD_CLOUD_ASSISTANT_SHARED: &str = "cloud_assistant.shared";

// Minor 4, feature `manage`.
pub const METHOD_COMMANDS_LIST: &str = "commands.list";
pub const METHOD_COMMAND_RUN: &str = "command.run";
pub const METHOD_CATALOG_GET: &str = "catalog.get";
pub const METHOD_CONFIG_GET: &str = "config.get";
// Minor 4, feature `stepup`.
pub const METHOD_CONFIG_SET: &str = "config.set";
pub const METHOD_CONFIG_RESET: &str = "config.reset";
pub const METHOD_SECRET_PUT: &str = "secret.put";
pub const METHOD_STEPUP_CONFIRM: &str = "stepup.confirm";
pub const METHOD_STEPUP_CANCEL: &str = "stepup.cancel";

// Feature `overview`: global jobs and agents views.
pub const METHOD_JOBS_LIST: &str = "jobs.list";
pub const METHOD_JOBS_GET: &str = "jobs.get";
pub const METHOD_JOBS_CANCEL: &str = "jobs.cancel";
pub const METHOD_JOBS_RETRY: &str = "jobs.retry";
pub const METHOD_AGENTS_LIST: &str = "agents.list";
pub const METHOD_AGENTS_STOP: &str = "agents.stop";

pub const NOTIF_SESSION_EVENT: &str = "event.session";
pub const NOTIF_TURN_EVENT: &str = "event.turn";
pub const NOTIF_APPROVAL_REQ: &str = "approval.request";
/// One [`crate::session_wire::FrameEnvelope`] of an attached session.
pub const NOTIF_EVENT_FRAME: &str = "event.frame";

/// Every method of the closed session control plane table (W00 D5).
pub const SESSION_METHODS: &[&str] = &[
    METHOD_SESSION_HELLO,
    METHOD_SESSION_LIST,
    METHOD_SESSION_CREATE,
    METHOD_SESSION_ATTACH,
    METHOD_SESSION_DETACH,
    METHOD_SESSION_HISTORY,
    METHOD_SESSION_RESUME,
    METHOD_SESSION_CLOSE,
    METHOD_SESSION_SET_MODEL,
    METHOD_SESSION_SET_MODE,
    METHOD_SESSION_SET_EFFORT,
    METHOD_TURN_SUBMIT,
    METHOD_TURN_INTERRUPT,
    METHOD_APPROVAL_RESP,
];

/// Minor 3, feature `projects`: projects and pages. Own table, NOT part of
/// `SESSION_METHODS`.
pub const PROJECT_METHODS: &[&str] = &[
    METHOD_SESSION_PROJECTS,
    METHOD_SESSION_PAGE,
    METHOD_SESSION_ADOPT,
    METHOD_SESSION_CREATE_IN,
    METHOD_SESSION_SET_PROJECT,
    METHOD_SESSION_RENAME,
    METHOD_SESSION_ARCHIVE,
];

/// Optional non-creating assignment, separate from the legacy project table.
pub const EXISTING_PROJECT_ASSIGNMENT_METHODS: &[&str] = &[METHOD_SESSION_SET_EXISTING_PROJECT];

/// Minor 5, feature `archive_checkpoint_v1`. Separate from base commands.
pub const ARCHIVE_CHECKPOINT_METHODS: &[&str] = &[
    METHOD_ARCHIVE_CHECKPOINT_ISSUE,
    METHOD_ARCHIVE_CHECKPOINT_DESCRIBE,
    METHOD_ARCHIVE_CHECKPOINT_PAGE,
    METHOD_FORK_INCLUSIVE,
];

/// Minor 5, optional feature `archive_retained_rewrite_v1`.
pub const ARCHIVE_RETAINED_REWRITE_METHODS: &[&str] = &[METHOD_ARCHIVE_CHECKPOINT_PREVIOUS];

/// Feature `media`: stored prompt images. Own table.
pub const MEDIA_METHODS: &[&str] = &[METHOD_MEDIA_GET];
/// Separate optional V5 exact-occurrence reader.
pub const SESSION_MEDIA_PREVIEW_METHODS: &[&str] = &[METHOD_SESSION_MEDIA_PREVIEW];


/// Minor 4, feature `manage`: typed, allow-listed operation execution (W00 D5
/// amendment). Own table.
pub const MANAGE_METHODS: &[&str] = &[
    METHOD_COMMANDS_LIST,
    METHOD_COMMAND_RUN,
    METHOD_CATALOG_GET,
    METHOD_CONFIG_GET,
];

/// Minor 4, feature `stepup`: step-up confirmed global writes. Own table.
pub const STEPUP_METHODS: &[&str] = &[
    METHOD_CONFIG_SET,
    METHOD_CONFIG_RESET,
    METHOD_SECRET_PUT,
    METHOD_STEPUP_CONFIRM,
    METHOD_STEPUP_CANCEL,
];

/// Feature `overview`: global jobs and agents views. Own table.
pub const OVERVIEW_METHODS: &[&str] = &[
    METHOD_JOBS_LIST,
    METHOD_JOBS_GET,
    METHOD_JOBS_CANCEL,
    METHOD_JOBS_RETRY,
    METHOD_AGENTS_LIST,
    METHOD_AGENTS_STOP,
];

/// R18 D-A: the closed `tool.*` method table.
pub const TOOL_METHODS: &[&str] = &[METHOD_TOOL_LIST, METHOD_TOOL_CALL, METHOD_TOOL_CANCEL];

/// R18 D-B: read-only `gateway.*` methods (cap `gateway_read`).
pub const GATEWAY_READ_METHODS: &[&str] = &[
    METHOD_GATEWAY_STATUS,
    METHOD_GATEWAY_CONNECTIONS_LIST,
    METHOD_GATEWAY_SESSIONS_LIST,
    METHOD_GATEWAY_LISTENERS_LIST,
    METHOD_GATEWAY_TOOLS_LIST,
];

/// R18 D-B: mutating `gateway.*` methods (cap `gateway_admin`).
pub const GATEWAY_ADMIN_METHODS: &[&str] = &[
    METHOD_GATEWAY_CONNECTIONS_REVOKE,
    METHOD_GATEWAY_DRAIN,
    METHOD_GATEWAY_LISTENERS_SET,
    METHOD_GATEWAY_TOOLS_GRANT,
    METHOD_GATEWAY_TOOLS_NARROW,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Die drei Tabellen sind disjunkt, und kein Name ist doppelt: ein
    /// Dispatcher ordnet jeden Namen genau einer Fähigkeit zu.
    #[test]
    fn method_tables_are_disjoint_and_unique() {
        let mut all: Vec<&str> = SESSION_METHODS
            .iter()
            .chain(PROJECT_METHODS)
            .chain(EXISTING_PROJECT_ASSIGNMENT_METHODS)
            .chain(ARCHIVE_CHECKPOINT_METHODS)
            .chain(ARCHIVE_RETAINED_REWRITE_METHODS)
            .chain(MEDIA_METHODS)
            .chain(SESSION_MEDIA_PREVIEW_METHODS)
            .chain(MANAGE_METHODS)
            .chain(STEPUP_METHODS)
            .chain(OVERVIEW_METHODS)
            .chain(TOOL_METHODS)
            .chain(GATEWAY_READ_METHODS)
            .chain(GATEWAY_ADMIN_METHODS)
            .copied()
            .collect();
        let total = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), total);
        assert!(TOOL_METHODS.iter().all(|name| name.starts_with("tool.")));
        assert!(
            PROJECT_METHODS
                .iter()
                .all(|name| name.starts_with("session."))
        );
        assert!(
            GATEWAY_READ_METHODS
                .iter()
                .chain(GATEWAY_ADMIN_METHODS)
                .all(|name| name.starts_with("gateway."))
        );
        assert!(
            MANAGE_METHODS
                .iter()
                .chain(STEPUP_METHODS)
                .all(|n| !n.starts_with("gateway.") && !n.starts_with("tool."))
        );
        // Schlüsseloperationen bleiben außerhalb jeder Gateway-Fläche.
        assert!(!all.iter().any(|name| name.contains("keys")));
    }

    #[test]
    fn manage_tables_have_exact_names() {
        assert_eq!(MANAGE_METHODS.len(), 4);
        assert_eq!(STEPUP_METHODS.len(), 5);
        assert_eq!(
            MANAGE_METHODS,
            &["commands.list", "command.run", "catalog.get", "config.get"]
        );
        assert_eq!(
            STEPUP_METHODS,
            &[
                "config.set",
                "config.reset",
                "secret.put",
                "stepup.confirm",
                "stepup.cancel"
            ]
        );
    }
}

/// Device/profile-scoped encrypted library, separately negotiated at minor 5.
pub const METHOD_CLOUD_LIBRARY: &str = "cloud.library";
/// Independent manual work create/receipt path, not exposed as a model tool.
pub const METHOD_MANUAL_CLOUD_WORK: &str = "session.manual_cloud_work";

/// One exact, coarse status on an independently authenticated owner Session connection.
pub const METHOD_DIRECT_OWNER_STATUS: &str = "session.direct_owner_status";

/// Exact selected placement with one-use Hub selectors on the original direct connection.
pub const METHOD_DIRECT_OWNER_STATUS_FENCED: &str = "session.direct_owner_status_fenced";

/// Create a short-lived exact Owner observation reservation on the current direct WebSocket.
pub const METHOD_RESERVE_OWNER_OBSERVATION: &str = "session.reserve_owner_observation";
