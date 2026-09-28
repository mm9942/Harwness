//! `ModelToolAdapter` — exponiert eine [`Operation`] als vom LLM aufrufbares Tool.
//!
//! # Verantwortungsbereich
//! Dieser Adapter übersetzt eine `Surface::ModelTool`-Deklaration einer Operation
//! in eine zur Laufzeit nutzbare Adapter-Instanz, die das LLM direkt aufrufen kann.
//!
//! # Abgrenzung gegenüber `CommandAdapter`
//! | Eigenschaft         | `CommandAdapter`                          | `ModelToolAdapter`                      |
//! |---------------------|-------------------------------------------|-----------------------------------------|
//! | Initiierung         | **Nutzer-initiiert** (Slash-Befehl, TUI)  | **Modell-initiiert** (Tool-Call)        |
//! | Eingabe             | `raw_args: Vec<String>` (Tokenisierung)   | `json_args: serde_json::Value` (JSON)   |
//! | Approval            | n/a (nutzergesteuert)                     | `ApprovalPolicy` (None / Always)        |
//! | Read-Only-Flag      | n/a                                       | `readonly: bool` für lib. Genehmigung   |
//! | Kardinalität        | 0..N Adapter pro Operation                | 0..1 Adapter pro Operation              |
//!
//! # Kardinalität
//! Eine Operation deklariert **maximal eine** `Surface::ModelTool`-Fläche.
//! `from_operation` gibt daher `Option<Self>` zurück. Sind defensiv mehrere
//! `Surface::ModelTool`-Einträge vorhanden, wird die **erste** verwendet.
//!
//! # Schlüsseltypen
//! - [`ModelToolAdapter`] — der Adapter selbst
//! - [`ModelToolProvider`] — veröffentlicht die Adapter als `ToolProvider` und
//!   reicht dabei [`crate::operation::OperationMeta::args_schema`] durch
//! - [`model_tool_schema_for`] — Argument-Schema eines Tools, mit Vorrang für ein
//!   vom Argument-Typ mitgebrachtes Schema (`#[derive(OpArgs)]`)
//!
//! # Nebenläufigkeit
//! `ModelToolAdapter` ist `Send + Sync` (alle Felder sind es). Mehrere Tokio-Tasks
//! können denselben Adapter über `Arc<ModelToolAdapter>` gemeinsam nutzen.
//!
//! # Fehlertypen
//! - [`crate::error::OpError`] — von `invoke` weitergeleitet.

use std::collections::BTreeMap;
use std::sync::Arc;

use harw_extension_api::contributors::ToolProvider;
use harw_tools::{
    AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType, ToolCall,
    ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput, ToolSpec,
    ToolsError,
};

use crate::context::OpContext;
use crate::error::OpError;
use crate::operation::{ApprovalPolicy, OpInput, OpOutput, Operation, Surface};

/// Trusted factory used to enrich harness-established tool authority with
/// operation services. It receives only [`ToolExecutionContext`], never model
/// arguments.
pub type OpContextFactory = dyn Fn(&ToolExecutionContext) -> OpContext + Send + Sync + 'static;

/// Bridges operations declaring [`Surface::ModelTool`] into the extension
/// [`ToolProvider`] contract.
///
/// The provider publishes each operation's own closed argument schema when its
/// argument type carries one (`#[derive(OpArgs)]` →
/// [`crate::operation::OperationMeta::args_schema`]), and otherwise falls back
/// to the closed schemas hard-coded for the built-in model tools. Unknown names
/// without either retain the conservative generic-object fallback so future
/// operations are not assigned invented argument contracts. The supplied
/// context factory remains the sole source of sandbox authority and services.
pub struct ModelToolProvider {
    adapters: Vec<Arc<ModelToolAdapter>>,
    context_factory: Arc<OpContextFactory>,
}

impl ModelToolProvider {
    /// Builds a provider, filtering out every operation that does not explicitly
    /// declare a [`Surface::ModelTool`].
    pub fn new<I, F>(operations: I, context_factory: F) -> Self
    where
        I: IntoIterator<Item = Arc<dyn Operation>>,
        F: Fn(&ToolExecutionContext) -> OpContext + Send + Sync + 'static,
    {
        let adapters = operations
            .into_iter()
            .filter_map(ModelToolAdapter::from_operation)
            .map(Arc::new)
            .collect();

        Self {
            adapters,
            context_factory: Arc::new(context_factory),
        }
    }

    /// Returns approval metadata for a published model tool. Enforcement is
    /// intentionally left to the later approval compositor.
    #[must_use]
    pub fn approval_policy(&self, name: &ToolName) -> Option<ApprovalPolicy> {
        self.find(name).map(|adapter| adapter.approval())
    }

    fn find(&self, name: &ToolName) -> Option<&Arc<ModelToolAdapter>> {
        self.adapters
            .iter()
            .find(|adapter| adapter.tool_name() == name.as_str())
    }
}

impl ToolProvider for ModelToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        self.adapters
            .iter()
            .map(|adapter| {
                // Das vom Argument-Typ mitgebrachte Schema (`#[derive(OpArgs)]`)
                // hat Vorrang; `None` fällt auf den Namens-`match` in
                // `model_tool_schema_for` zurück.
                let derived = adapter
                    .operation()
                    .meta()
                    .args_schema
                    .map(|build_schema| build_schema());
                ToolSpec::Function(FunctionToolSpec {
                    name: ToolName::new(adapter.tool_name()),
                    description: adapter.description().to_owned(),
                    parameters: model_tool_schema_for(adapter.tool_name(), derived),
                    strict: false,
                })
            })
            .collect()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        self.find(name).map(|adapter| {
            Arc::new(ModelToolExecutor {
                adapter: Arc::clone(adapter),
                context_factory: Arc::clone(&self.context_factory),
            }) as Arc<dyn ToolExecutor>
        })
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        self.find(name).is_some_and(|adapter| adapter.readonly())
    }
}

/// Ermittelt das JSON-Schema der Argumente eines Modell-Tools.
///
/// # Beschreibung
/// Die Auflösung erfolgt in drei Stufen, in dieser Reihenfolge:
///
/// 1. **`fallback`** — ein vom Aufrufer mitgebrachtes, geschlossenes Schema
///    (typischerweise aus `#[derive(OpArgs)]` über
///    [`crate::op_schema::OpArgsSchema::json_schema`]). Ist es gesetzt, gewinnt es
///    immer: es stammt direkt vom Argument-Typ der Operation und ist damit die
///    genauere Quelle.
/// 2. **Namens-`match`** — die historisch hartcodierten Schemata der eingebauten
///    Tools (`status`, `ps`, `diff`, `stop`). Dieser Zweig existiert nur noch aus
///    Rückwärtskompatibilität für Operationen, die noch kein `OpArgs`-Schema
///    mitbringen.
/// 3. **Offener Fallback** — ein `object`-Schema mit `additionalProperties: true`
///    ohne Feldinformation. Konservativ, aber für das Modell wertlos: es erfährt
///    weder Feldnamen noch Typen. Dieser Zweig protokolliert deshalb eine
///    `warn!`-Meldung.
///
/// # Verhältnis zum Namens-`match`
/// [`ModelToolProvider::tools`] reicht inzwischen
/// [`crate::operation::OperationMeta::args_schema`] als `fallback` herein
/// (`meta.args_schema.map(|build| build())`). Jede Operation mit
/// `#[derive(OpArgs)]` bringt ihr Schema damit selbst mit und erreicht Stufe 2
/// gar nicht mehr.
///
/// Stufe 2 bleibt trotzdem bestehen: die vier eingebauten Tools (`status`,
/// `ps`, `diff`, `stop`) tragen kein `OpArgs`-Derive. Ohne den `match` fielen
/// genau sie auf das offene Objekt aus Stufe 3 zurück — das wäre ein
/// Rückschritt, kein Aufräumen. Der Arm darf erst entfallen, wenn alle vier
/// Argument-Typen ein Schema mitbringen.
///
/// # Argumente
/// - `name` (`&str`): Tool-Name, entspricht `OperationMeta::name`.
/// - `fallback` (`Option<JsonSchema>`): Vom Argument-Typ geliefertes Schema.
///   Wird bei `Some` unverändert zurückgegeben (Eigentumsübergabe).
///
/// # Rückgabe
/// [`JsonSchema`] vom Typ `object`. Bei Stufe 1 und 2 geschlossen
/// (`additionalProperties: false`), bei Stufe 3 offen.
///
/// # Nebenläufigkeit
/// Rein, ohne Sperren; aus beliebig vielen Threads aufrufbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::adapter::model_tool::model_tool_schema_for;
///
/// let schema = model_tool_schema_for("stop", None);
/// assert!(schema.properties.is_some_and(|props| props.contains_key("job_id")));
/// ```
#[must_use]
pub fn model_tool_schema_for(name: &str, fallback: Option<JsonSchema>) -> JsonSchema {
    if let Some(schema) = fallback {
        return schema;
    }

    let property_types: &[(&str, JsonSchemaType)] = match name {
        "status" => &[],
        "ps" => &[
            ("status", JsonSchemaType::String),
            ("kind", JsonSchemaType::String),
        ],
        "diff" => &[
            ("path", JsonSchemaType::String),
            ("stat_only", JsonSchemaType::Boolean),
        ],
        "stop" => &[("job_id", JsonSchemaType::String)],
        _ => {
            tracing::warn!(
                tool = name,
                "model tool published without argument schema; falling back to an open object — \
                 derive OpArgs for its argument type and pass it as `fallback`"
            );
            return JsonSchema {
                schema_type: Some(JsonSchemaType::Object),
                additional_properties: Some(Box::new(AdditionalProperties::Bool(true))),
                ..Default::default()
            };
        }
    };

    let properties = property_types
        .iter()
        .map(|(property_name, schema_type)| {
            (
                (*property_name).to_owned(),
                JsonSchema {
                    schema_type: Some(schema_type.clone()),
                    ..Default::default()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(properties),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
}

struct ModelToolExecutor {
    adapter: Arc<ModelToolAdapter>,
    context_factory: Arc<OpContextFactory>,
}

impl ToolExecutor for ModelToolExecutor {
    fn execute<'a>(
        &'a self,
        execution_context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if call.name.as_str() != self.adapter.tool_name() {
                return Err(ToolsError::NotFound {
                    name: call.name.as_str().to_owned(),
                });
            }
            if !call.arguments.is_object() {
                return Err(ToolsError::InvalidArguments {
                    name: call.name.as_str().to_owned(),
                    reason: "arguments must be a JSON object".to_owned(),
                });
            }

            let op_context = (self.context_factory)(execution_context);
            self.adapter
                .invoke(&op_context, call.arguments.clone())
                .await
                .map_err(|error| map_operation_error(call.name.as_str(), error))
                .map(|output| ToolOutput::Text {
                    content: output.text,
                })
        })
    }
}

fn map_operation_error(name: &str, error: OpError) -> ToolsError {
    match error {
        OpError::InvalidArguments(reason) => ToolsError::InvalidArguments {
            name: name.to_owned(),
            reason,
        },
        error @ (OpError::Execution(_) | OpError::NotAvailable(_)) => {
            ToolsError::ExecutionFailed(error.to_string())
        }
    }
}

// ── Adapter ───────────────────────────────────────────────────────────────────

/// Adapter, der eine [`Operation`] als vom LLM direkt aufrufbares Tool exponiert.
///
/// # Beschreibung
/// `ModelToolAdapter` kapselt die `Surface::ModelTool`-Metadaten einer Operation
/// und stellt eine `invoke`-Methode bereit, die JSON-Argumente entgegennimmt und
/// an [`Operation::run`] delegiert.
///
/// Der Adapter wird ausschließlich über [`ModelToolAdapter::from_operation`] erzeugt —
/// direkte Konstruktion ist nicht vorgesehen.
///
/// # Nebenläufigkeit
/// `Send + Sync` — kann sicher hinter `Arc` über Thread-Grenzen geteilt werden.
///
/// # Beispiel
/// ```rust,no_run
/// use std::sync::Arc;
/// use harw_operations::adapter::ModelToolAdapter;
/// use harw_operations::operation::{
///     Operation, OperationMeta, OperationCategory, OperationDomain, PermissionTier, Surface,
///     ApprovalPolicy, BusyAvailability, OpInput, OpOutput, OpFuture,
/// };
/// use harw_operations::context::OpContext;
///
/// struct MyOp;
///
/// impl Operation for MyOp {
///     fn meta(&self) -> &OperationMeta {
///         static META: std::sync::OnceLock<OperationMeta> = std::sync::OnceLock::new();
///         META.get_or_init(|| OperationMeta {
///             name: "my.op",
///             summary: "Beispieloperation.",
///             domain: OperationDomain::Misc,
///             permission: PermissionTier::Observer,
///             surfaces: vec![Surface::ModelTool { readonly: true, approval: ApprovalPolicy::None }],
///             aliases: &[],
///             category: OperationCategory::Misc,
///             args_schema: None,
///             output_schema: None,
///             busy: BusyAvailability::DeferredUntilTurnEnd,
///         })
///     }
///     fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
///         Box::pin(async { Ok(OpOutput::from("ok".to_owned())) })
///     }
/// }
///
/// let adapter = ModelToolAdapter::from_operation(Arc::new(MyOp));
/// assert!(adapter.is_some());
/// ```
pub struct ModelToolAdapter {
    op: Arc<dyn Operation>,
    readonly: bool,
    approval: ApprovalPolicy,
}

impl ModelToolAdapter {
    /// Erstellt einen `ModelToolAdapter`, falls die Operation eine `Surface::ModelTool`-Fläche deklariert.
    ///
    /// # Beschreibung
    /// Durchsucht [`OperationMeta::surfaces`] nach dem ersten `Surface::ModelTool`-Eintrag.
    /// Sind mehrere vorhanden (defensiver Fall), wird die **erste** verwendet.
    ///
    /// # Argumente
    /// - `op` (`Arc<dyn Operation>`): Die zu adapterisierende Operation. Der `Arc` wird intern
    ///   gehalten und per `Arc::clone` geteilt — kein Clone der inneren Daten.
    ///
    /// # Rückgabe
    /// - `Some(ModelToolAdapter)`: Operation hat mindestens eine `Surface::ModelTool`-Fläche.
    /// - `None`: Operation deklariert keine `Surface::ModelTool`-Fläche.
    ///
    /// # Nebenläufigkeit
    /// Sicher von mehreren Threads aufzurufen; keine Sperren benötigt.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use harw_operations::adapter::ModelToolAdapter;
    /// // Adapter ist None, wenn keine ModelTool-Surface deklariert wurde.
    /// ```
    #[must_use]
    pub fn from_operation(op: Arc<dyn Operation>) -> Option<Self> {
        let meta = op.meta();
        let model_tool_surface = meta.surfaces.iter().find_map(|s| match s {
            Surface::ModelTool { readonly, approval } => Some((*readonly, *approval)),
            _ => None,
        })?;

        Some(Self {
            op,
            readonly: model_tool_surface.0,
            approval: model_tool_surface.1,
        })
    }

    /// Gibt `true` zurück, wenn die Operation keine Nebeneffekte hat.
    ///
    /// # Beschreibung
    /// Spiegelt das `readonly`-Flag aus der `Surface::ModelTool`-Deklaration wider.
    /// Downstream-Komponenten (z. B. ein Approval-Gate) können dieses Flag nutzen,
    /// um read-only Tools einer liberaleren Genehmigungsregel zu unterwerfen.
    ///
    /// # Rückgabe
    /// `bool` — `true` = ausschließlich lesend, `false` = Nebeneffekte möglich.
    ///
    /// # Nebenläufigkeit
    /// Reentrant; kein Locking erforderlich.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// # let adapter: harw_operations::adapter::ModelToolAdapter = todo!();
    /// let ro = adapter.readonly();
    /// ```
    #[must_use]
    pub fn readonly(&self) -> bool {
        self.readonly
    }

    /// Gibt die Approval-Politik dieses Tool-Adapters zurück.
    ///
    /// # Beschreibung
    /// Spiegelt das `approval`-Feld aus der `Surface::ModelTool`-Deklaration wider.
    /// [`ApprovalPolicy::None`] bedeutet, das Modell darf direkt ausführen.
    /// [`ApprovalPolicy::Always`] erfordert explizite Nutzer-Bestätigung vor jeder Ausführung.
    ///
    /// # Rückgabe
    /// [`ApprovalPolicy`] — Copy-Typ, kein Borrow nötig.
    ///
    /// # Nebenläufigkeit
    /// Reentrant; kein Locking erforderlich.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_operations::operation::ApprovalPolicy;
    /// # let adapter: harw_operations::adapter::ModelToolAdapter = todo!();
    /// let policy = adapter.approval();
    /// assert_eq!(policy, ApprovalPolicy::None);
    /// ```
    #[must_use]
    pub fn approval(&self) -> ApprovalPolicy {
        self.approval
    }

    /// Gibt den maschinenlesbaren Tool-Namen zurück (entspricht [`OperationMeta::name`]).
    ///
    /// # Beschreibung
    /// Der Tool-Name wird vom LLM verwendet, um das Tool in einem Tool-Call zu referenzieren.
    /// Er entspricht exakt `OperationMeta::name` — keine Transformation.
    ///
    /// # Rückgabe
    /// `&str` mit der Lebensdauer von `&self` (intern statisch über `OnceLock`).
    ///
    /// # Nebenläufigkeit
    /// Reentrant; kein Locking erforderlich.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// # let adapter: harw_operations::adapter::ModelToolAdapter = todo!();
    /// let name = adapter.tool_name();
    /// assert!(!name.is_empty());
    /// ```
    #[must_use]
    pub fn tool_name(&self) -> &str {
        self.op.meta().name
    }

    /// Gibt die kurze Beschreibung der Operation zurück (entspricht [`OperationMeta::summary`]).
    ///
    /// # Beschreibung
    /// Die Beschreibung wird als Tool-Description an das LLM übermittelt und beeinflusst,
    /// wann das Modell dieses Tool wählt. Sie entspricht exakt `OperationMeta::summary`.
    ///
    /// # Rückgabe
    /// `&str` mit der Lebensdauer von `&self` (intern statisch über `OnceLock`).
    ///
    /// # Nebenläufigkeit
    /// Reentrant; kein Locking erforderlich.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// # let adapter: harw_operations::adapter::ModelToolAdapter = todo!();
    /// let desc = adapter.description();
    /// assert!(!desc.is_empty());
    /// ```
    #[must_use]
    pub fn description(&self) -> &str {
        self.op.meta().summary
    }

    /// Führt die Operation mit JSON-Argumenten aus (Modell-Tool-Call).
    ///
    /// # Beschreibung
    /// Baut einen [`OpInput`] mit `raw_args: vec![]` und `json_args: args` und delegiert
    /// an [`Operation::run`]. Die Methode ist die primäre Einstiegsmethode für LLM-initiierte
    /// Tool-Calls und reicht alle Fehler der Operation unverändert weiter.
    ///
    /// # Argumente
    /// - `ctx` (`&OpContext`): Unveränderlicher Ausführungskontext (Session, Turn, Sandbox, Services).
    ///   Muss vom Executor bereitgestellt werden — nie aus dem Tool-Call selbst ableiten.
    /// - `args` (`serde_json::Value`): JSON-Argumente aus dem Modell-Tool-Call.
    ///   Üblicherweise ein `serde_json::Value::Object`. Die Operation ist selbst dafür
    ///   verantwortlich, die Argumente zu validieren.
    ///
    /// # Rückgabe
    /// - `Ok(OpOutput)`: Erfolgreiche Ausführung.
    /// - `Err(OpError)`: Fehler von der Operation.
    ///
    /// # Fehler
    /// - [`OpError::InvalidArguments`]: Die JSON-Argumente sind syntaktisch oder semantisch ungültig.
    /// - [`OpError::Execution`]: Laufzeitfehler während der Ausführung.
    /// - [`OpError::NotAvailable`]: Operation im aktuellen Kontext nicht verfügbar.
    ///
    /// # Nebenläufigkeit
    /// `async fn`, `Send`-fähig. Mehrere gleichzeitige `invoke`-Aufrufe auf demselben
    /// Adapter sind sicher, da kein gemeinsamer veränderlicher Zustand verwendet wird.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_operations::adapter::ModelToolAdapter;
    /// # async fn run(adapter: &ModelToolAdapter, ctx: &harw_operations::context::OpContext) {
    /// let result = adapter.invoke(ctx, serde_json::json!({ "limit": 10 })).await;
    /// match result {
    ///     Ok(out) => println!("{}", out.text),
    ///     Err(e) => eprintln!("Fehler: {e}"),
    /// }
    /// # }
    /// ```
    pub async fn invoke(
        &self,
        ctx: &OpContext,
        args: serde_json::Value,
    ) -> Result<OpOutput, OpError> {
        use tracing::Instrument;
        let json_bytes = args.to_string().len();
        let span = tracing::info_span!(
            "operation.model_tool",
            op = self.tool_name(),
            readonly = self.readonly,
            json_bytes,
        );
        async move {
            let start = std::time::Instant::now();
            let input = OpInput::model_tool(args);
            let result = self.op.run(ctx, input).await;
            tracing::info!(
                duration_ms = start.elapsed().as_millis() as u64,
                status = if result.is_ok() { "ok" } else { "err" },
                "operation.model_tool.done"
            );
            result
        }
        .instrument(span)
        .await
    }

    /// Gibt eine Referenz auf die zugrundeliegende Operation zurück.
    ///
    /// # Beschreibung
    /// Ermöglicht Introspektion der Operation — z. B. für Registrierung, Logging oder
    /// Berechtigungsprüfungen — ohne den Adapter zu konsumieren. Der `Arc` wird nicht
    /// geklont; der Aufrufer erhält eine Referenz auf den intern gespeicherten `Arc`.
    ///
    /// # Rückgabe
    /// `&Arc<dyn Operation>` — Referenz mit der Lebensdauer von `&self`.
    ///
    /// # Nebenläufigkeit
    /// Reentrant; kein Locking erforderlich.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// # let adapter: harw_operations::adapter::ModelToolAdapter = todo!();
    /// let op = adapter.operation();
    /// let name = op.meta().name;
    /// ```
    #[must_use]
    pub fn operation(&self) -> &Arc<dyn Operation> {
        &self.op
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{ModelToolAdapter, ModelToolProvider, model_tool_schema_for};
    use crate::context::{OpContext, ServiceMap};
    use crate::error::OpError;
    use crate::op_schema::{
        OpArgsSchema, described_object_schema, enum_string_schema, string_schema,
    };
    use crate::operation::{
        ApprovalPolicy, ArgsSchemaFn, BusyAvailability, CommandVisibility, OpFuture, OpInput,
        OpOutput, Operation, OperationCategory, OperationDomain, OperationMeta, PermissionTier,
        Surface,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_extension_api::contributors::ToolProvider;
    use harw_tools::{
        AdditionalProperties, JsonSchema, JsonSchemaType, ToolCall, ToolExecutionContext, ToolName,
        ToolOutput, ToolSpec, ToolsError,
    };
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, OnceLock};

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Erstellt einen minimalen `OpContext` für Tests.
    /// Verwendet `AtomicU64` für eindeutige temporäre Verzeichnisse bei Parallel-Tests.
    fn make_test_ctx() -> TestResult<(OpContext, PathBuf)> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CTX_COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = CTX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "harw-model-tool-test-{}-{}",
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(tmp.join("ws"))
            .map_err(ctx("Test-Workspace-Verzeichnis anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new());
        Ok((ctx, tmp))
    }

    // ── Fixture operations ────────────────────────────────────────────────────

    /// Operation ohne jegliche Surface-Deklaration.
    struct NoSurfaceOp;

    impl Operation for NoSurfaceOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "no-surface",
                summary: "Keine Flächen deklariert.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("no-surface-ok".to_owned())) })
        }
    }

    /// Operation mit `Surface::ModelTool { readonly: true, approval: None }`.
    struct ReadonlyModelToolOp;

    impl Operation for ReadonlyModelToolOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "readonly-tool",
                summary: "Lesende Modell-Tool-Operation.",
                domain: OperationDomain::Knowledge,
                permission: PermissionTier::Observer,
                surfaces: vec![Surface::ModelTool {
                    readonly: true,
                    approval: ApprovalPolicy::None,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("readonly-ok".to_owned())) })
        }
    }

    /// Operation mit `Surface::ModelTool { readonly: false, approval: Always }`.
    struct WritingModelToolOp;

    impl Operation for WritingModelToolOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "writing-tool",
                summary: "Schreibende Modell-Tool-Operation.",
                domain: OperationDomain::Execution,
                permission: PermissionTier::Operator,
                surfaces: vec![Surface::ModelTool {
                    readonly: false,
                    approval: ApprovalPolicy::Always,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("writing-ok".to_owned())) })
        }
    }

    /// Operation mit Command- UND ModelTool-Surface (gemischt).
    struct MixedSurfaceOp;

    impl Operation for MixedSurfaceOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "mixed-surface",
                summary: "Command und ModelTool kombiniert.",
                domain: OperationDomain::Session,
                permission: PermissionTier::Maintainer,
                surfaces: vec![
                    Surface::Command {
                        path: "/mixed/run",
                        visibility: CommandVisibility::TuiOnly,
                    },
                    Surface::ModelTool {
                        readonly: true,
                        approval: ApprovalPolicy::None,
                    },
                ],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("mixed-ok".to_owned())) })
        }
    }

    /// Operation, die `json_args["x"]` liest und als Text zurückgibt.
    struct EchoXOp;

    impl Operation for EchoXOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "echo-x",
                summary: "Gibt json_args['x'] als Text zurück.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![Surface::ModelTool {
                    readonly: true,
                    approval: ApprovalPolicy::None,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
            Box::pin(async move {
                let x = input
                    .invocation
                    .json_args()
                    .get("x")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Ok(OpOutput::from(x.to_owned()))
            })
        }
    }

    /// Operation, die immer `OpError::Execution` zurückgibt.
    struct AlwaysErrOp;

    impl Operation for AlwaysErrOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "always-err",
                summary: "Schlägt immer mit Execution-Fehler fehl.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![Surface::ModelTool {
                    readonly: false,
                    approval: ApprovalPolicy::Always,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }
        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Err(OpError::Execution("simulierter Fehler".to_owned())) })
        }
    }

    struct AuthorityEchoOp;

    impl Operation for AuthorityEchoOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "authority-echo",
                summary: "Reports trusted execution authority.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![Surface::ModelTool {
                    readonly: true,
                    approval: ApprovalPolicy::RequireForScope,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async move {
                let marker = ctx.service::<TrustedMarker>().map_or("missing", |v| v.0);
                Ok(OpOutput::from(format!(
                    "{}|{}|{}",
                    ctx.session_id().as_str(),
                    ctx.turn_id().as_str(),
                    marker
                )))
            })
        }
    }

    struct TrustedMarker(&'static str);

    struct NamedModelToolOp {
        meta: OperationMeta,
    }

    impl Operation for NamedModelToolOp {
        fn meta(&self) -> &OperationMeta {
            &self.meta
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("named-model-tool-ok".to_owned())) })
        }
    }

    fn named_model_tool_op(name: &'static str) -> Arc<dyn Operation> {
        named_model_tool_op_with_schema(name, None)
    }

    /// Wie [`named_model_tool_op`], aber mit einem vom Argument-Typ
    /// mitgebrachten Schema — so, wie `#[operation]` es für einen Args-Typ mit
    /// `#[derive(OpArgs)]` setzt.
    fn named_model_tool_op_with_schema(
        name: &'static str,
        args_schema: Option<ArgsSchemaFn>,
    ) -> Arc<dyn Operation> {
        Arc::new(NamedModelToolOp {
            meta: OperationMeta {
                name,
                summary: "Schema test model tool.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![Surface::ModelTool {
                    readonly: true,
                    approval: ApprovalPolicy::None,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            },
        })
    }

    // ── Test 1: from_operation ohne ModelTool-Surface → None ─────────────────

    #[test]
    fn test_from_operation_no_model_tool_surface_returns_none() {
        let op: Arc<dyn Operation> = Arc::new(NoSurfaceOp);
        let adapter = ModelToolAdapter::from_operation(op);
        assert!(
            adapter.is_none(),
            "Erwartet None bei Op ohne ModelTool-Surface"
        );
    }

    // ── Test 2: from_operation mit readonly=true, approval=None → Some ────────

    #[test]
    fn test_from_operation_readonly_true_approval_none_returns_some_with_correct_flags()
    -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(ReadonlyModelToolOp);
        let adapter = ModelToolAdapter::from_operation(op);
        let adapter = adapter.ok_or(TestError::Missing("Some bei Op mit ModelTool-Surface"))?;
        assert!(adapter.readonly(), "readonly sollte true sein");
        assert_eq!(
            adapter.approval(),
            ApprovalPolicy::None,
            "approval sollte None sein"
        );
        Ok(())
    }

    // ── Test 3: from_operation mit readonly=false, approval=Always ────────────

    #[test]
    fn test_from_operation_readonly_false_approval_always_returns_correct_flags() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(WritingModelToolOp);
        let adapter = ModelToolAdapter::from_operation(op)
            .ok_or(TestError::Missing("Some bei Op mit ModelTool-Surface"))?;
        assert!(!adapter.readonly(), "readonly sollte false sein");
        assert_eq!(
            adapter.approval(),
            ApprovalPolicy::Always,
            "approval sollte Always sein"
        );
        Ok(())
    }

    // ── Test 4: from_operation mit gemischten Surfaces → ModelTool-Info wird verwendet

    #[test]
    fn test_from_operation_mixed_surfaces_uses_model_tool_info() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(MixedSurfaceOp);
        let adapter = ModelToolAdapter::from_operation(op)
            .ok_or(TestError::Missing("Some bei Op mit gemischten Surfaces"))?;
        // Die ModelTool-Surface ist readonly=true, approval=None
        assert!(
            adapter.readonly(),
            "readonly sollte true sein (aus ModelTool-Surface)"
        );
        assert_eq!(
            adapter.approval(),
            ApprovalPolicy::None,
            "approval sollte None sein (aus ModelTool-Surface)"
        );
        Ok(())
    }

    // ── Test 5: tool_name() == OperationMeta::name ────────────────────────────

    #[test]
    fn test_tool_name_equals_operation_meta_name() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(ReadonlyModelToolOp);
        let adapter = ModelToolAdapter::from_operation(op).ok_or(TestError::Missing("Some"))?;
        assert_eq!(adapter.tool_name(), "readonly-tool");
        Ok(())
    }

    #[test]
    fn test_tool_name_equals_operation_meta_name_mixed() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(MixedSurfaceOp);
        let adapter = ModelToolAdapter::from_operation(op).ok_or(TestError::Missing("Some"))?;
        assert_eq!(adapter.tool_name(), "mixed-surface");
        Ok(())
    }

    // ── Test 6: description() == OperationMeta::summary ──────────────────────

    #[test]
    fn test_description_equals_operation_meta_summary() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(ReadonlyModelToolOp);
        let adapter = ModelToolAdapter::from_operation(op).ok_or(TestError::Missing("Some"))?;
        assert_eq!(adapter.description(), "Lesende Modell-Tool-Operation.");
        Ok(())
    }

    #[test]
    fn test_description_equals_operation_meta_summary_writing() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(WritingModelToolOp);
        let adapter = ModelToolAdapter::from_operation(op).ok_or(TestError::Missing("Some"))?;
        assert_eq!(adapter.description(), "Schreibende Modell-Tool-Operation.");
        Ok(())
    }

    // ── Test 7: invoke reicht JSON durch ──────────────────────────────────────

    #[tokio::test]
    async fn test_invoke_passes_json_args_to_operation() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(EchoXOp);
        let adapter = ModelToolAdapter::from_operation(op).ok_or(TestError::Missing("Some"))?;
        let (ctx, tmp) = make_test_ctx()?;
        let args = serde_json::json!({ "x": "hallo-welt" });
        let result = adapter.invoke(&ctx, args).await;
        std::fs::remove_dir_all(tmp).ok();
        let Ok(out) = result else {
            return Err(TestError::Unexpected("Erwartet Ok".into()));
        };
        assert_eq!(
            out.text, "hallo-welt",
            "invoke sollte json_args['x'] weitergeben"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_invoke_with_missing_x_key_returns_empty_string() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(EchoXOp);
        let adapter = ModelToolAdapter::from_operation(op).ok_or(TestError::Missing("Some"))?;
        let (ctx, tmp) = make_test_ctx()?;
        let args = serde_json::json!({});
        let result = adapter.invoke(&ctx, args).await;
        std::fs::remove_dir_all(tmp).ok();
        let Ok(out) = result else {
            return Err(TestError::Unexpected("Erwartet Ok".into()));
        };
        assert_eq!(
            out.text, "",
            "invoke ohne 'x'-Schlüssel sollte leeren String liefern"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_invoke_raw_args_are_empty() -> TestResult {
        // Prüft, dass raw_args leer sind (indirekt: EchoXOp ignoriert raw_args,
        // aber eine Op, die raw_args.len() ausgibt, würde 0 zurückgeben).
        struct CountRawArgsOp;
        impl Operation for CountRawArgsOp {
            fn meta(&self) -> &OperationMeta {
                static META: OnceLock<OperationMeta> = OnceLock::new();
                META.get_or_init(|| OperationMeta {
                    name: "count-raw",
                    summary: "Zählt raw_args.",
                    domain: OperationDomain::Misc,
                    permission: PermissionTier::Observer,
                    surfaces: vec![Surface::ModelTool {
                        readonly: true,
                        approval: ApprovalPolicy::None,
                    }],
                    aliases: &[],
                    category: OperationCategory::Misc,
                    args_schema: None,
                    output_schema: None,
                    busy: BusyAvailability::DeferredUntilTurnEnd,
                })
            }
            fn run<'a>(&'a self, _ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
                Box::pin(async move {
                    Ok(OpOutput::from(
                        input.invocation.raw_args().len().to_string(),
                    ))
                })
            }
        }

        let op: Arc<dyn Operation> = Arc::new(CountRawArgsOp);
        let adapter = ModelToolAdapter::from_operation(op).ok_or(TestError::Missing("Some"))?;
        let (ctx, tmp) = make_test_ctx()?;
        let result = adapter.invoke(&ctx, serde_json::Value::Null).await;
        std::fs::remove_dir_all(tmp).ok();
        let Ok(out) = result else {
            return Err(TestError::Unexpected("Erwartet Ok".into()));
        };
        assert_eq!(out.text, "0", "raw_args müssen bei invoke leer sein");
        Ok(())
    }

    // ── Test 8: invoke propagiert OpError::Execution ─────────────────────────

    #[tokio::test]
    async fn test_invoke_propagates_execution_error() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(AlwaysErrOp);
        let adapter = ModelToolAdapter::from_operation(op).ok_or(TestError::Missing("Some"))?;
        let (ctx, tmp) = make_test_ctx()?;
        let result = adapter.invoke(&ctx, serde_json::Value::Null).await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Err(OpError::Execution(msg)) => {
                assert_eq!(
                    msg, "simulierter Fehler",
                    "Fehlermeldung sollte weitergeleitet werden"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Erwartet OpError::Execution, war: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── Zusatz: operation() liefert Zugriff auf die zugrunde liegende Op ──────

    #[test]
    fn test_operation_accessor_returns_arc_with_correct_meta() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(ReadonlyModelToolOp);
        let adapter =
            ModelToolAdapter::from_operation(Arc::clone(&op)).ok_or(TestError::Missing("Some"))?;
        assert_eq!(adapter.operation().meta().name, "readonly-tool");
        Ok(())
    }

    // ── Compile-Zeit: ModelToolAdapter ist Send + Sync ────────────────────────

    #[test]
    fn test_model_tool_adapter_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ModelToolAdapter>();
    }

    // ── model_tool_schema_for ─────────────────────────────────────────────────

    /// Argument-Typ einer Planungs-Operation, wie ihn `#[derive(OpArgs)]`
    /// erzeugen würde. `harw-operations` hängt nicht von `harw-macros` ab, also
    /// steht die Trait-Implementierung hier von Hand — geprüft wird das Derive
    /// selbst in `harw-macros`.
    struct PlanArgs;

    impl OpArgsSchema for PlanArgs {
        fn json_schema() -> JsonSchema {
            described_object_schema(
                "Subcommand-Aufruf: `action` wählt den Subcommand.",
                vec![
                    (
                        "action",
                        enum_string_schema("Subcommand.", &["show", "add"]),
                    ),
                    ("title", string_schema("Titel des Plans.")),
                ],
                &["action"],
            )
        }
    }

    /// Baut ein Schema, wie es `#[derive(OpArgs)]` für ein Subcommand-Enum liefert.
    fn custom_plan_schema() -> JsonSchema {
        <PlanArgs as OpArgsSchema>::json_schema()
    }

    #[test]
    fn test_model_tool_schema_for_status_without_fallback_returns_known_schema() -> TestResult {
        let schema = model_tool_schema_for("status", None);

        assert_eq!(schema.schema_type, Some(JsonSchemaType::Object));
        assert_eq!(
            schema.additional_properties,
            Some(Box::new(AdditionalProperties::Bool(false)))
        );
        assert_eq!(schema.required, None);
        assert!(
            schema
                .properties
                .ok_or(TestError::Missing(
                    "status behält sein geschlossenes Schema"
                ))?
                .is_empty(),
            "status hat keine Argumente"
        );
        Ok(())
    }

    #[test]
    fn test_model_tool_schema_for_stop_without_fallback_returns_known_schema() -> TestResult {
        let schema = model_tool_schema_for("stop", None);

        let properties = schema
            .properties
            .ok_or(TestError::Missing("stop behält sein Schema"))?;
        assert_eq!(
            properties.get("job_id").map(|s| &s.schema_type),
            Some(&Some(JsonSchemaType::String))
        );
        Ok(())
    }

    #[test]
    fn test_model_tool_schema_for_plan_with_fallback_returns_custom_schema() {
        let schema = model_tool_schema_for("plan", Some(custom_plan_schema()));

        assert_eq!(schema, custom_plan_schema(), "das Fallback-Schema gewinnt");
        assert_eq!(schema.required, Some(vec!["action".to_owned()]));
        assert_eq!(
            schema.additional_properties,
            Some(Box::new(AdditionalProperties::Bool(false)))
        );
    }

    #[test]
    fn test_model_tool_schema_for_known_name_with_fallback_prefers_fallback() {
        let schema = model_tool_schema_for("status", Some(custom_plan_schema()));

        assert_eq!(
            schema,
            custom_plan_schema(),
            "ein mitgebrachtes Schema schlägt den Namens-match"
        );
    }

    #[test]
    fn test_model_tool_schema_for_unknown_without_fallback_returns_open_object() {
        let schema = model_tool_schema_for("future-tool", None);

        assert_eq!(schema.schema_type, Some(JsonSchemaType::Object));
        assert_eq!(
            schema.additional_properties,
            Some(Box::new(AdditionalProperties::Bool(true))),
            "Rückwärtskompatibilität: unbekannte Tools behalten den offenen Fallback"
        );
        assert!(schema.properties.is_none());
    }

    // ── args_schema hat Vorrang vor dem Namens-`match` ────────────────────────

    /// Baut einen Provider über genau eine Operation und gibt das Schema
    /// zurück, das er für sie veröffentlicht.
    ///
    /// Der `context_factory`-Abschluss darf laut Vertrag der Schema-Entdeckung
    /// (`ToolProvider::tools`) nie aufgerufen werden. Da die Abschlusssignatur
    /// `Fn(&ToolExecutionContext) -> OpContext` (aus `ModelToolProvider::new`,
    /// Produktionscode) kein `Result` zulässt, dient statt einer Panik ein
    /// `AtomicBool`-Kanarienvogel: der Abschluss setzt ihn und liefert einen
    /// neutralen `OpContext` aus der übergebenen Authority zurück; anschließend
    /// prüft ein `assert!`, dass der Kanarienvogel nie gesetzt wurde.
    fn published_parameters(op: Arc<dyn Operation>) -> TestResult<JsonSchema> {
        let name = op.meta().name;
        let factory_called = Arc::new(AtomicBool::new(false));
        let observed_called = Arc::clone(&factory_called);
        let provider = ModelToolProvider::new(vec![op], move |authority| -> OpContext {
            observed_called.store(true, Ordering::Relaxed);
            OpContext::new(
                authority.session_id().clone(),
                authority.turn_id().clone(),
                authority.sandbox().clone(),
                ServiceMap::new(),
            )
        });
        let specs = provider.tools();
        assert!(
            !factory_called.load(Ordering::Relaxed),
            "schema discovery must not construct execution authority"
        );
        let Some(spec) = specs.iter().find(|spec| spec.name() == name) else {
            return Err(TestError::Unexpected(format!(
                "Tool `{name}` muss veröffentlicht werden"
            )));
        };
        let ToolSpec::Function(function) = spec;
        Ok(function.parameters.clone())
    }

    #[test]
    fn provider_publishes_closed_schema_for_operation_with_args_schema() -> TestResult {
        let op =
            named_model_tool_op_with_schema("plan", Some(<PlanArgs as OpArgsSchema>::json_schema));
        let parameters = published_parameters(op)?;

        assert_eq!(
            parameters,
            custom_plan_schema(),
            "der Provider muss das Schema des Argument-Typs unverändert veröffentlichen"
        );
        assert_eq!(
            parameters.additional_properties,
            Some(Box::new(AdditionalProperties::Bool(false))),
            "ein OpArgs-Schema ist geschlossen — kein offenes Objekt"
        );
        assert_eq!(parameters.required, Some(vec!["action".to_owned()]));
        let Some(properties) = parameters.properties else {
            return Err(TestError::Missing(
                "ein geschlossenes Schema nennt seine erlaubten Felder",
            ));
        };
        assert!(properties.contains_key("action"));
        assert!(
            properties.contains_key("title"),
            "das Modell muss die Feldnamen erfahren statt sie zu raten"
        );
        Ok(())
    }

    #[test]
    fn operation_meta_args_schema_is_the_source_of_the_published_schema() -> TestResult {
        let op =
            named_model_tool_op_with_schema("plan", Some(<PlanArgs as OpArgsSchema>::json_schema));
        let Some(build_schema) = op.meta().args_schema else {
            return Err(TestError::Missing(
                "die Operation trägt ein Argument-Schema",
            ));
        };

        assert_eq!(
            build_schema(),
            published_parameters(Arc::clone(&op))?,
            "veröffentlicht wird exakt `meta.args_schema`"
        );
        Ok(())
    }

    #[test]
    fn provider_prefers_args_schema_over_hardcoded_name_match() -> TestResult {
        // `stop` hat einen bekannten `match`-Arm — das mitgebrachte Schema
        // muss ihn trotzdem schlagen.
        let op =
            named_model_tool_op_with_schema("stop", Some(<PlanArgs as OpArgsSchema>::json_schema));
        let parameters = published_parameters(op)?;

        assert_eq!(parameters, custom_plan_schema());
        let Some(properties) = parameters.properties else {
            return Err(TestError::Missing(
                "ein geschlossenes Schema nennt seine erlaubten Felder",
            ));
        };
        assert!(
            !properties.contains_key("job_id"),
            "der Namens-`match` darf ein mitgebrachtes Schema nicht überschreiben"
        );
        Ok(())
    }

    #[test]
    fn provider_falls_back_to_name_match_without_args_schema() -> TestResult {
        // Eine Operation ohne `OpArgs` behält ihr bekanntes Schema, statt auf
        // das offene Objekt zu fallen.
        let parameters = published_parameters(named_model_tool_op("stop"))?;

        assert_eq!(
            parameters.additional_properties,
            Some(Box::new(AdditionalProperties::Bool(false)))
        );
        let Some(properties) = parameters.properties else {
            return Err(TestError::Missing(
                "`stop` behält sein geschlossenes Schema",
            ));
        };
        assert_eq!(
            properties.get("job_id").map(|schema| &schema.schema_type),
            Some(&Some(JsonSchemaType::String))
        );
        Ok(())
    }

    #[test]
    fn provider_keeps_open_object_for_unknown_operation_without_args_schema() -> TestResult {
        let parameters = published_parameters(named_model_tool_op("future-tool"))?;

        assert_eq!(parameters.schema_type, Some(JsonSchemaType::Object));
        assert_eq!(
            parameters.additional_properties,
            Some(Box::new(AdditionalProperties::Bool(true))),
            "dokumentiertes Rückfallverhalten bleibt unverändert"
        );
        assert!(parameters.properties.is_none());
        Ok(())
    }

    #[test]
    fn provider_publishes_derived_and_name_matched_schemas_side_by_side() {
        let operations: Vec<Arc<dyn Operation>> = vec![
            named_model_tool_op_with_schema("plan", Some(<PlanArgs as OpArgsSchema>::json_schema)),
            named_model_tool_op("stop"),
            named_model_tool_op("future-tool"),
        ];
        let factory_called = Arc::new(AtomicBool::new(false));
        let observed_called = Arc::clone(&factory_called);
        let provider = ModelToolProvider::new(operations, move |authority| -> OpContext {
            observed_called.store(true, Ordering::Relaxed);
            OpContext::new(
                authority.session_id().clone(),
                authority.turn_id().clone(),
                authority.sandbox().clone(),
                ServiceMap::new(),
            )
        });

        let specs = provider.tools();
        assert!(
            !factory_called.load(Ordering::Relaxed),
            "schema discovery must not construct execution authority"
        );
        assert_eq!(specs.len(), 3);

        let closed_count = specs
            .iter()
            .filter(|spec| {
                let ToolSpec::Function(function) = spec;
                function.parameters.additional_properties
                    == Some(Box::new(AdditionalProperties::Bool(false)))
            })
            .count();
        assert_eq!(
            closed_count, 2,
            "`plan` (OpArgs) und `stop` (Namens-`match`) sind geschlossen, \
             nur `future-tool` bleibt offen"
        );
    }

    #[test]
    fn provider_filters_surfaces_and_emits_exact_or_compatible_schemas() -> TestResult {
        let operations: Vec<Arc<dyn Operation>> = vec![
            Arc::new(NoSurfaceOp),
            named_model_tool_op("status"),
            named_model_tool_op("ps"),
            named_model_tool_op("diff"),
            named_model_tool_op("stop"),
            named_model_tool_op("future-tool"),
        ];
        let factory_called = Arc::new(AtomicBool::new(false));
        let observed_called = Arc::clone(&factory_called);
        let provider = ModelToolProvider::new(operations, move |authority| -> OpContext {
            observed_called.store(true, Ordering::Relaxed);
            OpContext::new(
                authority.session_id().clone(),
                authority.turn_id().clone(),
                authority.sandbox().clone(),
                ServiceMap::new(),
            )
        });

        let specs = provider.tools();
        assert!(
            !factory_called.load(Ordering::Relaxed),
            "schema discovery must not construct execution authority"
        );
        assert_eq!(specs.len(), 5);

        let expected_properties = [
            ("status", &[][..]),
            (
                "ps",
                &[
                    ("status", JsonSchemaType::String),
                    ("kind", JsonSchemaType::String),
                ][..],
            ),
            (
                "diff",
                &[
                    ("path", JsonSchemaType::String),
                    ("stat_only", JsonSchemaType::Boolean),
                ][..],
            ),
            ("stop", &[("job_id", JsonSchemaType::String)][..]),
        ];

        for (name, expected_properties) in expected_properties {
            let spec = specs
                .iter()
                .find(|spec| spec.name() == name)
                .ok_or(TestError::Missing("known model tool must be published"))?;
            let ToolSpec::Function(function) = spec;
            assert!(!function.strict);
            assert_eq!(
                function.parameters.schema_type,
                Some(JsonSchemaType::Object)
            );
            assert_eq!(
                function.parameters.additional_properties,
                Some(Box::new(AdditionalProperties::Bool(false)))
            );
            assert_eq!(function.parameters.required, None);
            let properties = function
                .parameters
                .properties
                .as_ref()
                .ok_or(TestError::Missing(
                    "closed schema must declare its allowed properties",
                ))?;
            assert_eq!(properties.len(), expected_properties.len());
            for (property_name, property_type) in expected_properties {
                assert_eq!(
                    properties
                        .get(*property_name)
                        .map(|schema| &schema.schema_type),
                    Some(&Some(property_type.clone()))
                );
            }
        }

        let fallback = specs
            .iter()
            .find(|spec| spec.name() == "future-tool")
            .ok_or(TestError::Missing("future model tool must be published"))?;
        let ToolSpec::Function(fallback) = fallback;
        assert_eq!(
            fallback.parameters.schema_type,
            Some(JsonSchemaType::Object)
        );
        assert_eq!(
            fallback.parameters.additional_properties,
            Some(Box::new(AdditionalProperties::Bool(true)))
        );
        assert!(fallback.parameters.properties.is_none());
        assert!(provider.executor(&ToolName::new("no-surface")).is_none());
        assert_eq!(
            provider.approval_policy(&ToolName::new("stop")),
            Some(ApprovalPolicy::None)
        );
        Ok(())
    }

    #[tokio::test]
    async fn provider_propagates_only_factory_derived_authority() -> TestResult {
        let (trusted_ctx, tmp) = make_test_ctx()?;
        let execution_context = ToolExecutionContext::new(
            trusted_ctx.session_id().clone(),
            trusted_ctx.turn_id().clone(),
            trusted_ctx.sandbox().clone(),
        );
        let expected = format!(
            "{}|{}|trusted",
            execution_context.session_id().as_str(),
            execution_context.turn_id().as_str()
        );
        let provider = ModelToolProvider::new(
            vec![Arc::new(AuthorityEchoOp) as Arc<dyn Operation>],
            |authority| {
                let mut services = ServiceMap::new();
                services.insert(TrustedMarker("trusted"));
                OpContext::new(
                    authority.session_id().clone(),
                    authority.turn_id().clone(),
                    authority.sandbox().clone(),
                    services,
                )
            },
        );
        assert_eq!(
            provider.approval_policy(&ToolName::new("authority-echo")),
            Some(ApprovalPolicy::RequireForScope)
        );
        let executor = provider
            .executor(&ToolName::new("authority-echo"))
            .ok_or(TestError::Missing("model tool executor exists"))?;
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("authority-echo"),
            arguments: serde_json::json!({
                "session_id": "model-controlled",
                "turn_id": "model-controlled",
                "sandbox": { "permissions": ["everything"] },
                "services": { "marker": "untrusted" }
            }),
        };

        let output = executor
            .execute(&execution_context, &call)
            .await
            .map_err(ctx("operation succeeds"))?;
        std::fs::remove_dir_all(tmp).ok();
        match output {
            ToolOutput::Text { content } => assert_eq!(content, expected),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn provider_rejects_non_object_arguments_before_context_creation() -> TestResult {
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = Arc::clone(&factory_calls);
        let provider = ModelToolProvider::new(
            vec![Arc::new(ReadonlyModelToolOp) as Arc<dyn Operation>],
            move |authority| -> OpContext {
                observed_calls.fetch_add(1, Ordering::Relaxed);
                OpContext::new(
                    authority.session_id().clone(),
                    authority.turn_id().clone(),
                    authority.sandbox().clone(),
                    ServiceMap::new(),
                )
            },
        );
        let executor = provider
            .executor(&ToolName::new("readonly-tool"))
            .ok_or(TestError::Missing("model tool executor exists"))?;
        let (op_ctx, tmp) = make_test_ctx()?;
        let execution_context = ToolExecutionContext::new(
            op_ctx.session_id().clone(),
            op_ctx.turn_id().clone(),
            op_ctx.sandbox().clone(),
        );
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("readonly-tool"),
            arguments: serde_json::json!(["not", "an", "object"]),
        };

        let result = executor.execute(&execution_context, &call).await;
        std::fs::remove_dir_all(tmp).ok();
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "array arguments must be rejected".into(),
            ));
        };
        assert_eq!(factory_calls.load(Ordering::Relaxed), 0);
        match error {
            ToolsError::InvalidArguments { name, reason } => {
                assert_eq!(name, "readonly-tool");
                assert!(reason.contains("JSON object"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected typed invalid-arguments error, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn provider_converts_operation_error_to_typed_tools_error() -> TestResult {
        let provider = ModelToolProvider::new(
            vec![Arc::new(AlwaysErrOp) as Arc<dyn Operation>],
            |authority| {
                OpContext::new(
                    authority.session_id().clone(),
                    authority.turn_id().clone(),
                    authority.sandbox().clone(),
                    ServiceMap::new(),
                )
            },
        );
        let executor = provider
            .executor(&ToolName::new("always-err"))
            .ok_or(TestError::Missing("model tool executor exists"))?;
        let (op_ctx, tmp) = make_test_ctx()?;
        let execution_context = ToolExecutionContext::new(
            op_ctx.session_id().clone(),
            op_ctx.turn_id().clone(),
            op_ctx.sandbox().clone(),
        );
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("always-err"),
            arguments: serde_json::json!({}),
        };

        let result = executor.execute(&execution_context, &call).await;
        std::fs::remove_dir_all(tmp).ok();
        let Err(error) = result else {
            return Err(TestError::Unexpected("operation must fail".into()));
        };
        match error {
            ToolsError::ExecutionFailed(message) => {
                assert!(message.contains("simulierter Fehler"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected typed execution error, got {other:?}"
                )));
            }
        }
        Ok(())
    }
}
