//! Turn-Loop: der eigentliche Agent-Zyklus.
//!
//! Schritt für Schritt:
//! 1. Context sammeln (ContextProvider)
//! 2. Instructions laden (InstructionsProvider)
//! 3. Observer benachrichtigen (TurnObserver::on_turn_start)
//! 4. Model Call (provider-neutral, mit Kontextfenster-Wächter — siehe
//!    „Neunter Nachtrag")
//! 5. Für jeden ToolCall:
//!    a. Guardrail (ApprovalHandler)
//!    a2. Zulassung beim [`ToolCallMeter`] (falls am [`TurnControl`]
//!    registriert): bucht den Aufruf, bevor irgendetwas davon läuft, und
//!    liefert einen [`ToolCallPermit`] — die Dispatcher-Seite des
//!    Lease-Zauns. Der Loop hält ihn, bis das Ergebnis genau dieses Aufrufs
//!    feststeht (b/c: Kind gespawnt und übergeben, Werkzeug ausgeführt,
//!    lokale Antwort erzeugt), und lässt ihn dann fallen: sequenziell vor
//!    der nächsten Zulassung, parallel je Job nach dessen eigener
//!    Ausführung, in jedem Fall vor der nächsten Modellrunde. Solange er
//!    lebt, hält der Guard des Meters (falls einer drinsteckt) die Lease:
//!    ein Reclaim läuft nie zwischen Buchung und Seiteneffekt, sondern wird
//!    über den Dispatch serialisiert. Was der Guard ist, entscheidet der
//!    Meter (siehe [`crate::tool_call_meter`]).
//!    b. Handoff? → AgentSpawner → WaitingForChild
//!    c. Sonst: ToolExecutor::execute
//!    d. ToolResult in History
//! 6. Keine ToolCalls mehr → Turn fertig
//! 7. Observer: on_turn_stop, state → Idle
//!
//! # Tracing
//!
//! Every public entry point (run_turn, run_turn_durable, resume_after_child,
//! resume_after_child_durable, resume_after_approval, resume_after_approval_durable)
//! is wrapped in an `agent.turn` span carrying `turn_id` and `session_id`.
//!
//! **Redaction policy**: no prompt text, no tool arguments JSON, no response
//! content is ever logged. Only IDs, names, byte-sizes, counters, and status
//! strings ("ok" | "err") appear in structured fields.
//!
//! Span/event hierarchy per turn:
//! ```text
//! agent.turn { turn_id, session_id }
//! ├─ model.request  { size_bytes }          ← info event
//! ├─ model.response { size_bytes, tool_call_count }   ← info event
//! ├─ tool.call      { tool_name }           ← info span
//! │    └─ tool.execute { duration_ms, status } ← info event
//! ├─ approval.wait  { approval_kind }       ← info span
//! └─ transcript.persist { records_written } ← debug event
//! ```
//!
//! # W4a A-LOOP: Abbruch, Grenzwerte, Vertrauen, Stop-Gründe
//!
//! - **Steuerblock je Turn.** [`TurnControl`] trägt den [`CancelToken`], die
//!   [`TurnLimits`], die Serveruhr ([`harw_types::Clock`]) und einen geteilten
//!   Zähler (Modellrunden, Werkzeugaufrufe, Token-Nutzung, Startzeit). Er reist
//!   in [`TurnInput::control`] in `run_turn`/`run_turn_durable` hinein; die
//!   `resume_*`-Einstiege bekommen **denselben** Block als `&TurnControl`, damit
//!   Grenzwerte und Nutzung über eine Pause hinweg weiterzählen.
//! - **Prüfpunkte.** Vor jedem Modellaufruf (Abbruch, Runden, Ausgabe-Tokens,
//!   Wanduhr) und vor jeder Werkzeugausführung (Abbruch, Aufrufzahl, Wanduhr).
//!   Der Modellaufruf selbst läuft gegen `CancelToken::cancelled` (kein Timer
//!   nötig). Ein Treffer beendet den Turn mit [`TurnOutcome::Cancelled`]
//!   (`CancelReason::Budget` bei einer Grenzwertverletzung); jeder noch offene
//!   Tool-Call der Runde bekommt ein synthetisches Fehlerergebnis mit
//!   `ResultTrust::Runtime`, damit der Verlauf provider-gültig bleibt.
//! - **Vertrauen.** Werkzeugausgaben landen mit `ResultTrust::Untrusted` im
//!   Verlauf; vom Harness erzeugte Ergebnisse (Ablehnung, Abbruch, fehlender
//!   Kontext, fehlender Ausführer, deaktivierter Handoff) mit
//!   `ResultTrust::Runtime`.
//! - **Stop-Gründe.** Bei `StopReason::MaxTokens` fordert der Loop höchstens
//!   drei Mal automatisch eine nahtlose Fortsetzung an. Bleibt die Ausgabe
//!   danach abgeschnitten, sowie sofort bei `ContextWindowExceeded`, endet der
//!   Turn mit [`TurnOutcome::Truncated`]. `StopReason::{Refusal,
//!   ContentFilter}` endet mit [`TurnOutcome::Refused`]. Tool-Calls einer
//!   solchen Antwort werden nie ausgeführt (Argumente können abgeschnitten
//!   sein). Für jede Session (Root wie Kind): trägt `response.reasoning`
//!   lesbaren Denktext (Anthropic-`"thinking"`, OpenAI-Reasoning-Summary,
//!   `reasoning_content`), wird dieser Text als `TurnItem::Reasoning` (VOR der
//!   AssistantMessage) in die History gepusht, persistiert und per
//!   `TurnEvent::ItemAdded` gemeldet. `"redacted_thinking"`- und
//!   verschlüsseltes OpenAI-Reasoning liefern keinen extrahierbaren Text und
//!   bleiben unsichtbar. Modell-Input wird das Item nie:
//!   `ConversationHistory::to_model_messages` (`history.rs`) überspringt
//!   `Reasoning`-Items bedingungslos.
//! - **Resume-Fehler.** Eine abgelehnte Wiederaufnahme (falscher Actor, falsches
//!   Kind, keine offene Anfrage, bereits aufgelöst) bleibt `Err` und lässt die
//!   Pause intakt. Scheitert eine *angenommene* Wiederaufnahme (Persistenz,
//!   Spawner, Ausführungsgrenze) oder ist die dauerhafte Freigabe abgelaufen
//!   bzw. defekt, endet der Turn mit [`TurnOutcome::Failed`] (Session `Failed`).
//! - **Handoffs** (`transfer_to_*`): Für jedes vom Spawner gemeldete
//!   Delegationsziel bietet `drive_turn` eine echte Werkzeugdefinition
//!   `transfer_to_<role>` (`{"task", "context"?}`) an, sofern die Aktivierung
//!   sie nicht ausdrücklich abschaltet. Beim Aufruf wird
//!   `SpawnInput::instructions` aus `task`/`instructions`/`objective`/
//!   `question` (plus optionalem `context`) bzw. dem Argument-JSON gefüllt.
//!   Plan R9, Teil C: bei mehr als acht Zielen ersetzt das eine Werkzeug
//!   `agents.delegate {agent, …}` die Einzelwerkzeuge (derselbe Spawn-Weg);
//!   `agents.catalog` beschreibt die Ziele. Im Plan-Modus bleiben nur
//!   lesende Ziele delegierbar (E1), ein anderes Ziel bekommt eine benannte
//!   Ablehnung als Werkzeugergebnis.
//!
//! # AW1-03: warum Schritt 1 noch der alte Pfad ist
//!
//! [`gather_context`] (Schritt 1) liefert bis heute
//! `Vec<harw_extension_api::ContextFragment>` — nur `label` + `content`,
//! ohne Sektion, `TrustClass`, `Stability` oder Kosten. `drive_turn` reicht
//! das unverändert an `ModelRequest::with_context_budget` weiter, das intern
//! die **Bestandsmontage** aus `crate::context_budget` (`ContextBudget`,
//! `ContextAssembly`, `assemble`) aufruft.
//!
//! Der neue, `ContextProgram`-bewusste, deterministisch geordnete Pfad aus
//! diesem Knoten — `context_budget::Assembly<Gathered|Admitted|Budgeted>`,
//! der `harw_context::Fragment` statt `ContextFragment` voraussetzt — ist
//! **absichtlich nicht** hier verdrahtet: es gibt noch keinen
//! `ContextProvider`, der ein `harw_context::Fragment` (mit Sektion,
//! Vertrauensklasse, Stabilität, bereits berechneten Kosten) produziert.
//! Eine Verdrahtung an dieser Stelle hätte bedeutet, diese fünf Angaben pro
//! Fragment zu erfinden — das wäre keine Durchsetzung gewesen, nur ihr
//! Anschein. Sobald ein Fragment-Provider existiert, ersetzt
//! `context_budget::Assembly::gather(fragments).admit(&program,
//! &ceiling)?.budget(&spec)?.render()` diesen Schritt.
//!
//! ## Nachtrag (Folgeknoten): eine der beiden Lücken hat sich seither geschlossen
//!
//! Eigene Prüfung von `crate::session`, nicht Übernahme des obigen Absatzes:
//! `SpawnContext` (`session.rs`) trägt inzwischen ein
//! `ceiling: Option<harw_context::ContextCeiling>`, erreichbar über
//! `session.spawn_context().and_then(|c| c.ceiling.clone())`. Eine
//! `ContextCeiling` ist damit produktiv vorhanden. **Kein** `ContextProgram`
//! ist es weiterhin: weder `AgentSession` noch `SpawnContext` tragen eines,
//! und `harw_agent_dsl::executable::ContextProgram` selbst hat (eigene
//! Prüfung von `harw-agent-dsl/src/executable.rs`) kein Feld für einen
//! `harw_context::DetailMode` je Sektion — nur `must_include`/`exclude`.
//! Der ursprüngliche Blocker "fünf Angaben pro Fragment erfinden" (Absatz
//! oben) bleibt deshalb unverändert bestehen: eine Ceiling allein liefert
//! weder Sektion noch Vertrauen noch Stabilität noch Kosten eines Fragments
//! — nur eine Erlaubnisgrenze dafür. Siehe `crate::context_budget`s
//! Modul-Abschnitt "Die nachgeholte Verdrahtung" für die vollständige,
//! aktuelle Bestandsaufnahme aller drei noch offenen Voraussetzungen
//! (Fragment-Provider, `ContextProgram` je Sitzung, ein Weg von hier zu
//! `harw_tools::context_load::ContextLoadExecutor::seed_turn`) sowie für die
//! beiden additiven Bausteine, die dieser Folgeknoten dafür bereits in
//! `context_budget.rs` bereitgestellt hat
//! (`ContextAssemblyV2::render_trust_blocks_with_detail`,
//! `ContextAssemblyV2::spent_per_section`) — beide ungenutzt von dieser
//! Datei, aus genau den hier genannten Gründen, nicht aus Versehen.
//!
//! ## Zweiter Nachtrag (dieser Knoten): `ContextProgram` ist jetzt erreichbar,
//! `DetailMode` auch — zwei der vier Lücken sind kleiner geworden, keine ist ganz zu
//!
//! Eigene Prüfung dieses Knotens gegen den obigen Absatz:
//!
//! 1. **`ContextProgram` je Sitzung ist jetzt erreichbar.**
//!    [`crate::session::AgentSession`] trägt seit diesem Knoten
//!    `context_program: Option<harw_agent_dsl::executable::ContextProgram>`
//!    über `AgentSession::with_context_program`/`AgentSession::context_program`
//!    (bewusst auf `AgentSession`, nicht auf [`SpawnContext`] — Begründung in
//!    `session.rs`s Moduldoku, Abschnitt "Folgeknoten zu AW2-01/AW2-02"). Eine
//!    Sitzung ohne diesen Aufruf verhält sich unverändert: `gather_context`
//!    unten liest dieses Feld nicht, also ändert seine bloße Existenz kein
//!    bestehendes Rendering.
//! 2. **`harw_agent_dsl::executable::ContextProgram` trägt jetzt den
//!    [`harw_context::DetailMode`] je Sektion.** `ContextProgram::section_detail`
//!    (Folgeknoten zu AW2-01) schließt genau die Lücke, die der Absatz oben
//!    ("kein Feld für einen `harw_context::DetailMode` je Sektion") noch offen
//!    ließ — `SNAPSHOT_HASH_DOMAIN` in `executable.rs` ist deshalb auf `v3`
//!    gestiegen.
//! 3. **`gather_context` (Schritt 1 unten) ist unverändert der alte Pfad.**
//!    Es liefert weiterhin `Vec<harw_extension_api::ContextFragment>`, nicht
//!    `harw_context::Fragment`. Diese Datei besitzt keinen `ContextProvider`
//!    — die Trait-Definition und ihre Implementierungen liegen in
//!    `harw-extension-api`/den jeweiligen Provider-Crates, außerhalb des
//!    Schreibbereichs dieses Knotens (`harw-agent-dsl/src/executable.rs`,
//!    `harw-core/src/session.rs`, `harw-core/src/turn_loop.rs`). Der
//!    ursprüngliche Blocker "kein `ContextProvider` erzeugt ein
//!    `harw_context::Fragment`" bleibt deshalb unverändert bestehen — **hier**
//!    bricht die Kette zu `FragmentReference`/`DetailMode::References`
//!    weiterhin, nicht erst bei der Montage.
//! 4. **Kein Weg von hier zu einer konkreten `ContextLoadExecutor`-Instanz.**
//!    [`find_executor`] liefert ausschließlich `Arc<dyn ToolExecutor>`
//!    (`harw_tools::executor::ToolExecutor`, definiert in
//!    `harw-tools/src/executor.rs:64`) — dieses Trait-Objekt bietet keine
//!    Möglichkeit, das konkrete `harw_tools::context_load::ContextLoadExecutor`
//!    dahinter zu erreichen. Geprüft und verworfen:
//!      - eine neue Methode auf `ToolExecutor` selbst (z. B.
//!        `fn as_context_load_executor(&self) -> Option<&ContextLoadExecutor> { None }`
//!        als Default-Methode in `harw-tools/src/executor.rs`, überschrieben
//!        in `impl ToolExecutor for ContextLoadExecutor`,
//!        `harw-tools/src/context_load.rs:539`) — berührt `harw-tools`, das
//!        nicht zum Schreibbereich dieses Knotens gehört;
//!      - ein `downcast` über `std::any::Any` in dieser Datei — technisch
//!        machbar (`ToolExecutor: Send + Sync` bräuchte zusätzlich
//!        `+ Any` bzw. eine `as_any`-Methode, ebenfalls eine Änderung in
//!        `harw-tools`), aber eine Typprüfung zur Laufzeit an einer Stelle,
//!        die sie mit der ersten Option nicht bräuchte — deshalb hier bewusst
//!        **nicht** eingebaut, um niemanden mit einem `downcast` zurückzulassen,
//!        den ein Folgeknoten erst wieder entfernen müsste.
//!
//!    Ein dritter, in diesem Schreibbereich baubarer Weg existiert nicht: jede
//!    Alternative läuft über die `Arc<dyn ToolExecutor>`-Grenze, die
//!    ausschließlich `harw-tools` definiert. Das ist der Befund für Punkt 3
//!    des Knotenauftrags — die obige Zeile ist die wörtliche Ergänzung, die in
//!    `harw-tools/src/executor.rs` nötig wäre.
//!
//! ## Dritter Nachtrag (dieser Knoten): Blocker 3 ist geschlossen — die Grenze
//! war der Schreibbereich, nicht die Machbarkeit
//!
//! `harw-tools/src/executor.rs` gehört jetzt zum Schreibbereich. Gebaut wurde
//! genau die Zeile, die Punkt 4 oben als Befund nannte:
//! `ToolExecutor::as_context_load_executor` als Default-Methode (`None`),
//! überschrieben in `impl ToolExecutor for ContextLoadExecutor`
//! (`harw-tools/src/context_load.rs`). Kein bestehender `ToolExecutor`
//! bricht dadurch — die Vorgabe ist dafür da. Kein `downcast` — aus demselben
//! Grund, den Punkt 4 oben bereits nannte.
//!
//! [`seed_context_load_ledger`] nutzt diesen Weg von einem echten
//! Eintrittspunkt aus: [`drive_turn`] ruft sie einmal je Turn auf, bevor die
//! Model-/Tool-Schleife beginnt. Sie erreicht **beide** Hälften — das
//! deklarierte `ContextProgram` über [`AgentSession::context_program`] und
//! die konkrete `ContextLoadExecutor`-Instanz über [`find_executor`] +
//! `as_context_load_executor` — und ruft `seed_turn` auf. Die dabei
//! übergebene `spent_per_section`-Map ist leer, weil Blocker 1 (kein
//! `ContextProvider` erzeugt `harw_context::Fragment`) unverändert offen ist:
//! ohne die neue Montage gibt es keine echten Kosten je Sektion zu
//! übergeben. **Hier, an genau dieser Stelle, bricht die Kette zu
//! `FragmentReference`/`DetailMode::References` jetzt weiterhin** — nicht
//! mehr am fehlenden Weg zum Ausführer.
//!
//! **Ergebnis (Stand vor diesem Knoten):** kein Produktionspfad erreicht
//! `FragmentReference` — Blocker 1 (`ContextProvider` → `harw_context::Fragment`)
//! lag außerhalb dieses Schreibbereichs.
//!
//! ## Vierter Nachtrag (dieser Knoten): Blocker 1 war eine Rückwandlung, kein
//! fehlender Erzeuger — die Montage läuft jetzt in Produktion
//!
//! Eigene Prüfung, dieser Knoten: **dieser** Knoten hat Schreibzugriff auf
//! `harw-core/src/model.rs` bekommen, den frühere Knoten nicht hatten (siehe
//! `context_budget.rs`s Moduldoku, Abschnitt „Warum der Bestandspfad
//! unangetastet bleibt" — dort ist die fehlende Berechtigung auf `model.rs`
//! als der Grund genannt, warum die Montage nicht angeschlossen wurde). Mit
//! diesem Zugriff zeigt sich: „Blocker 1" war nie ein fehlender
//! `harw_context::Fragment`-Erzeuger — [`gather_context`] ruft `contribute_v2`
//! bereits seit dem „Vierter Nachtrag" oben auf und bekommt dabei **echte**
//! `harw_context::Fragment`-Werte zurück (für jeden v1-Provider über
//! `harw_extension_api::fragment_from_v1`s dokumentierte, nicht erfundene
//! Abbildung: `trust = TrustClass::Data`, `stability = Stability::Fresh`,
//! `section` = eine feste v1-Sektion, `cost` über `BytesOverFour`). Der
//! Blocker war die **Rückwandlung**, die [`gather_context`] direkt danach auf
//! diese Fragmente anwandte, um `ModelRequest::with_context_budget`s
//! `Vec<ContextFragment>`-Signatur zu bedienen.
//!
//! [`gather_context`] liefert seit diesem Knoten `Vec<harw_context::Fragment>`
//! **ohne** diese Rückwandlung (siehe seine eigene Doku, „Fünfter Nachtrag").
//! [`drive_turn`] reicht das Ergebnis zusammen mit
//! [`AgentSession::context_program`] und
//! `AgentSession::spawn_context().and_then(|c| c.ceiling.as_ref())` an
//! [`crate::model::ModelRequest::with_context_program`] weiter
//! (`harw-core/src/model.rs`, Moduldoku „Zwei Wege zur Kontextmontage") —
//! **dort**, nicht hier, entscheidet sich, ob die AW1-03/AW4-01-Montage läuft
//! oder auf den alten Byte-Budget-Pfad zurückfällt. Diese Datei kennt die
//! Unterscheidung selbst nicht mehr; sie reicht beide Werte unbedingt durch.
//!
//! **Ergebnis:**
//! - Die Zwei-Block-Trennung nach Vertrauensklassen (AW4-01) läuft jetzt in
//!   Produktion, für jede Sitzung, die sowohl ein `ContextProgram` als auch
//!   eine geschnittene `ContextCeiling` trägt.
//! - Der Nullzähler `TRUST_BLOCK_VIOLATION` ist damit von [`drive_turn`] aus
//!   erreichbar — nicht mehr nur aus den Unit-Tests von `context_budget.rs`.
//! - `DetailMode::References`/`FragmentReference` sind ebenfalls erreichbar:
//!   `ModelRequest::with_context_program` liest
//!   `ContextProgram::section_detail()` (`model.rs`s `section_detail_map`)
//!   und reicht das Ergebnis an `render_trust_blocks_with_detail` weiter.
//! - **Weiterhin offen:** `seed_context_load_ledger` unten übergibt
//!   `seed_turn` weiterhin eine **leere** `spent_per_section`-Map, nicht das
//!   Ergebnis von `ContextAssemblyV2::spent_per_section()`. Der Grund ist
//!   jetzt ein struktureller, kein fehlender Baustein mehr:
//!   `seed_context_load_ledger` läuft **einmal**, vor der Schleife, bevor
//!   überhaupt ein `ModelRequest` gebaut wird — die Montage, die
//!   `spent_per_section` liefern würde, entsteht aber erst **innerhalb**
//!   dieser Schleife, in `with_context_program`. Sie vorzuziehen würde
//!   entweder die Montage doppelt laufen lassen (einmal zum Seeden, einmal
//!   für den ersten Model-Aufruf) oder `seed_turn` mehrfach je Turn aufrufen
//!   — beides verwirft genau die Buchhaltung, die `seed_turn`s eigene Doku
//!   ausdrücklich ausschließt (siehe oben, „ein Aufruf je Model-Runde würde
//!   … bereits verbuchte `context.load`-Aufrufe verwerfen"). Das ist ein
//!   struktureller Umbau der Aufrufreihenfolge dieser Schleife, größer als
//!   dieser Knoten rechtfertigt — dokumentiert als offener Folgeknoten, nicht
//!   halbfertig gebaut.
//!
//! ## Fünfter Nachtrag (dieser Knoten): [`TurnControl`] wird tatsächlich
//! durchgesetzt — vorher las nichts in [`drive_turn`] den Steuerblock
//!
//! Eigene Prüfung: [`TurnControl`] und [`TurnLimits`] existierten bereits vor
//! diesem Knoten (samt vollständiger Prüfpunkt-Logik,
//! `model_checkpoint`/`tool_checkpoint`/`start`/`record_*`), aber **keine**
//! dieser Methoden wurde von [`drive_turn`] je aufgerufen — ein `TurnInput`
//! mit engen [`TurnLimits`] verhielt sich exakt wie eines mit
//! [`TurnLimits::unlimited`]. Dieser Knoten schließt die Lücke:
//!
//! - [`run_turn`]/[`run_turn_durable`] entnehmen `input.control` und reichen
//!   ihn nach [`drive_turn`] durch, das ihn vor jedem Modellaufruf
//!   ([`TurnControl::model_checkpoint`]) und vor jeder Werkzeugausführung
//!   ([`TurnControl::tool_checkpoint`]) befragt. Ein Treffer beendet den Turn
//!   als [`TurnOutcome::Cancelled`] — die Session kehrt über
//!   `AgentSession::complete_turn` nach `Idle` zurück, exakt wie ein
//!   regulärer Abschluss, nur mit `CancelReason` statt stillschweigendem Ende.
//! - `control.start()` markiert den Wanduhr-Nullpunkt einmal je
//!   `drive_turn`-Aufruf; `record_model_round`/`record_usage` laufen nach
//!   jeder Modellantwort, `record_tool_calls` nach bestandenem
//!   `tool_checkpoint`, unmittelbar bevor die Runde tatsächlich dispatcht wird.
//! - **Lücke war offen, ist seit „Siebter Nachtrag" geschlossen:**
//!   [`resume_after_child`] und [`resume_after_approval`] (bzw. ihre
//!   `_durable`-Varianten) sind öffentliche Einstiegspunkte, die von
//!   `harw-tui`, `harw-cli` und den Tests dieses Workspaces mit ihrer
//!   heutigen Signatur aufgerufen werden — ihre Signatur nimmt weiterhin
//!   **keinen** `TurnControl`-Parameter entgegen, das war und bleibt
//!   unverändert. Der damalige Blocker war ein fehlendes session-persistes
//!   Feld außerhalb des Schreibbereichs dieses Knotens (`session.rs`); jener
//!   Schreibbereich schließt seit „Siebter Nachtrag" auch `session.rs` ein.
//!   `AgentSession::active_turn_control` trägt den Block jetzt über die Pause
//!   hinweg, `resume_after_child`/`resume_after_approval` (bzw. ihre
//!   `_durable`-Varianten) lesen ihn darüber zurück, statt bedingungslos einen
//!   frischen `TurnControl::new()` zu bauen — Details siehe „Siebter
//!   Nachtrag" unten.
//!
//! ## Sechster Nachtrag (dieser Knoten, Welle 3): der Modellaufruf racet
//! jetzt tatsächlich, und `ToolsError::Cancelled` bricht mitten in der
//! Ausführung sauber ab
//!
//! Der „Fünfter Nachtrag" oben beschrieb bereits **prüfpunktbasierten**
//! Abbruch (vor dem Modellaufruf, vor der Werkzeugausführung). Zwei Lücken
//! blieben offen, weil beide vorausgesetzte Bausteine erst in einer früheren
//! Welle dieses Knotens entstanden: [`ModelRequest::cancel`] (racebar über
//! [`ModelRequest::with_cancel_token`]) und [`ToolExecutionContext::cancel`]
//! (racebar über `ToolExecutionContext::with_cancel`). Dieser Knoten
//! verdrahtet beide:
//!
//! - **Modellaufruf.** [`drive_turn`] racet `model.respond(request)` per
//!   `tokio::select!` (`biased`) gegen `control.cancel_token().cancelled()`
//!   — der Modellaufruf selbst muss nicht mehr bis zu seiner Antwort
//!   durchlaufen, um einen bereits erfolgten Abbruch zu bemerken. `request`
//!   trägt zusätzlich denselben Token über `with_cancel_token`, damit ein
//!   cancel-fähiger Provider (z. B. `RetryingProvider::race_respond`) auch
//!   innerhalb eigener Retry-Versuche früher abbricht; das `select!` bleibt
//!   der Rückfallpfad für jeden anderen Provider. Meldet `respond()` selbst
//!   [`crate::model::ModelError::Cancelled`] (weil ein cancel-fähiger
//!   Provider den Token vor dem `select!` sah), mündet das in denselben
//!   [`cancel_turn`]-Pfad wie ein Treffer am Prüfpunkt — kein neuer
//!   [`TurnOutcome`], keine neue Fehlervariante.
//! - **Werkzeugausführung.** [`tool_execution_context`] hängt
//!   `control.cancel_token()` an jeden gebauten `ToolExecutionContext` — an
//!   allen drei Aufrufstellen (sequenzieller Dispatch,
//!   [`try_execute_parallel_calls`], [`resume_after_approval_with_store`]).
//!   Meldet ein Ausführer daraufhin `ToolsError::Cancelled`, behandeln beide
//!   Dispatch-Pfade das **nicht** als gewöhnlichen Tool-Fehler (kein
//!   `ToolCallResult::error(...)` ans Modell): der laufende Call bekommt sein
//!   Cancel-Ergebnis, alle noch nicht ausgelieferten Calls derselben Antwort
//!   bekommen ein synthetisches Ergebnis über denselben Mechanismus wie ein
//!   `tool_checkpoint()`-Treffer ([`cancel_turn_with_pending_calls`]
//!   sequenziell; der bereits bestehende `ParallelOutcome::Aborted`-Pfad
//!   parallel — dort stehen alle `tool_call`-Einträge ohnehin schon vor dem
//!   Start der Jobs im Verlauf, sodass nur noch das `tool_result` fehlt).
//!   `resume_after_approval_with_store` bekommt den Token seines eigenen
//!   frischen `TurnControl::new()` (siehe „Fünfter Nachtrag", offene Lücke)
//!   — plumbing, keine neue Dispatch-Reaktion dort.
//!
//! ## Siebter Nachtrag (dieser Knoten): die „Fünfter Nachtrag"-Lücke ist
//! geschlossen — der Steuerblock überlebt Handoff- und Rückfrage-Pausen jetzt
//! tatsächlich, und zwei bisher ungeracete Ausführungspfade racen jetzt auch
//!
//! Der „Fünfter Nachtrag" oben nannte drei offene Punkte: kein session-persister
//! `TurnControl`-Speicher über eine Pause hinweg, ein ungeracetes
//! `executor.traced_execute(...)` im Rückfrage-Fortsetzungspfad, und einen
//! ungeraceten parallelen Join-Loop. Alle drei sind mit diesem Knoten
//! geschlossen:
//!
//! - **Fix A — Steuerblock übersteht die Pause.** [`AgentSession`] trägt jetzt
//!   `active_turn_control: Option<TurnControl>` (`session.rs`, Feld-Doku dort):
//!   [`run_turn`]/[`run_turn_durable`] hinterlegen darauf einen Klon des
//!   `TurnInput::control`-Blocks, sobald der Turn beginnt
//!   (`AgentSession::set_active_turn_control`); [`resume_after_child`]/
//!   [`resume_after_child_durable`] und [`resume_after_approval`]/
//!   [`resume_after_approval_durable`] lesen ihn über
//!   `session.active_turn_control().cloned().unwrap_or_else(TurnControl::new)`
//!   zurück, statt bedingungslos einen frischen, unbegrenzten Block zu bauen.
//!   Der Fallback auf einen frischen Block bleibt für den Fall, dass eine
//!   Session ihren Turn nie über `run_turn`/`run_turn_durable` gestartet hat
//!   (z. B. Tests, die direkt `AgentSession::try_start_turn` rufen) — kein
//!   Panic, keine Regression für Aufrufer außerhalb dieses Knotens.
//!   `AgentSession::complete_turn` löscht das Feld wieder, symmetrisch zu
//!   `current_turn`. Ein `CancelToken`, der vor einer Handoff- oder
//!   Rückfrage-Pause abgebrochen wurde (Ctrl+C), erreicht damit jetzt auch die
//!   Fortsetzung nach der Pause — vorher verschluckte der frische Token genau
//!   diesen Fall stillschweigend.
//! - **Fix B — der Rückfrage-Fortsetzungspfad racet jetzt auch.**
//!   `resume_after_approval_with_store`s Zweig für einen genehmigten,
//!   nicht-Handoff-Call (der „Rückfrage"-Pfad, nach einem `AskUser`) hatte
//!   keine `was_cancelled`-Weiche — ein hier hängender Ausführer war gegen
//!   Cancel vollständig blind, unabhängig davon, ob er selbst kooperativ ist.
//!   Der Aufruf racet jetzt exakt wie im sequenziellen Pfad (`tokio::select!`,
//!   `biased`, `control.cancel_token().cancelled()` gegen
//!   `executor.traced_execute(...)`) und liefert bei einem Treffer ein
//!   synthetisches `Err(ToolsError::Cancelled)`; ein Treffer geht über
//!   [`cancel_turn_with_pending_calls`] mit `remaining: Vec::new()` — ein
//!   Rückfrage-Resume setzt genau einen Call fort, die übrigen Calls derselben
//!   Modellantwort wurden beim `AskUser`-Treffer nie in den Verlauf
//!   geschrieben (siehe `ApprovalDecision::AskUser` im sequenziellen Pfad) und
//!   sind daher keine offenen Geschwister, die noch ein Ergebnis bräuchten.
//!   Der sequenzielle Dispatch-Pfad selbst bekam denselben Race schon
//!   zusätzlich um den ungeraceten `.await` seines Werkzeugaufrufs herum —
//!   vorher konnte ein Ausführer, der `ctx.cancel()` nie selbst abfragt, den
//!   Turn bis zu seinem eigenen Ende blockieren; jetzt beendet ein Treffer den
//!   `select!` synchron mit einem synthetischen `Err(ToolsError::Cancelled)`.
//! - **Fix C — der parallele Join-Loop racet jetzt auch.**
//!   [`try_execute_parallel_calls`] wartete mit
//!   `while let Some(joined) = joins.join_next().await` ungeracet auf **alle**
//!   Aufrufe der `JoinSet`, bevor überhaupt auf Cancel reagiert wurde — ein
//!   nicht kooperativer Ausführer blockierte damit die gesamte Antwort. Die
//!   Reaper-Schleife racet jetzt selbst gegen `control.cancel_token()`: ein
//!   Treffer setzt `cancel_hit` und ruft `joins.abort_all()`; danach werden
//!   weiterhin abgeschlossene Jobs abgeholt (`join_next()` bleibt aktiv), ein
//!   `JoinError` eines abgebrochenen Jobs wird nach einem Cancel-Treffer
//!   verworfen statt propagiert (vorher — ohne Cancel-Treffer — bleibt ein
//!   `JoinError` weiterhin ein echter Fehler dieser Funktion). Jeder Slot, der
//!   danach noch `None` ist (abgebrochen, bevor sein Ergebnis geerntet wurde),
//!   bekommt synthetisch dasselbe `"turn cancelled before delivery"`-Ergebnis,
//!   das die bereits bestehende `TurnGuard`-Abort-Auslieferung darunter auch
//!   für andere Abbruchgründe erzeugt — die Auslieferungsschleife selbst
//!   bleibt unverändert, sie verlangt nur, dass jeder Slot `Some(...)` ist,
//!   was jetzt garantiert ist.
//!
//! ## Achter Nachtrag (dieser Knoten): hängender `Running`-Zustand nach einem
//! Fehler beim Rückfrage-Resume, und ein Fortschritts-Hinweis, den ein Modell
//! als Antwort-Cache fehldeutete
//!
//! - **Fix A — jeder Fehlerausstieg von [`resume_after_approval_with_store`]
//!   nach erfolgreichem `resolve_approval` durchläuft jetzt
//!   [`transition_after_turn_failure`].** Vorher gaben mehrere Stellen (jedes
//!   `?` nach dem Rückfrage-Resume, sowie zwei `return drive_turn(...)`/
//!   `return cancel_turn_with_pending_calls(...)`) ihr `Result` direkt aus der
//!   Funktion zurück — anders als [`run_turn_with_approvals`] und
//!   [`resume_after_child_with_approvals`], die diesen Übergang bereits
//!   korrekt anwenden. Ein Transport-Fehler des Modells während eines
//!   fortgesetzten Rückfrage-Turns ließ die Session dadurch dauerhaft in
//!   `Running` hängen; der nächste Turn scheiterte mit „session not idle:
//!   Running", obwohl der ursprüngliche Fehler retrybar war. Der gesamte
//!   Auflösungspfad (der `match resolution { ... }`-Ausdruck) läuft jetzt in
//!   einem inneren `async`-Block; `return`/`?` darin beenden nur diesen Block,
//!   nicht die Funktion — genau ein `Result` wird danach geprüft und im
//!   Fehlerfall genau einmal an [`transition_after_turn_failure`] gereicht,
//!   bevor es zurückgegeben wird.
//! - **Fix C — Polling zählt als Fortschritt.** [`apply_tool_guard`] wertete
//!   einen erfolgreichen Tool-Aufruf nur dann als Rundenfortschritt, wenn
//!   seine Signatur (Name + kanonische Argumente) in diesem Turn noch nicht
//!   gesehen wurde. Für [`crate::guard::POLLING_TOOLS`] (aktuell nur
//!   `shell.exec`) ist ein wiederholter identischer Aufruf jedoch legitimes
//!   Warten auf einen laufenden Hintergrundprozess, kein Stillstand — ein
//!   Modell, das so pollte, sah stattdessen den
//!   [`crate::guard::TurnGuard::observe_round_end`]-Hinweis zu ausbleibendem
//!   Fortschritt und deutete ihn als Beleg für einen nicht existenten
//!   Antwort-Cache, woraufhin es Befehle künstlich variierte, um „neue"
//!   Aufrufe zu erzwingen. Ein wiederholter *erfolgreicher* Aufruf eines
//!   `POLLING_TOOLS`-Eintrags zählt deshalb jetzt unabhängig von seiner
//!   Signatur als Fortschritt, und der Hinweistext benennt das Missverständnis
//!   jetzt explizit („Es gibt keinen Antwort-Cache — jeder Aufruf wurde echt
//!   ausgeführt"). Die Schwellenwerte selbst (`no_progress_rounds_warn`/
//!   `_abort`) sind unverändert.
//!
//! ## Neunter Nachtrag (Welle 3): Kontextfenster-Wächter
//!
//! - **Pre-flight**: vor jedem Modellaufruf schätzt
//!   [`crate::context_budget::estimate_request_tokens`] den fertig gebauten
//!   Request (kalibriert über [`AgentSession::token_calibration`]). Ist eine
//!   Verdichtung vorgemerkt ([`AgentSession::pending_compaction`], z. B. nach
//!   einem Wechsel auf ein kleineres Modell), meldet
//!   `AutoCompactPolicy::must_compact_before_send` einen Notfall oder sprengt
//!   Schätzung + Output-Reserve das Fenster, läuft vor dem Senden
//!   [`crate::compaction::compact_for_budget`] (Ziel 60 % von
//!   `Fenster − Reserve`) und der Request wird neu gebaut. Passt er danach
//!   immer noch nicht, endet der Turn mit „Kontext erschöpft"
//!   (`ModelError::ContextLength` mit deutscher Meldung).
//! - **Ausgabelimit**: jeder Request trägt
//!   [`AgentSession::max_output_tokens`] als `max_output_tokens`.
//! - **Kalibrierung**: nach jeder Antwort wird die geschätzte Byte-Zahl mit
//!   `usage.prompt_tokens()` verglichen (`TokenCalibration::observe`).
//! - **Recovery**: `ModelError::ContextLength` bzw.
//!   `StopReason::ContextWindowExceeded` lösen genau eine Notfall-Verdichtung
//!   (Ziel 50 %) plus eine Wiederholung aus; ein zweiter Fehlschlag geht an
//!   den Aufrufer bzw. endet als `Truncated`.
//! - **`MaxTokens` mit Tool-Calls**: die Runde wird einmal mit verdoppeltem
//!   Ausgabelimit (≤ 2 × Reserve, ≤ Fenster/2) wiederholt, bevor der Turn
//!   `Truncated` endet. Eine verworfene Antwort hinterlässt nichts im Verlauf.
//! - **Auto-Compaction** (`maybe_compact`): entscheidet über Prompt- +
//!   Output-Tokens der letzten Runde plus die geschätzten seither angehängten
//!   Tool-Ergebnisse, mit Hysterese (`AutoCompactPolicy::worth_compacting`);
//!   ein No-op wird weder persistiert noch gemeldet.
//! - `TurnEvent::ContextUpdated` trägt zusätzlich Schätzung, Schwelle und
//!   Reserve.
//!
mod api;
mod control;
mod delegation;
mod persistence;
mod round_context;

pub use api::{ApprovalResolution, TurnInput, TurnOutcome};
pub use control::{
    DEFAULT_TOKEN_BUDGET_WRAP_UP_PERCENT, RewindMediaCheck, TOKEN_BUDGET_WRAP_UP_INSTRUCTION,
    TOOL_BUDGET_SKIP_PREFIX, TOOL_CALL_REFUSED_PREFIX, TurnControl, TurnLimits, TurnTokenBudget,
};
pub use delegation::{AGENTS_CATALOG_TOOL, AGENTS_DELEGATE_TOOL, HANDOFF_DESCRIPTION_CHARS};
pub(crate) use delegation::generated_delegation_tool_available;
use delegation::*;
use persistence::*;
use round_context::*;

#[cfg(test)]
mod tests;

use crate::cancel::{CancelReason, CancelToken};
use crate::capture::{ToolOutcome, ToolOutcomeStatus};
use crate::error::{CoreError, CoreResult};
use crate::guard::{DriftEvent, DriftKind, GuardVerdict, SessionGuardVerdict, TurnGuard};
use crate::model::{ModelProvider, ModelRequest, RequestIdentity};
use crate::session::{AgentSession, SpawnContext, TurnHandle};
use crate::state_store::{StateStore, StateStoreError, UsageRound};
use crate::tool_call_meter::{ToolCallMeter, ToolCallPermit};
use harw_extension_api::{
    ApprovalDecision, DelegationTargetInfo, LoadedInstructions, SpawnInput, ToolExecutor,
    TurnInputContext, TurnStartInput, TurnStopInput,
};
use harw_protocol::events::TurnEvent;
use harw_protocol::items::{
    AssistantMessageItem, ContentPart, OpaqueReasoning, ReasoningItem, ToolCallResult,
    ToolPlacement, TurnItem,
};
use harw_session_store::{ApprovalRecord, ApprovalStore};
use harw_tools::executor::ExecutionPlacement;
use harw_tools::{
    AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType, ToolCall,
    ToolExecutionContext, ToolName, ToolOutput, ToolSpec, ToolsError, TracedToolExecutor,
};
use harw_types::{
    ApprovalActor, Clock, ReviewDecision, SessionId, SystemClock, TokenUsage, ToolCallId,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tracing::Instrument as _;

/// Präfix, an dem ein Tool-Call als Handoff erkannt wird (Agents-SDK-Muster
/// `transfer_to_<role>`).
pub const HANDOFF_PREFIX: &str = "transfer_to_";

/// Anfang des Endgrunds ([`TurnControl::stop_detail`]) eines Turns, den das
/// Token-Budget beendet hat. Der Kind-Spawner erkennt daran, dass die
/// Budget-Übergabe (`enforce_child_budget`) statt der Endbericht-Verdichtung
/// greift.
pub const TOKEN_BUDGET_STOP_PREFIX: &str = "Token-Budget erschöpft";

/// Ergebnis-Slot für einen parallelen Tool-Call: (ID, Ergebnis, Wandzeit ms).
// Vorletztes Feld: `true`, wenn dieser Call mit `ToolsError::Cancelled`
// endete (siehe `try_execute_parallel_calls`s Ergebnis-Auslieferung).
// Letztes Feld: die Platzierung aus den Ausführer-Metadaten (R18 D-D).
type ParallelCallSlot = Option<(
    ToolCallId,
    ToolCallResult,
    u64,
    String,
    serde_json::Value,
    bool,
    Option<ToolPlacement>,
)>;

/// Ergebnis eines sequenziell ausgeführten Tool-Calls: (Ergebnis, Wandzeit
/// ms, Platzierung aus den Ausführer-Metadaten, R18 D-D).
type PlacedCallResult = (ToolCallResult, u64, Option<ToolPlacement>);



/// Sammelt Kontext von allen `ContextProvider`n und filtert nach der
/// session-level [`SessionActivation`][crate::activation::SessionActivation].
///
/// # Description
/// Iterates every registered `ContextProvider`, but calls
/// [`harw_extension_api::ContextProvider::contribute_v2`] instead of the
/// legacy `contribute` — **this is the production caller referenced in this
/// module's "Zweiter Nachtrag"/"Dritter Nachtrag" sections**: every provider
/// that does not override `contribute_v2` still runs through it via its
/// default bridge (`fragment_from_v1`), so `contribute_v2` is now reached on
/// every turn, not only from `harw-extension-api`'s own tests.
///
/// **Fünfter Nachtrag (dieser Knoten):** vor diesem Knoten projizierte diese
/// Funktion jedes zurückgegebene [`harw_context::Fragment`] sofort zurück auf
/// das alte [`ContextFragment`] (`label` + `content` ← `label.as_str()` +
/// `body`), weil `ModelRequest::with_context_budget`
/// (`harw-core/src/model.rs`) ausschließlich `ContextFragment` annahm. Dieser
/// Knoten hat Schreibzugriff auf `model.rs` bekommen und dort
/// `ModelRequest::with_context_program` ergänzt, das native
/// `harw_context::Fragment`s entgegennimmt (siehe dessen Moduldoku, Abschnitt
/// „Zwei Wege zur Kontextmontage") — die Rückwandlung ist deshalb **nicht
/// mehr hier** nötig; sie passiert (wenn überhaupt) jetzt in `model.rs`s
/// Rückfallpfad, für eine Sitzung ohne Programm oder ohne Decke. Diese
/// Funktion liefert seit diesem Knoten die von `contribute_v2` gelieferten
/// `harw_context::Fragment`s **unverändert** (nach dem Aktivierungsfilter)
/// zurück — keine Information geht mehr auf dem Weg zu `drive_turn` verloren.
///
/// Fragmente werden weiterhin exakt wie vor diesem Knoten gefiltert: ein
/// Fragment, dessen Label in der Aktivierung der Sitzung deaktiviert ist,
/// wird verworfen. Ein v1-Fragment mit **leerem** Label wird von
/// `contribute_v2`s Vorgabe-Bridge bereits vorher verworfen (bei der
/// `FragmentLabel::try_new`-Prüfung) — siehe
/// `harw_extension_api::fragment_from_v1`s eigene Dokumentation; kein
/// Provider oder Test in diesem Workspace konstruiert ein leeres Label.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
/// - `ctx` (`&TurnInputContext`): turn metadata forwarded to each provider.
///
/// # Returns
/// Filtered list of [`harw_context::Fragment`]s that are model-visible, in
/// arrival order (order has no effect on the deterministic montage in
/// `crate::context_budget::Assembly`).
///
/// # Concurrency
/// `async`; no locks held across `.await` points.
pub async fn gather_context(
    session: &AgentSession,
    ctx: &TurnInputContext,
) -> Vec<harw_context::Fragment> {
    let activation = session.activation();
    let mut fragments = Vec::new();
    for provider in session.registry().context_providers() {
        let contributed = provider.contribute_v2(ctx).await;
        for fragment in contributed {
            let label_str = fragment.label.as_str();
            let filter_label = if label_str.is_empty() {
                "unlabeled"
            } else {
                label_str
            };
            if activation.is_context_enabled(filter_label) {
                fragments.push(fragment);
            }
        }
    }
    fragments
}

/// Höchstzahl der Wunsch-Tags, die [`wanted_tags`] aus einem Text liest.
const MAX_WANTED_TAGS: usize = 8;

/// Mindestlänge (in Zeichen) eines Worts, damit es als Wunsch-Tag zählt.
const MIN_WANTED_TAG_CHARS: usize = 4;

/// Leitet aus dem Text der jüngsten User-Nachricht die Wunsch-Tags für die
/// kompakte Zielempfehlung ab: kleingeschriebene Wörter ab
/// [`MIN_WANTED_TAG_CHARS`] Zeichen, ohne Duplikate, in Textreihenfolge, die
/// ersten [`MAX_WANTED_TAGS`].
///
/// # Beschreibung
/// Wörter trennen sich an allem außer Buchstaben, Ziffern, `-` und `_`;
/// Bindestriche und Unterstriche am Rand fallen weg. Reine Darstellungshilfe:
/// ein Tag verleiht kein Ziel und entzieht keines.
fn wanted_tags(text: &str) -> BTreeSet<String> {
    let mut tags = BTreeSet::new();
    for word in text.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')) {
        if tags.len() >= MAX_WANTED_TAGS {
            break;
        }
        let word = word.trim_matches(['-', '_']);
        if word.chars().count() >= MIN_WANTED_TAG_CHARS {
            tags.insert(word.to_lowercase());
        }
    }
    tags
}

/// Baut den Delegationsziel-Kontextblock (Nachtrag F) aus bereits sortierten
/// Rollennamen — oder `None`, wenn keine Ziele sichtbar sind.
///
/// # Arguments
/// - `names` (`&[String]`): exakte, sortierte Rollennamen aus
///   [`harw_extension_api::AgentSpawner::delegation_targets`].
/// - `via_delegate_tool` (`bool`): ob die Ziele über `agents.delegate`
///   (statt `transfer_to_<name>`) angeboten werden.
/// - `catalog` (`bool`): ob `agents.catalog` angeboten wird.
/// - `offered` (`&[String]`): die kompakte Top-k-Empfehlung
///   ([`harw_extension_api::DelegationTargets::offers`]); leer ⇒ keine
///   Empfehlung.
///
/// # Beschreibung
/// Bei mehr als [`crate::delegation_visibility::DELEGATE_TOOL_THRESHOLD`]
/// Zielen und nicht leerer Empfehlung steht im Block nur die Anzahl plus die
/// Empfehlung statt aller Namen. Das ist reine Darstellung: das
/// Werkzeugschema (`agents.delegate`) bleibt unverändert, jeder Name bleibt
/// aufrufbar, und die Admission prüft jedes Ziel weiterhin selbst.
///
/// # Returns
/// `None` bei leerer Liste oder wenn die (statischen, stets gültigen)
/// Label-/Sektionsnamen unerwartet nicht konstruierbar wären — dann bleibt
/// der Turn ohne diesen Block, statt zu scheitern.
fn delegation_targets_fragment(
    names: &[String],
    via_delegate_tool: bool,
    catalog: bool,
    offered: &[String],
) -> Option<harw_context::Fragment> {
    if names.is_empty() {
        return None;
    }
    // Plan R9, Teil C: nur Namen (Cache-stabil sortiert) plus Verweis auf
    // den Katalog; Beschreibungen stehen in `transfer_to_*` bzw. im Katalog.
    let tool = if via_delegate_tool {
        "agents.delegate"
    } else {
        "transfer_to_<name>"
    };
    let details = if catalog {
        " Details: `agents.catalog`."
    } else {
        ""
    };
    let with_offers =
        names.len() > crate::delegation_visibility::DELEGATE_TOOL_THRESHOLD && !offered.is_empty();
    let body = if with_offers {
        format!(
            "Delegierbare Ziele ({tool}): {} Ziele. Empfohlen für diese Aufgabe: {}. \
             Alle Namen stehen im Werkzeugschema.{details}",
            names.len(),
            offered.join(", ")
        )
    } else {
        format!(
            "Delegierbare Ziele ({tool}): {}.{details}",
            names.join(", ")
        )
    };
    // Die Empfehlung folgt der letzten Nutzernachricht und ist damit nicht
    // cache-stabil; die reine Namensliste bleibt es.
    let stability = if with_offers {
        harw_context::Stability::Fresh
    } else {
        harw_context::Stability::Stable
    };
    let label = harw_context::FragmentLabel::try_new("delegation.targets").ok()?;
    let section = harw_context::SectionName::try_new("delegation.targets").ok()?;
    Some(harw_context::Fragment {
        label,
        section,
        trust: harw_context::TrustClass::Instruction,
        stability,
        origin: harw_context::FragmentOrigin {
            provider: "turn_loop.delegation_targets".to_owned(),
            namespace: "core".to_owned(),
            produced_at: jiff::Timestamp::now(),
        },
        cost: harw_lens_types::CostEstimate(body.len() as u32),
        digest: harw_types::ContentDigest::of(body.as_bytes()),
        body,
    })
}

/// Abstand, in dem ein wartendes Orchestrator-Kind nach Ergebnissen seiner
/// Hintergrund-Kinder sieht (kein Modellaufruf, keine Tokens).
const BACKGROUND_RESULT_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// Legt die Abschlussmeldungen der Hintergrund-Kinder dieser Sitzung als
/// Nutzer-Nachricht in den Verlauf.
///
/// # Description
/// Nur für Kind-Sitzungen (Orchestratoren unter der UIA): die UIA-Wurzel
/// holt ihre Meldungen über die TUI (Auto-Turn). Früher blieben die
/// Meldungen der Enkel im Register liegen — ein Orchestrator-Kind erfuhr nie,
/// was seine Hintergrund-Kinder geliefert hatten.
///
/// Zustellung genau einmal: die Meldungen werden nur gelesen, in den Verlauf
/// gelegt und erst **nach** erfolgreichem Persistieren bestätigt. Scheitert
/// das Schreiben, wird der Eintrag aus dem Verlauf zurückgenommen und die
/// Meldungen bleiben im Register — ein Neuversuch stellt sie zu.
///
/// # Returns
/// `true`, wenn mindestens eine Meldung zugestellt wurde.
///
/// # Errors
/// Fehler des [`StateStore`] (dann ist nichts bestätigt).
async fn inject_background_notices(
    session: &mut AgentSession,
    store: &dyn StateStore,
) -> CoreResult<bool> {
    if session.parent_session_id().is_none() {
        return Ok(false);
    }
    let Some(spawner) = session.registry().spawner().cloned() else {
        return Ok(false);
    };
    let notices = spawner.pending_background_notices(session.id());
    if notices.is_empty() {
        return Ok(false);
    }
    let mark = session.history().len();
    let text = notices
        .iter()
        .map(|notice| notice.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    session.history_mut().push_user_text(text);
    if let Err(error) = persist_last(session, store).await {
        session.history_mut().truncate(mark);
        return Err(error);
    }
    let children: Vec<harw_types::SessionId> =
        notices.into_iter().map(|notice| notice.child).collect();
    spawner.acknowledge_background_notices(session.id(), &children);
    tracing::info!(
        session = %session.id(),
        notices = children.len(),
        "turn_loop.background_notices_delivered"
    );
    Ok(true)
}

/// Bricht die noch laufenden Hintergrund-Kinder einer abgebrochenen
/// Kind-Sitzung ab.
///
/// # Description
/// Zentral in [`cancel_turn`] (auch über `cancel_turn_with_pending_calls`),
/// damit **jeder** abgebrochene Ausgang eines Orchestrator-Kindes seine
/// Hintergrund-Kinder mitnimmt — auch wenn der Abbruch erst nach einer
/// zugestellten Meldung in der nächsten Runde greift. Ihre Ergebnisse
/// erreichten sonst nie mehr einen Elternteil. Die UIA-Wurzel ist
/// ausgenommen: dort verwaltet die TUI ihre Hintergrund-Agenten selbst
/// (`/new`, `/resume`, Beenden), ein abgebrochener UIA-Turn beendet sie nicht.
fn cancel_owned_background_children(session: &AgentSession, reason: CancelReason) {
    if session.parent_session_id().is_none() {
        return;
    }
    let Some(spawner) = session.registry().spawner() else {
        return;
    };
    if spawner.running_background_children(session.id()) == 0 {
        return;
    }
    let cancelled = spawner.cancel_background_children(session.id(), "parent turn cancelled");
    tracing::info!(
        session = %session.id(),
        ?reason,
        cancelled,
        "turn_loop.background_children_cancelled_with_parent"
    );
}

/// Ergebnis des Wartens auf Hintergrund-Kinder am Turn-Ende.
#[derive(Debug)]
enum BackgroundWait {
    /// Mindestens eine Meldung liegt im Verlauf; der Turn läuft weiter.
    Delivered,
    /// Kein Kind läuft (mehr) oder keine Kind-Sitzung: der Turn endet normal.
    Idle,
    /// Abbruch oder Budget-/Zeitende während des Wartens; bereits
    /// vorliegende Meldungen sind zugestellt, die laufenden Hintergrund-Kinder
    /// bricht [`cancel_turn`] ab.
    Stopped(CancelReason),
}

/// Wartet am Ende eines Orchestrator-Kind-Turns auf Ergebnisse noch
/// laufender Hintergrund-Kinder.
///
/// # Description
/// Gewartet wird ohne Modellaufruf ([`BACKGROUND_RESULT_POLL`]). Ein
/// Abbruch, ein erschöpftes Budget oder die Wanduhr des Turns beenden das
/// Warten als [`BackgroundWait::Stopped`] — nie als Erfolg. Der Aufrufer
/// endet dann über [`cancel_turn`], das die noch laufenden Hintergrund-Kinder
/// dieser Sitzung abbricht ([`cancel_owned_background_children`]).
///
/// # Errors
/// Fehler des [`StateStore`] beim Zustellen.
async fn await_background_results(
    session: &mut AgentSession,
    store: &dyn StateStore,
    control: &TurnControl,
) -> CoreResult<BackgroundWait> {
    if session.parent_session_id().is_none() {
        return Ok(BackgroundWait::Idle);
    }
    let Some(spawner) = session.registry().spawner().cloned() else {
        return Ok(BackgroundWait::Idle);
    };
    loop {
        // Erst der Stopp-Prüfpunkt, dann die Zustellung. Bei einem Stopp
        // werden die noch laufenden Geschwister *vor* dem fehlbaren
        // Schreibversuch abgebrochen — ein Schreibfehler darf sie nicht
        // weiterlaufen lassen. Eine bereits vorliegende Meldung wird danach
        // noch geschrieben und bestätigt; scheitert das, bleibt sie
        // unbestätigt im Register (siehe `inject_background_notices`).
        if let Some(reason) = control
            .model_checkpoint()
            .or_else(|| control.tool_checkpoint(0))
        {
            cancel_owned_background_children(session, reason);
            inject_background_notices(session, store).await?;
            tracing::info!(
                session = %session.id(),
                ?reason,
                "turn_loop.background_wait_stopped"
            );
            return Ok(BackgroundWait::Stopped(reason));
        }
        if inject_background_notices(session, store).await? {
            return Ok(BackgroundWait::Delivered);
        }
        if spawner.running_background_children(session.id()) == 0 {
            return Ok(BackgroundWait::Idle);
        }
        tokio::time::sleep(BACKGROUND_RESULT_POLL).await;
    }
}

/// Text der Werkzeug-Antwort auf eine abgelehnte Delegation.
///
/// # Description
/// Bekannte Ablehnungen (Orchestrierungsgrenze, abgelehnte Fortsetzung,
/// Plan-Modus, typisierte Kapazitäts-/Tiefengrenzen) behalten ihren Text.
/// Alle übrigen (z. B. nicht registrierte Rolle, fehlende Sandbox, ein nach
/// `prepare` gescheiterter Fortsetzungsschritt) bekommen den stabilen Code
/// `HARW-ORCH-001` und einen Hinweis — früher brachen sie als
/// `CoreError::HandoffFailed` den ganzen Turn des Orchestrators ab.
fn spawn_rejection_text(role: &str, error: &harw_extension_api::AgentSpawnError) -> String {
    let known = crate::background_children::is_orchestration_limit_rejection(&error.message)
        || crate::child_handoff::is_continuation_rejection(&error.message)
        || crate::delegation_visibility::is_plan_mode_refusal(&error.message)
        || error.kind != harw_extension_api::SpawnRejectionKind::Other;
    tracing::warn!(
        code = "HARW-ORCH-001",
        role,
        kind = ?error.kind,
        reason = %error.message,
        "turn_loop.delegation_rejected"
    );
    if known {
        error.message.clone()
    } else {
        format!(
            "[HARW-ORCH-001] Delegation an '{role}' abgelehnt: {}. Dein Turn läuft weiter: \
             prüfe Rolle, Sandbox und Grenzen, oder schneide den Auftrag neu zu.",
            error.message
        )
    }
}

/// Wickelt eine Fortsetzungs-Instruktion nach einem `MaxTokens`-Abbruch in
/// einen [`harw_context::Fragment`]-Block (Trust-Klasse `Instruction`), damit
/// sie wie die übrigen Instructions in die Kontextmontage eingeht. Ein
/// Fehlschlag der Newtype-Validierung ist kein Turn-Fehler: ohne den Hinweis
/// liefert der Provider eben eine zweiteilige statt einer nahtlosen Antwort.
fn continuation_fragment(body: &str) -> Option<harw_context::Fragment> {
    instruction_fragment("continuation.instruction", "turn_loop.continuation", body)
}

/// Wickelt eine vom Turn-Loop selbst erzeugte Anweisung in einen
/// [`harw_context::Fragment`]-Block (Trust-Klasse `Instruction`); `name` ist
/// Label und Abschnitt zugleich, `provider` die Herkunft. `None`, wenn die
/// Newtype-Validierung scheitert (dann fehlt nur der Hinweis).
fn instruction_fragment(name: &str, provider: &str, body: &str) -> Option<harw_context::Fragment> {
    let label = harw_context::FragmentLabel::try_new(name).ok()?;
    let section = harw_context::SectionName::try_new(name).ok()?;
    Some(harw_context::Fragment {
        label,
        section,
        trust: harw_context::TrustClass::Instruction,
        stability: harw_context::Stability::Stable,
        origin: harw_context::FragmentOrigin {
            provider: provider.to_owned(),
            namespace: "core".to_owned(),
            produced_at: jiff::Timestamp::now(),
        },
        cost: harw_lens_types::CostEstimate(body.len() as u32),
        digest: harw_types::ContentDigest::of(body.as_bytes()),
        body: body.to_owned(),
    })
}

// Baut die per-Request-Identität für optionale Gateway-Header (`x-harw-*`),
// die `drive_turn` unten an `ModelRequest::with_identity` übergibt.
//
// `agent` ist die eigene Session-ID. `session` soll die Wurzel-Session des
// Agentenbaums gruppieren — `SpawnContext` (siehe `harw-core/src/session.rs`)
// trägt dafür kein eigenes Feld, nur `organizational_role`, deshalb ist der
// direkte Parent (`session.parent_session_id()`) die nächstbeste verfügbare
// Näherung: ein Kind kennt nur seinen unmittelbaren Elternteil, keine
// mehrstufige Kette bis zur Wurzel. Ohne Parent ist die Session selbst die
// Wurzel. `role` liest `SpawnContext::organizational_role`; fehlt der
// `SpawnContext` ganz (kein tatsächlich modellierter Root — jeder modellierte
// Root — kein Parent, Organisationsrolle `UserInterface`, Kriterium wie
// `is_uia_root_session` in `harw-tui` — trägt einen `SpawnContext`, z. B.
// eine bare Test-Session), fällt `role` auf `"main"` zurück.
fn request_identity(session: &AgentSession) -> RequestIdentity {
    let agent = session.id().as_str().to_owned();
    let session_id = session
        .parent_session_id()
        .map(|parent| parent.as_str().to_owned())
        .unwrap_or_else(|| agent.clone());
    let role = session
        .spawn_context()
        .map(|context| organizational_role_str(context.organizational_role))
        .unwrap_or("main")
        .to_owned();
    RequestIdentity {
        session: session_id,
        agent,
        role,
    }
}

// Kebab-case-Darstellung von `AgentRoleId`, identisch zu dessen
// `#[serde(rename_all = "kebab-case")]` (`harw-agent-dsl/src/roles.rs`) — das
// Enum trägt selbst kein `as_str`/`Display`.
fn organizational_role_str(role: harw_agent_dsl::roles::AgentRoleId) -> &'static str {
    use harw_agent_dsl::roles::AgentRoleId;
    match role {
        AgentRoleId::UserInterface => "user-interface",
        AgentRoleId::RootOrchestrator => "root-orchestrator",
        AgentRoleId::ChildOrchestrator => "child-orchestrator",
        AgentRoleId::Worker => "worker",
        AgentRoleId::UiaWorker => "uia-worker",
        AgentRoleId::AgentSteward => "agent-steward",
    }
}

/// Extrahiert lesbaren Denktext aus den Blöcken eines `OpaqueReasoning`
/// (Welle 3 — 3e; Block-Typen `"reasoning"`/`"reasoning_content"` ergänzt in
/// Teil 2 / Datei B des Reasoning-Sichtbarkeits-Plans).
///
/// # Beschreibung
/// Unterstützt drei Block-Formen, je nach Provider/Transport:
/// - `"thinking"` (Anthropic Messages API): Text direkt aus dem
///   `"thinking"`-Feld.
/// - `"reasoning"` (OpenAI-Responses-Transport): Text aus allen Einträgen
///   des `"summary"`-Arrays vom Typ `"summary_text"`, deren `"text"`-Felder
///   mit `\n` verbunden werden (dasselbe Verkettungsmuster wie für mehrere
///   `"thinking"`-Blöcke unten). Ein leeres oder fehlendes `"summary"`-Array
///   sowie ausschließlich leere `"text"`-Felder liefern für diesen Block
///   keinen Text (zählt nicht als „gefunden").
/// - `"reasoning_content"` (Chat-Transport, DeepSeek/Kimi/GLM-Konvention):
///   Text direkt aus dem `"text"`-Feld, wenn nicht leer.
///
/// `"redacted_thinking"` (Anthropic, verschlüsselt) und jeder andere/
/// unbekannte Block-Typ liefern keinen extrahierbaren Text und werden
/// stillschweigend übersprungen — robust gegen fehlende Felder, kein Panic,
/// kein Fehler. Eine gemischte Blockliste (in der Praxis nie vom selben
/// Provider gemeinsam geliefert) wird pro Block einzeln ausgewertet und die
/// gefundenen Texte werden wie bei mehreren `"thinking"`-Blöcken mit `\n`
/// verbunden.
///
/// Bewusst lokal in `turn_loop.rs` statt in `harw-provider-http` verortet:
/// letzteres Crate wird parallel von einem anderen Agenten bearbeitet; die
/// Extraktion gehört logisch dorthin (siehe
/// `harw_provider_http::anthropic::extract_anthropic_reasoning`), wird hier
/// aber bewusst dupliziert, um keine Datei außerhalb dieser zu berühren.
///
/// # Arguments
/// - `reasoning` (`&OpaqueReasoning`): die vom Provider gelieferten,
///   unverändert erhaltenen Denkblöcke.
///
/// # Returns
/// `Some(String)` — die pro Block extrahierten Texte in Blockreihenfolge,
/// mit `\n` verbunden — wenn mindestens ein Block extrahierbaren Text trug;
/// sonst `None` (z. B. nur `redacted_thinking`-/unbekannte Blöcke, leere
/// `summary`-Arrays oder eine leere Blockliste).
fn extract_thinking_text(reasoning: &OpaqueReasoning) -> Option<String> {
    /// Extrahiert den lesbaren Text eines einzelnen `"reasoning"`-Blocks
    /// (OpenAI Responses) aus dessen `"summary"`-Array: alle Einträge vom
    /// Typ `"summary_text"` mit nicht-leerem `"text"`-Feld, mit `\n`
    /// verbunden. Lokale Hilfsfunktion, kein eigener Doc-Header nötig.
    fn extract_reasoning_summary_text(block: &serde_json::Value) -> Option<String> {
        let summary = block.get("summary").and_then(serde_json::Value::as_array)?;
        let mut joined = String::new();
        for entry in summary {
            if entry.get("type").and_then(serde_json::Value::as_str) != Some("summary_text") {
                continue;
            }
            let Some(text) = entry.get("text").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            if !joined.is_empty() {
                joined.push('\n');
            }
            joined.push_str(text);
        }
        (!joined.is_empty()).then_some(joined)
    }

    let mut joined = String::new();
    for block in &reasoning.blocks {
        let block_text = match block.get("type").and_then(serde_json::Value::as_str) {
            Some("thinking") => block
                .get("thinking")
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
            Some("reasoning") => extract_reasoning_summary_text(block),
            Some("reasoning_content") => block
                .get("text")
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
            _ => None,
        };
        let Some(block_text) = block_text else {
            continue;
        };
        if !joined.is_empty() {
            joined.push('\n');
        }
        joined.push_str(&block_text);
    }
    (!joined.is_empty()).then_some(joined)
}

/// Lädt Instructions von allen `InstructionsProvider` und filtert nach der
/// session-level [`SessionActivation`][crate::activation::SessionActivation].
///
/// # Description
/// Iterates every registered `InstructionsProvider` and merges their output
/// into a single [`LoadedInstructions`]. The session activation is applied
/// using label conventions:
/// - The first non-empty `system_prompt` across all providers uses the label
///   `"baseline"`. Subsequent non-empty system prompts (if any provider
///   returned one and `combined.system_prompt` is already set) would use
///   `"extra_<index>"`, but the underlying merge already ignores them.
/// - Extra `fragments` from each provider use the label
///   `"fragment_<provider_index>_<fragment_index>"`.
///
/// If a label is disabled via [`SessionActivation::disable_instructions`], the
/// corresponding content is dropped.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
///
/// # Returns
/// Merged, filtered [`LoadedInstructions`].
///
/// # Concurrency
/// `async`; no locks held across `.await` points.
pub async fn load_instructions(session: &AgentSession) -> LoadedInstructions {
    let activation = session.activation();
    let mut combined = LoadedInstructions::default();
    for (provider_idx, provider) in session
        .registry()
        .instructions_providers()
        .iter()
        .enumerate()
    {
        let loaded = provider.load().await;
        // System prompt: first non-empty one wins with label "baseline";
        // subsequent providers' system_prompts are implicitly labelled
        // "extra_<n>" and skipped if already combined (existing logic).
        if !loaded.system_prompt.is_empty()
            && combined.system_prompt.is_empty()
            && activation.is_instructions_enabled("baseline")
        {
            combined.system_prompt = loaded.system_prompt;
        }
        // Fragments: each gets a deterministic label for filtering.
        for (frag_idx, fragment) in loaded.fragments.into_iter().enumerate() {
            let label = format!("fragment_{provider_idx}_{frag_idx}");
            if activation.is_instructions_enabled(&label) {
                combined.fragments.push(fragment);
            }
        }
    }
    combined
}

/// Benachrichtigt alle Turn-Observer über Start.
pub fn notify_turn_start(session: &AgentSession, input: &TurnStartInput) {
    for observer in session.registry().turn_observers() {
        observer.on_turn_start(input);
    }
}

/// Benachrichtigt alle Turn-Observer über Stop.
pub fn notify_turn_stop(session: &AgentSession, input: &TurnStopInput) {
    for observer in session.registry().turn_observers() {
        observer.on_turn_stop(input);
    }
}

/// Sendet ein [`TurnEvent`] an den optionalen Live-Event-Sink der Session.
/// No-op, wenn kein Sink konfiguriert ist oder der Empfänger bereits
/// abgehängt hat (Best-Effort, blockiert den Turn-Hot-Path nie).
fn emit(session: &AgentSession, event: TurnEvent) {
    session.live_emitter().emit(event);
}

/// Baut den Streaming-Sink einer Modell-Runde: Text-/Reasoning-Deltas und der
/// laufende Usage-Stand gehen live als [`TurnEvent`] hinaus. `None`, wenn
/// niemand zuhört (dann spart sich der Provider auch das SSE-Parsing nicht,
/// aber der Turn-Loop fordert kein Streaming an).
fn stream_sink_for_round(
    session: &AgentSession,
    turn_id: &harw_types::TurnId,
    turn_usage_so_far: &harw_types::TokenUsage,
) -> Option<crate::stream::StreamSink> {
    let emitter = session.live_emitter();
    if !emitter.is_observed() {
        return None;
    }
    let turn_id = turn_id.clone();
    let base = turn_usage_so_far.clone();
    Some(crate::stream::StreamSink::new(move |event| match event {
        crate::stream::ModelStreamEvent::TextDelta(text) if !text.is_empty() => {
            emitter.emit(TurnEvent::AssistantDelta {
                turn_id: turn_id.clone(),
                text,
            });
        }
        crate::stream::ModelStreamEvent::ReasoningDelta(text) if !text.is_empty() => {
            emitter.emit(TurnEvent::ReasoningDelta {
                turn_id: turn_id.clone(),
                text,
            });
        }
        crate::stream::ModelStreamEvent::Usage(round) => {
            let mut turn_total = base.clone();
            turn_total.add(&round);
            emitter.emit(TurnEvent::UsageUpdated {
                turn_id: turn_id.clone(),
                round,
                turn_total,
                final_round: false,
            });
        }
        _ => {}
    }))
}

/// Prüft einen `ToolCall` gegen alle `ApprovalHandler` und aggregiert.
///
/// # Beschreibung
/// **Jeder** registrierte Handler wird befragt, genau einmal und in
/// Registrierungsreihenfolge. Das Ergebnis folgt der strengsten Stimme:
///
/// `Deny` > `AskUser` > `Allow`
///
/// - Sagt irgendein Handler `Deny`, gilt das **erste** `Deny` (dessen
///   Begründung), auch wenn ein früherer Handler `AskUser` geliefert hat.
/// - Sonst gilt das **erste** `AskUser` samt seiner `ItemId`; spätere
///   `AskUser`-Anfragen werden verworfen.
/// - Nur wenn alle `Allow` sagen (oder kein Handler registriert ist), gilt
///   `Allow` — außer der DoD-Taint-Fallback greift (siehe Abschnitt
///   DoD-Taint).
///
/// Früher gewann der erste Nicht-`Allow`: ein vorn registriertes `AskUser`
/// (etwa `DefaultApprovalPolicy`) überstimmte ein später registriertes `Deny`
/// (etwa `ConfigApprovalPolicy`), der Nutzer konnte also freigeben, was eine
/// Politik verboten hatte (W1-05, Register G-004). Mit der Aggregation kann ein
/// angehängter Handler nur noch einschränken, nie lockern.
///
/// Ein `Deny` bricht die Befragung **nicht** ab: die Einmal-Befragung je
/// Handler bleibt die Invariante, ein Kurzschluss würde dagegen die
/// Befragungszahl von der Handler-Reihenfolge abhängig machen. `review` ist
/// laut Vertrag seiteneffektfrei (`harw_extension_api::ApprovalHandler`,
/// W0B-07); die weiteren Aufrufe öffnen deshalb keine zusätzlichen Prompts.
///
/// # Invariante
/// Ein `ApprovalHandler` wird pro Tool-Call **höchstens einmal** befragt: er
/// darf eine Approval-`ItemId` vergeben oder einen Audit-Eintrag schreiben.
/// Wer diese Funktion außerhalb des sequenziellen Tool-Loops aufruft (siehe
/// [`PreparedApprovals`]), muss die Entscheidung deshalb weiterreichen statt
/// sie später erneut einzuholen.
///
/// # DoD-Taint (PL-100 Phase 4, B22/O1)
/// Die Befragung läuft im Prüfkontext
/// [`crate::dod_taint::with_review_taint`] mit dem Taint-Stand des laufenden
/// Turns; die [`crate::policy::DodTaintPolicy`] liest ihn dort. Trägt die
/// Registry keine solche Politik, fragt der Turn-Loop modusblind nach,
/// sobald [`crate::policy::DodTaintPolicy::fallback_applies`] zutrifft
/// (fail-closed; unter Full Access verletzt das O1 — die Produktivketten
/// müssen die Politik deshalb installieren). Der Taint kann eine
/// Entscheidung nur verschärfen (`Allow` → `AskUser` bzw. → `Deny`), nie
/// lockern.
///
/// Hat der Taint die Rückfrage verursacht (die Politik lieferte `AskUser`
/// oder der Fallback griff) und ist das Ergebnis `AskUser`, gilt:
/// - Kann niemand antworten
///   ([`crate::policy::DodTaintPolicy::ask_answerable`]: kein
///   Freigabe-Akteur oder ein nicht interaktiver Einstieg), wird der Aufruf
///   mit [`crate::dod_taint::unanswerable_reason`] abgelehnt — statt einer
///   Pause, die niemand auflöst (bzw. statt `MissingApprovalActor`).
/// - Sonst vermerkt der Turn die Call-Id
///   ([`crate::dod_taint::DodTaint::note_ask`]); nur dann nennt
///   [`crate::dod_taint::pending_ask_reason`] der Oberfläche den Grund.
pub async fn check_approval(session: &AgentSession, call: &ToolCall) -> ApprovalDecision {
    let tainted = crate::dod_taint::session_turn_tainted(session);
    let (decision, policy_asked) =
        crate::dod_taint::with_review_taint(tainted, aggregate_approval(session, call)).await;
    let handlers = session.registry().approval_handlers();
    // Genau eine Auswertung des Fallbacks: derselbe Befund ändert die
    // Entscheidung und schreibt die Rückfrage dem Taint zu.
    let fallback =
        crate::policy::DodTaintPolicy::fallback_applies(handlers, call, tainted, &decision);
    let decision = if fallback {
        ApprovalDecision::AskUser(harw_types::ItemId::new())
    } else {
        decision
    };
    let taint_caused_ask =
        (policy_asked || fallback) && matches!(decision, ApprovalDecision::AskUser(_));
    if !taint_caused_ask {
        return decision;
    }
    let actor_present = session
        .spawn_context()
        .is_some_and(|context| context.approval_actor.is_some());
    if !crate::policy::DodTaintPolicy::ask_answerable(handlers, actor_present) {
        tracing::info!(
            tool = %crate::dod_taint::display_tool_name(call.name.as_str()),
            actor_present,
            "dod_taint.ask_unanswerable_denied"
        );
        return ApprovalDecision::Deny(crate::dod_taint::unanswerable_reason(call.name.as_str()));
    }
    crate::dod_taint::note_session_ask(session, &call.id);
    tracing::info!(
        reason = %crate::dod_taint::ask_reason(call.name.as_str()),
        fallback,
        "dod_taint.ask"
    );
    decision
}

// Befragt jeden registrierten Handler genau einmal und aggregiert
// `Deny` > `AskUser` > `Allow` (siehe `check_approval`). Der zweite Wert
// meldet, ob die `DodTaintPolicy` (Label `DOD_TAINT_POLICY_LABEL`) selbst
// `AskUser` geliefert hat — so wird die Rückfrage dem Taint nur
// zugeschrieben, wenn er sie verursacht hat.
async fn aggregate_approval(session: &AgentSession, call: &ToolCall) -> (ApprovalDecision, bool) {
    let mut first_ask: Option<ApprovalDecision> = None;
    let mut first_deny: Option<ApprovalDecision> = None;
    let mut taint_policy_asked = false;
    for handler in session.registry().approval_handlers() {
        let decision = handler.review(call).await;
        match decision {
            ApprovalDecision::Allow => {}
            ApprovalDecision::AskUser(_) => {
                if handler.label() == crate::policy::DOD_TAINT_POLICY_LABEL {
                    taint_policy_asked = true;
                }
                if first_ask.is_none() {
                    first_ask = Some(decision);
                }
            }
            ApprovalDecision::Deny(_) => {
                if first_deny.is_none() {
                    first_deny = Some(decision);
                }
            }
        }
    }
    (
        first_deny.or(first_ask).unwrap_or(ApprovalDecision::Allow),
        taint_policy_asked,
    )
}

/// Guardrail-Entscheidungen, die für eine Modellantwort bereits eingeholt
/// wurden, aber noch nicht ausgewertet sind.
///
/// # Beschreibung
/// Der Parallel-Pfad muss jeden Call **vor** dem Start prüfen — sonst würde ein
/// `Deny` erst bemerkt, wenn das Werkzeug bereits läuft. Der sequenzielle Pfad
/// prüft dagegen jeden Call an seiner natürlichen Stelle. Damit ein
/// `ApprovalHandler` nie zweimal zur selben Modellantwort befragt wird, legt die
/// Vorprüfung ihre Entscheidungen hier ab; der sequenzielle Fallback verbraucht
/// sie, statt erneut zu fragen (siehe [`check_approval`]).
///
/// Jeder Eintrag ist bereits die **aggregierte** Entscheidung aller Handler
/// (`Deny` > `AskUser` > `Allow`, siehe [`check_approval`]) — Vorprüfung und
/// sequenzieller Pfad entscheiden damit identisch. Die Vorprüfung bricht beim
/// ersten Call ab, dessen Aggregat nicht `Allow` ist. Für die dahinter
/// liegenden Calls steht deshalb kein Eintrag bereit — die werden im
/// sequenziellen Pfad ganz normal, also ebenfalls genau einmal, geprüft.
///
/// # Nebenläufigkeit
/// Reiner Datenhalter ohne innere Veränderlichkeit; lebt genau eine
/// Modellantwort lang auf dem Stack von `drive_turn`.
#[derive(Debug, Default)]
struct PreparedApprovals {
    /// Entscheidung je Call-Position der Modellantwort. `None` heißt „noch
    /// nicht gefragt", ein verbrauchter Eintrag wird wieder zu `None`.
    decisions: Vec<Option<ApprovalDecision>>,
}

impl PreparedApprovals {
    /// Entnimmt die für `index` bereits eingeholte Entscheidung.
    ///
    /// # Returns
    /// `Some(decision)`, wenn die Vorprüfung diesen Call schon geprüft hat —
    /// der Eintrag ist danach verbraucht. `None`, wenn der Aufrufer selbst
    /// fragen muss.
    fn take(&mut self, index: usize) -> Option<ApprovalDecision> {
        self.decisions.get_mut(index).and_then(Option::take)
    }
}


/// Sammelt alle Tool-Specs über alle `ToolProvider` und filtert nach der
/// session-level [`SessionActivation`][crate::activation::SessionActivation].
///
/// # Description
/// Iterates every registered `ToolProvider`, collects their [`ToolSpec`]s, and
/// removes any tool whose name is disabled by the session activation. Duplicate
/// tool names across providers are still rejected with
/// [`CoreError::DuplicateTool`] regardless of activation state (a disabled tool
/// that shares a name with an enabled one is still a registry error).
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
///
/// # Returns
/// Filtered, de-duplicated list of [`ToolSpec`]s, or a
/// [`CoreError::DuplicateTool`] if any tool name appears more than once.
///
/// # Errors
/// - [`CoreError::DuplicateTool`]: two or more providers registered a tool
///   with the same name.
pub fn collect_tools(session: &AgentSession) -> CoreResult<Vec<ToolSpec>> {
    let activation = session.activation();
    let mut specs = Vec::new();
    let mut names = BTreeSet::new();
    for provider in session.registry().tool_providers() {
        for spec in provider.tools() {
            let name = match &spec {
                ToolSpec::Function(function) => function.name.as_str(),
            };
            if !names.insert(name.to_owned()) {
                return Err(CoreError::DuplicateTool {
                    name: name.to_owned(),
                });
            }
            let tool_name = match &spec {
                ToolSpec::Function(f) => &f.name,
            };
            if activation.is_tool_enabled(tool_name) {
                specs.push(spec);
            }
        }
    }
    // Stabil nach Tool-Namen sortieren: Provider-Prompt-Caches brauchen ein
    // byte-identisches Tool-Array über Runden hinweg; die Registrierungs-
    // reihenfolge der `ToolProvider` ist dafür kein verlässliches Kriterium.
    specs.sort_by(|a, b| a.name().cmp(b.name()));
    // Runde 7, Teil L9: kleine Kontextfenster bekommen kompakte Schemas
    // (deterministisch, damit der Prompt-Cache stabil bleibt).
    if session.compact_tool_schemas() {
        specs = specs.into_iter().map(compact_tool_spec).collect();
    }
    Ok(specs)
}

/// Höchstlänge (Zeichen) einer Werkzeugbeschreibung im kompakten Schema.
pub const COMPACT_TOOL_DESCRIPTION_CHARS: usize = 160;

/// Höchstlänge (Zeichen) einer Parameterbeschreibung im kompakten Schema.
pub const COMPACT_PARAMETER_DESCRIPTION_CHARS: usize = 80;

/// Kompakte Form eines Werkzeugschemas für kleine Kontextfenster (Runde 7,
/// Teil L9).
///
/// # Beschreibung
/// Kürzt die Werkzeugbeschreibung auf ihren ersten Satz (höchstens
/// [`COMPACT_TOOL_DESCRIPTION_CHARS`] Zeichen) und jede Parameter-
/// beschreibung rekursiv auf [`COMPACT_PARAMETER_DESCRIPTION_CHARS`]
/// Zeichen. Namen, Typen, `required`, `enum` und Vorgaben bleiben — der
/// Vertrag des Werkzeugs ändert sich nicht.
///
/// # Arguments
/// - `spec` (`ToolSpec`): das vollständige Schema.
///
/// # Returns
/// Das kompakte Schema.
#[must_use]
pub fn compact_tool_spec(spec: ToolSpec) -> ToolSpec {
    match spec {
        ToolSpec::Function(mut function) => {
            function.description = compact_text(
                first_sentence(&function.description),
                COMPACT_TOOL_DESCRIPTION_CHARS,
            );
            compact_schema_descriptions(&mut function.parameters);
            ToolSpec::Function(function)
        }
    }
}

/// Der erste Satz eines Textes (bis einschließlich `. `, sonst alles).
fn first_sentence(text: &str) -> &str {
    let trimmed = text.trim();
    match trimmed.find(". ") {
        Some(index) => &trimmed[..=index],
        None => trimmed,
    }
}

/// Kürzt auf `max` Zeichen (mit „…").
fn compact_text(text: &str, max: usize) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= max {
        return single_line;
    }
    let mut out: String = single_line.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Kürzt Parameterbeschreibungen rekursiv (Runde 7, Teil L9).
fn compact_schema_descriptions(schema: &mut JsonSchema) {
    if let Some(description) = schema.description.take() {
        schema.description = Some(compact_text(
            &description,
            COMPACT_PARAMETER_DESCRIPTION_CHARS,
        ));
    }
    if let Some(properties) = schema.properties.as_mut() {
        for property in properties.values_mut() {
            compact_schema_descriptions(property);
        }
    }
    if let Some(items) = schema.items.as_mut() {
        compact_schema_descriptions(items);
    }
    if let Some(variants) = schema.any_of.as_mut() {
        for variant in variants.iter_mut() {
            compact_schema_descriptions(variant);
        }
    }
}

/// Sucht den zuständigen `ToolExecutor` für einen Tool-Namen.
///
/// # Description
/// Returns a cloned `Arc` so no borrow on the session is held across the
/// (async) tool execution boundary. Returns `None` if the tool is disabled by
/// the session's [`SessionActivation`][crate::activation::SessionActivation] or
/// if no provider claims the tool.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
/// - `name` (`&ToolName`): the tool to look up.
///
/// # Returns
/// `Some(executor)` if the tool is enabled and a provider declares an executor
/// for it; `None` otherwise.
///
/// # Concurrency
/// Synchronous; safe to call from any thread while holding a shared reference
/// to the session.
pub fn find_executor(session: &AgentSession, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
    if !session.activation().is_tool_enabled(name) {
        return None;
    }
    session
        .registry()
        .tool_providers()
        .iter()
        .find_map(|p| p.executor(name))
}

/// Entfernt Top-Level-Argumente mit dem String `"null"`, die im Schema des
/// Werkzeugs **nicht** `required` sind (Runde 7, Teil B3).
///
/// # Beschreibung
/// Manche Modelle füllen optionale Parameter mit dem String `"null"` statt
/// sie wegzulassen (beobachtet bei `diary`, `fs.glob`, `deps.*`); die
/// Ausführer lasen daraus einen echten Wert (Pfad „null", Filter „null").
/// Nur Top-Level-Felder werden betrachtet; Pflichtfelder bleiben unberührt
/// (dort ist „null" ein — wenn auch seltsamer — Wert des Modells). Ohne
/// bekanntes Schema (Werkzeug unbekannt oder deaktiviert) bleibt der Aufruf
/// unverändert.
///
/// # Arguments
/// - `session` (`&AgentSession`): liefert die Werkzeugschemas.
/// - `calls` (`&mut [ToolCall]`): die Aufrufe einer Modellantwort.
pub(crate) fn normalize_null_string_arguments(session: &AgentSession, calls: &mut [ToolCall]) {
    for call in calls.iter_mut() {
        let has_null_string = call.arguments.as_object().is_some_and(|map| {
            map.values()
                .any(|value| value.as_str() == Some(NULL_STRING_ARGUMENT))
        });
        if !has_null_string {
            continue;
        }
        let Some(schema) = tool_parameters_schema(session, &call.name) else {
            continue;
        };
        let removed = strip_null_string_arguments(&mut call.arguments, &schema);
        if !removed.is_empty() {
            tracing::info!(
                tool = %call.name,
                fields = ?removed,
                "turn_loop.null_string_arguments_removed"
            );
        }
    }
}

/// Der String, den Modelle fälschlich für „nicht gesetzt" schicken.
const NULL_STRING_ARGUMENT: &str = "null";

/// Das Parameterschema eines aktivierten Werkzeugs (Runde 7, Teil B3).
fn tool_parameters_schema(session: &AgentSession, name: &ToolName) -> Option<JsonSchema> {
    if !session.activation().is_tool_enabled(name) {
        return None;
    }
    session
        .registry()
        .tool_providers()
        .iter()
        .flat_map(|provider| provider.tools())
        .find_map(|spec| match spec {
            ToolSpec::Function(function) if &function.name == name => Some(function.parameters),
            ToolSpec::Function(_) => None,
        })
}

/// Entfernt aus `arguments` jedes Top-Level-Feld mit dem String `"null"`,
/// das in `schema.required` nicht vorkommt (Runde 7, Teil B3).
///
/// # Arguments
/// - `arguments` (`&mut serde_json::Value`): die Argumente (nur ein Objekt
///   wird verändert).
/// - `schema` (`&JsonSchema`): das Parameterschema des Werkzeugs.
///
/// # Returns
/// Die Namen der entfernten Felder (sortiert).
pub(crate) fn strip_null_string_arguments(
    arguments: &mut serde_json::Value,
    schema: &JsonSchema,
) -> Vec<String> {
    let Some(map) = arguments.as_object_mut() else {
        return Vec::new();
    };
    let required = schema.required.as_deref().unwrap_or_default();
    let mut removed: Vec<String> = map
        .iter()
        .filter(|(key, value)| {
            value.as_str() == Some(NULL_STRING_ARGUMENT) && !required.iter().any(|r| r == *key)
        })
        .map(|(key, _)| key.clone())
        .collect();
    removed.sort();
    for key in &removed {
        map.remove(key);
    }
    removed
}

/// Resolve an executor only when its provider explicitly declares the tool
/// safe for concurrent execution.
///
/// # Description
/// Like [`find_executor`] but additionally requires the provider to declare
/// the tool as `parallel_safe`. Also checks the session's
/// [`SessionActivation`][crate::activation::SessionActivation]: a disabled tool
/// is never returned even if the provider marks it parallel-safe.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
/// - `name` (`&ToolName`): the tool to look up.
///
/// # Returns
/// `Some(executor)` if the tool is enabled, registered, and parallel-safe;
/// `None` otherwise.
///
/// # Concurrency
/// Synchronous; safe to call from any thread while holding a shared reference
/// to the session.
pub fn find_parallel_executor(
    session: &AgentSession,
    name: &ToolName,
) -> Option<Arc<dyn ToolExecutor>> {
    if !session.activation().is_tool_enabled(name) {
        return None;
    }
    session
        .registry()
        .tool_providers()
        .iter()
        .find_map(|provider| {
            provider
                .parallel_safe(name)
                .then(|| provider.executor(name))
                .flatten()
        })
}

/// Sucht den registrierten `context.load`-Ausführer über [`find_executor`] und
/// belegt sein Kassenbuch für den laufenden Turn vor, sofern die Sitzung ein
/// `ContextProgram` deklariert.
///
/// # Beschreibung
///
/// Schließt den in der Moduldoku (§ "Zweiter Nachtrag", Punkt 4) benannten
/// Weg: von einem echten Eintrittspunkt — [`drive_turn`], vor dem ersten
/// Model-Aufruf eines Turns — über [`find_executor`] und die neue
/// `harw_tools::ToolExecutor::as_context_load_executor`-Methode zu einer
/// konkreten [`harw_tools::ContextLoadExecutor`]-Instanz, und über
/// [`AgentSession::context_program`] zum deklarierten `ContextProgram`
/// derselben Sitzung. **Beide Hälften sind damit von hier aus erreichbar** —
/// die Frage aus dem Knotenauftrag ist positiv beantwortet.
///
/// Aufgerufen wird `seed_turn` mit einer **leeren** `spent_per_section`-Map,
/// nicht mit tatsächlich verbrauchten Kosten je Sektion:
/// [`gather_context`] läuft weiterhin über den alten
/// `Vec<harw_extension_api::ContextFragment>`-Pfad (§ Moduldoku, "Zweiter
/// Nachtrag", Punkt 3) — die neue, `harw_context::Fragment`-bewusste Montage
/// (`crate::context_budget::Assembly<..>`), die tatsächliche Kosten je
/// Sektion kennen würde, läuft in dieser Datei nicht. Eine leere Map ist
/// deshalb keine Vereinfachung, sondern die exakte Angabe: die Montage
/// dieses Turns hat im neuen Kosten-Schema bislang nichts ausgegeben, weil
/// sie das neue Kosten-Schema noch nicht bedient. **Hier bricht die Kette zu
/// `DetailMode::References` deshalb weiterhin** — nicht mehr am fehlenden
/// Weg zum Ausführer (das schließt diese Funktion), sondern unverändert am
/// fehlenden `ContextProvider`, der `harw_context::Fragment` produziert
/// (Blocker 1 der Moduldoku, außerhalb dieses Schreibbereichs). Sobald dieser
/// Provider existiert, ersetzt die tatsächliche `spent_per_section` diese
/// leere Map.
///
/// Eine Sitzung ohne deklariertes `ContextProgram`
/// (`AgentSession::context_program() == None`) bricht diese Funktion sofort
/// ab, ohne `find_executor` überhaupt aufzurufen, und verhält sich damit
/// unverändert — die Auflage, unter der der Vorgängerknoten
/// `with_context_program` eingeführt hat, bleibt gewahrt. Ebenso ein no-op,
/// wenn `context.load` gar nicht registriert oder per
/// `SessionActivation` deaktiviert ist ([`find_executor`] liefert dann
/// `None`).
///
/// # Arguments
/// - `session` (`&AgentSession`): liefert sowohl das optionale
///   `ContextProgram` als auch (über [`find_executor`]) den registrierten
///   `context.load`-Ausführer.
/// - `turn_id` (`&harw_types::TurnId`): der Turn, dessen Kassenbuch vorbelegt
///   wird.
///
/// # Concurrency
/// Synchron; `ContextLoadExecutor::seed_turn` serialisiert intern über ein
/// `Mutex`.
fn seed_context_load_ledger(session: &AgentSession, turn_id: &harw_types::TurnId) {
    if session.context_program().is_none() {
        return;
    }
    let tool_name = ToolName::new(harw_tools::CONTEXT_LOAD_TOOL_NAME);
    let Some(executor) = find_executor(session, &tool_name) else {
        return;
    };
    let Some(context_load_executor) = executor.as_context_load_executor() else {
        return;
    };
    let spent_per_section: BTreeMap<harw_context::SectionName, u32> = BTreeMap::new();
    context_load_executor.seed_turn(turn_id.clone(), spent_per_section);
}

/// Runde 9, E3: `agent.message` an ein eigenes, bereits **beendetes** Kind
/// wird zur Fortsetzung dieses Kindes über `continue_from` — mit der
/// Nachricht als neuem Auftrag — statt mit „kein eigenes, laufendes Kind"
/// zu scheitern (Export: die UIA konnte der Spielleitung nach ihrer Frage
/// nicht mehr antworten).
///
/// # Returns
/// `(rolle, handoff-argumente)` für den Handoff-Pfad; `None` für jeden
/// anderen Aufruf, ein laufendes, fremdes, unbekanntes oder nicht
/// fortsetzbares Kind (dann läuft `agent.message` normal und meldet
/// dieselbe Ablehnung wie bisher). Die Eigentumsprüfung liegt beim Spawner
/// ([`harw_extension_api::AgentSpawner::resumable_child_role`]); der
/// Aufrufer kommt aus der Sitzung, nie aus Modell-Argumenten.
fn message_resume_handoff(
    session: &AgentSession,
    call: &ToolCall,
) -> Option<(String, serde_json::Value)> {
    if call.name.as_str() != crate::child_comms::AGENT_MESSAGE_TOOL {
        return None;
    }
    let field = |name: &str| {
        call.arguments
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let child_id = field("child_id")?;
    let text = field("text")?;
    let role = session
        .registry()
        .spawner()?
        .resumable_child_role(session.id(), child_id)?;
    Some((
        role,
        serde_json::json!({ "task": text, "continue_from": child_id }),
    ))
}

/// Erkennt, ob ein Tool-Call ein Handoff ist (`transfer_to_<role>`).
#[must_use]
pub fn handoff_role(name: &ToolName) -> Option<String> {
    name.as_str()
        .strip_prefix(HANDOFF_PREFIX)
        .filter(|rest| !rest.is_empty())
        .map(ToOwned::to_owned)
}

/// Übersetzt die Platzierung eines Ausführers (R18 D-D) in das Wire-Feld
/// `TurnEvent::ToolCallCompleted::placement`.
///
/// `None` bleibt `None` („unbekannt“): Oberflächen zeigen dann keine
/// Platzierung, statt `host` zu raten.
fn wire_placement(placement: Option<ExecutionPlacement>) -> Option<ToolPlacement> {
    placement.map(|placement| match placement {
        ExecutionPlacement::Host => ToolPlacement::Host,
        ExecutionPlacement::Sandbox => ToolPlacement::Sandbox,
        ExecutionPlacement::Gateway { node } => ToolPlacement::Gateway { node },
    })
}

/// Übersetzt eine `ToolOutput` in das explizite Wire-Ergebnis eines
/// `ToolResultItem`.
fn output_to_result(output: ToolOutput) -> ToolCallResult {
    match output {
        ToolOutput::Text { content } => ToolCallResult::success(serde_json::Value::String(content)),
        ToolOutput::Json { content } => ToolCallResult::success(content),
        ToolOutput::Error { message } => ToolCallResult::error(message),
    }
}

/// Meldet das Ergebnis eines ausgeführten Tool-Aufrufs an den optionalen
/// [`crate::capture::ToolOutcomeObserver`] der Session (Projektgedächtnis-
/// Erfassung, Addendum B).
///
/// # Beschreibung
/// No-op, wenn keine Session einen Beobachter registriert hat (der
/// Normalfall). Der `Arc` wird vor dem Aufruf geklont, um Borrow-Konflikte
/// mit dem übrigen `session`-Zugriff an den Aufrufstellen zu vermeiden.
/// `output_text` ist der reine Text bei einem Text-Erfolg, die kompakte
/// JSON-Form (`serde_json::to_string`, Fallback leerer String) bei einem
/// strukturierten Erfolg, und die Fehlermeldung bei `ToolCallResult::Error`.
/// Wird bewusst **nicht** für abgebrochene (`cancelled`) Turns aufgerufen —
/// die jeweiligen Aufrufstellen lassen diesen Pfad aus.
///
/// # Arguments
/// - `session` (`&AgentSession`): liefert Session-ID und den optionalen
///   Beobachter.
/// - `tool_name` (`&str`): Name des aufgerufenen Tools.
/// - `arguments` (`&serde_json::Value`): die vom Modell übergebenen Argumente.
/// - `result` (`&ToolCallResult`): das endgültige, bereits bekannte Ergebnis.
///
/// # Concurrency
/// Synchron; ruft `ToolOutcomeObserver::on_tool_outcome` direkt aus dem
/// Turn-Loop-Pfad auf. Beobachter müssen billig sein und dürfen nicht
/// fehlschlagen.
fn notify_tool_outcome(
    session: &AgentSession,
    tool_name: &str,
    arguments: &serde_json::Value,
    result: &ToolCallResult,
) {
    // PL-100 Phase 4 (B22): jeder abgefertigte `dod.*`-Aufruf taintet den
    // laufenden Turn — auch ein gescheiterter oder abgelehnter.
    crate::dod_taint::note_session_dispatch(session, tool_name);
    emit_plan_update(session, tool_name, arguments, result);
    // Runde 9, E3: ein Erfolg löst passende Pitfalls (kein veralteter
    // `[harw-Wächter]`-Hinweis an späteren Ergebnissen desselben Werkzeugs).
    if matches!(result, ToolCallResult::Success { .. })
        && let Some(advisor) = session.pitfall_advisor()
    {
        advisor.resolved_in_session(session.id(), tool_name, arguments);
    }
    let Some(observer) = session.tool_outcome_observer().cloned() else {
        return;
    };
    let (status, output_text) = tool_outcome_parts(result);
    observer.on_tool_outcome(
        session.id(),
        &ToolOutcome {
            tool_name,
            arguments,
            status,
            output_text: &output_text,
        },
    );
}

/// Meldet [`TurnEvent::PlanUpdated`] nach einem erfolgreichen Aufruf des
/// Plan-Werkzeugs (Operation `plan` als Modell-Tool). Plan-ID
/// und Revision stammen aus dem Ergebnis, sofern es sie trägt, sonst aus den
/// Argumenten; die Zusammenfassung ist die ausgeführte Aktion.
fn emit_plan_update(
    session: &AgentSession,
    tool_name: &str,
    arguments: &serde_json::Value,
    result: &ToolCallResult,
) {
    let is_plan_tool = tool_name == "plan";
    let ToolCallResult::Success { value } = result else {
        return;
    };
    if !is_plan_tool {
        return;
    }
    let lookup = |key: &str| {
        value
            .get(key)
            .or_else(|| value.get("data").and_then(|data| data.get(key)))
            .or_else(|| arguments.get(key))
    };
    let plan_id = lookup("plan_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("current")
        .to_owned();
    let revision = lookup("revision")
        .and_then(|rev| {
            rev.as_u64()
                .or_else(|| rev.as_str().and_then(|text| text.parse().ok()))
        })
        .unwrap_or(0);
    let summary = arguments
        .get("action")
        .or_else(|| arguments.get("command"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Plan aktualisiert")
        .to_owned();
    emit(
        session,
        TurnEvent::PlanUpdated {
            plan_id,
            revision,
            summary,
        },
    );
}

/// Benachrichtigt den optionalen [`crate::guard::ProgressObserver`] der
/// Session, dass gerade Fortschritt stattgefunden hat (Addendum F+G —
/// Lease-Erneuerung durch `ManagedAgentSpawner`).
///
/// # Beschreibung
/// Wird nach jeder Modellrunde und nach jedem — auch abgelehnten oder
/// fehlgeschlagenen — Tool-Ergebnis aufgerufen: schon der Versuch zeigt, dass
/// die Session noch lebt, unabhängig vom Erfolg des einzelnen Aufrufs.
fn notify_progress(session: &AgentSession) {
    if let Some(observer) = session.progress_observer() {
        observer.on_progress(session.id());
    }
}

/// Wie [`notify_progress`], nach einem Tool-Ergebnis: zählt zusätzlich den
/// Aufruf für die Live-Beobachtung.
fn notify_tool_progress(session: &AgentSession) {
    if let Some(observer) = session.progress_observer() {
        observer.on_tool_call(session.id());
    }
    notify_progress(session);
}

/// Runde 5, Teil M: Runden-Grenze eines Kindes (alle Tool-Ergebnisse der
/// Runde stehen im Verlauf, der nächste Modellaufruf ist noch nicht gebaut).
///
/// # Beschreibung
/// Meldet dem [`crate::guard::ProgressObserver`] den jüngsten
/// Assistententext (Aktivitätsjournal) und hängt die wartenden Nachrichten
/// (`agent.message` vom Elternteil, `parent.message` der Kinder) als
/// Hinweis an das letzte Verlaufs-Item an — provider-gültig, weil das
/// letzte Item hier ein Tool-Ergebnis ist. Ohne Beobachter (Wurzel) ein
/// No-op.
fn deliver_round_boundary(session: &mut AgentSession) {
    let Some(observer) = session.progress_observer().cloned() else {
        return;
    };
    let last_text = session.history().items().iter().rev().find_map(|item| {
        let TurnItem::AssistantMessage(message) = item else {
            return None;
        };
        let text: String = message
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ImageUrl { .. } | ContentPart::Media { .. } => None,
            })
            .collect();
        (!text.trim().is_empty()).then_some(text)
    });
    if let Some(text) = last_text {
        observer.on_assistant_text(session.id(), &text);
    }
    let messages = observer.take_inbound_messages(session.id());
    if messages.is_empty() {
        return;
    }
    let block = messages.join("\n\n");
    if !session.history_mut().append_hint_to_last(&block) {
        let _ = session.history_mut().push_user_text(block);
    }
    tracing::info!(
        session = %session.id(),
        count = messages.len(),
        "turn_loop.inbound_messages_delivered"
    );
}

/// Wie [`notify_progress`], nach einer Modell-Runde: verbucht zusätzlich
/// deren Token-Nutzung.
fn notify_round_progress(session: &AgentSession, usage: &harw_types::TokenUsage) {
    if let Some(observer) = session.progress_observer() {
        observer.on_round_usage(session.id(), usage);
    }
    notify_progress(session);
}

// Status + Textform eines `ToolCallResult`, wie sie sowohl
// `notify_tool_outcome` als auch die Turn-Wächter (Addendum F+G) brauchen —
// gleiche Ableitungsregel wie in `notify_tool_outcome`s Doku beschrieben.
fn tool_outcome_parts(result: &ToolCallResult) -> (ToolOutcomeStatus, String) {
    match result {
        ToolCallResult::Success { value } => {
            let text = match value {
                serde_json::Value::String(text) => text.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            (ToolOutcomeStatus::Success, text)
        }
        ToolCallResult::Error { message } => (ToolOutcomeStatus::Error, message.clone()),
    }
}

/// Befragt den optionalen [`crate::guard::PitfallAdvisor`] der Session vor
/// einer Werkzeugausführung (Addendum F+G).
///
/// # Returns
/// `None`, wenn keine Beratung registriert ist oder kein Pitfall zutrifft.
/// Sonst ein bereits mit `[harw-Wächter] ` präfixierter Hinweistext, den der
/// Aufrufer an das Ergebnis **desselben** Aufrufs anhängen soll — der Treffer
/// selbst ist bereits über [`report_drift`] gemeldet.
async fn apply_pitfall_advice(
    session: &AgentSession,
    store: &dyn StateStore,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> Option<String> {
    let advisor = session.pitfall_advisor()?.clone();
    let hint = advisor.advise_in_session(session.id(), tool_name, arguments)?;
    let event = DriftEvent {
        kind: DriftKind::PitfallMatch,
        session_id: session.id().to_string(),
        detail: format!("Pitfall-Treffer für `{tool_name}`"),
        tool_name: Some(tool_name.to_owned()),
        child_role: None,
    };
    report_drift(session, store, &event).await;
    Some(format!("[harw-Wächter] {hint}"))
}

/// Meldet, ob ein Aufruf von den sitzungsweiten Wächtern (Runde 7, Teile
/// A2/A5) betroffen sein kann — dann läuft die Antwort sequenziell, damit
/// Zählung, Ablehnung und Hinweis je Aufruf in Reihenfolge greifen.
fn session_guard_applies(session: &AgentSession, tool_name: &str) -> bool {
    session.guard_policy().enabled
        && (tool_name == crate::guard::AGENT_STATUS_TOOL_NAME
            || (session.is_read_budgeted_orchestrator()
                && crate::guard::is_orchestrator_read_tool(tool_name)))
}

/// Befragt die sitzungsweiten Wächter **vor** einer Werkzeugausführung
/// (Runde 7, Teile A2/A5).
///
/// # Beschreibung
/// - **Lesebudget** (A2): nur für Orchestrator-Kindsitzungen
///   ([`AgentSession::is_read_budgeted_orchestrator`]); ab dem
///   `orchestrator_read_warn`-ten Lesezugriff ein Hinweis, über
///   `orchestrator_read_limit` hinaus eine Ablehnung.
/// - **Polling** (A5): `agent.status` auf dieselbe `child_id` binnen 30 s
///   bekommt einen Hinweis.
///
/// Jeder Befund wird über [`report_drift`] gemeldet; ein Hinweis wird an
/// `pending_hint` angehängt (und von [`apply_tool_guard`] an das Ergebnis
/// dieses Aufrufs gehängt).
///
/// # Returns
/// `Some(meldung)`, wenn der Aufruf abgelehnt werden muss (die Meldung ist
/// sein Fehlerergebnis); sonst `None`.
async fn apply_session_guard(
    session: &mut AgentSession,
    store: &dyn StateStore,
    pending_hint: &mut Option<String>,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> Option<String> {
    if !session_guard_applies(session, tool_name) {
        return None;
    }
    let policy = session.guard_policy();
    let session_id = session.id().clone();
    let mut verdicts = Vec::with_capacity(2);
    if session.is_read_budgeted_orchestrator() {
        verdicts.push(session.guard_state_mut().observe_orchestrator_read(
            &policy,
            &session_id,
            tool_name,
        ));
    }
    verdicts.push(session.guard_state_mut().observe_status_poll(
        &policy,
        &session_id,
        tool_name,
        arguments,
        Instant::now(),
    ));
    let mut denied = None;
    for verdict in verdicts {
        match verdict {
            SessionGuardVerdict::Continue => {}
            SessionGuardVerdict::Warn { event, hint } => {
                report_drift(session, store, &event).await;
                *pending_hint = Some(match pending_hint.take() {
                    Some(existing) => format!("{existing}\n\n{hint}"),
                    None => hint,
                });
            }
            SessionGuardVerdict::Deny { event, message } => {
                report_drift(session, store, &event).await;
                tracing::warn!(tool = tool_name, "turn_loop.read_budget_denied");
                denied = Some(message);
            }
        }
    }
    denied
}

/// Vermerkt den Abbruch durch einen Turn-Wächter als konkreten Endgrund im
/// Steuerblock des laufenden Turns ([`TurnControl::stop_detail`]), damit ein
/// Kind-Endbericht „Turn-Wächter (no_progress_rounds): …" statt einer
/// Vermutung nennt.
fn note_guard_stop(session: &AgentSession, event: &crate::guard::DriftEvent) {
    if let Some(control) = session.active_turn_control() {
        let detail: String = event.detail.chars().take(200).collect();
        control.note_stop_detail(format!("Turn-Wächter ({}): {detail}", event.kind.key()));
    }
}

/// Wendet die Turn-Wächter (Addendum F+G) auf ein einzelnes, bereits
/// berechnetes Tool-Ergebnis an.
///
/// # Beschreibung
/// No-op (liefert `None`), wenn kein [`TurnGuard`] aktiv ist. Sonst:
/// vermerkt eine neue erfolgreiche Signatur für die Fortschritts-Erkennung
/// der Runde (`round_progressed`), wertet
/// [`TurnGuard::observe_tool_result`] aus und hängt bei `Warn` den
/// kombinierten Hinweis (ein ggf. aus einer vorherigen Runde übrig
/// gebliebener `pending_hint` plus der neue Hinweis) über [`append_hint`] an
/// `result` an. Ein Hinweis, der keinem Aufruf dieser Runde mehr zugeordnet
/// werden kann, bleibt in `pending_hint` für das nächste Tool-Ergebnis des
/// Turns stehen.
///
/// Für [`crate::guard::POLLING_TOOLS`] zählt ein wiederholter *erfolgreicher*
/// Aufruf mit bereits gesehener Signatur trotzdem als Rundenfortschritt
/// (Fix C, Moduldoku „Achter Nachtrag") — legitimes Polling eines laufenden
/// Hintergrundprozesses soll nicht als Stillstand erkannt werden.
///
/// # Returns
/// `None`, wenn der Turn weiterläuft; `Some(reason)`, wenn ein `Abort`-Befund
/// den Turn beenden muss — der Aufrufer beendet ihn dann wie bei
/// ausgeschöpftem `max_model_rounds` (siehe [`cancel_turn`]/
/// [`cancel_turn_with_pending_calls`]), ohne eine neue `CoreError`-Variante.
#[allow(clippy::too_many_arguments)]
async fn apply_tool_guard(
    session: &AgentSession,
    store: &dyn StateStore,
    guard: Option<&mut TurnGuard>,
    seen_success_signatures: &mut HashSet<String>,
    pending_hint: &mut Option<String>,
    round_progressed: &mut bool,
    tool_name: &str,
    arguments: &serde_json::Value,
    result: &mut ToolCallResult,
) -> Option<CancelReason> {
    let guard = guard?;
    let (status, output_text) = tool_outcome_parts(result);
    if status == ToolOutcomeStatus::Success {
        let signature = crate::guard::call_signature(tool_name, arguments);
        // Fix C (Moduldoku „Achter Nachtrag"): eine bereits gesehene Signatur
        // zählt normalerweise nicht erneut als Fortschritt — für
        // `crate::guard::POLLING_TOOLS` (z. B. `shell.exec`) ist ein
        // wiederholter *erfolgreicher* Aufruf mit identischen Argumenten
        // jedoch legitimes Warten auf einen laufenden Hintergrundprozess,
        // kein Stillstand, und zählt deshalb unabhängig davon, ob die
        // Signatur neu ist.
        let is_polling_tool = crate::guard::POLLING_TOOLS.contains(&tool_name);
        if seen_success_signatures.insert(signature) || is_polling_tool {
            *round_progressed = true;
        }
    }
    let verdict = guard.observe_tool_result(tool_name, arguments, status, &output_text);
    let mut hint_to_apply = pending_hint.take();
    let abort_reason = match verdict {
        GuardVerdict::Continue => None,
        GuardVerdict::Warn { event, hint } => {
            report_drift(session, store, &event).await;
            hint_to_apply = Some(match hint_to_apply {
                Some(existing) => format!("{existing}\n\n{hint}"),
                None => hint,
            });
            None
        }
        GuardVerdict::Abort { event, hint } => {
            report_drift(session, store, &event).await;
            tracing::warn!(hint = %hint, "turn_loop.guard_abort");
            note_guard_stop(session, &event);
            Some(CancelReason::Budget)
        }
    };
    if let Some(hint) = hint_to_apply {
        append_hint(result, &hint);
    }
    abort_reason
}

/// Meldet ein einzelnes erkanntes Drift-Ereignis an `StateStore::record_drift`
/// und den optionalen [`crate::guard::DriftObserver`] der Session (Addendum
/// F+G).
///
/// # Beschreibung
/// Beide Meldewege laufen unabhängig voneinander: ein fehlschlagender
/// `record_drift`-Aufruf (Persistenz) wird nur geloggt (`tracing::warn!`) und
/// verhindert nicht, dass der synchrone [`crate::guard::DriftObserver`]
/// trotzdem benachrichtigt wird — dieselbe Best-Effort-Haltung wie
/// `StateStore::record_usage`.
///
/// # Concurrency
/// `async` nur wegen `store.record_drift`; der Beobachter-Aufruf selbst ist
/// synchron.
async fn report_drift(session: &AgentSession, store: &dyn StateStore, event: &DriftEvent) {
    if let Err(error) = store.record_drift(session.id(), event).await {
        tracing::warn!(%error, kind = event.kind.key(), "turn_loop.record_drift_failed");
    }
    if let Some(observer) = session.drift_observer() {
        observer.on_drift(event);
    }
}

/// Hängt einen Wächter-Hinweis (Addendum F+G, Präfix `[harw-Wächter] `) an ein
/// [`ToolCallResult`] an, statt es zu ersetzen — das Modell sieht das
/// tatsächliche Tool-Ergebnis weiterhin vollständig.
///
/// # Arguments
/// - `result` (`&mut ToolCallResult`): wird um `hint` erweitert (String-Erfolg
///   bzw. Fehlermeldung bekommt `hint` angehängt; ein strukturierter
///   Erfolgswert wird dafür zu seiner kompakten JSON-Textform samt `hint`).
/// - `hint` (`&str`): bereits fertig formatierter Hinweistext.
fn append_hint(result: &mut ToolCallResult, hint: &str) {
    match result {
        ToolCallResult::Success { value } => match &mut *value {
            serde_json::Value::String(text) => {
                text.push_str("\n\n");
                text.push_str(hint);
            }
            other => {
                let rendered = serde_json::to_string(other).unwrap_or_default();
                *value = serde_json::Value::String(format!("{rendered}\n\n{hint}"));
            }
        },
        ToolCallResult::Error { message } => {
            message.push_str("\n\n");
            message.push_str(hint);
        }
    }
}

/// Builds the authority passed across the final tool-execution boundary.
///
/// This is deliberately derived from session state rather than `TurnInput`
/// or `ToolCall.arguments`: both of those can originate with untrusted model
/// or channel input. A session with tools but no resolved sandbox is rejected
/// instead of falling back to ambient host permissions.
// `cancel` (W3/C-CANCEL): the turn's `CancelToken`, attached to the built
// `ToolExecutionContext` so a long-running executor (process, MCP,
// context-load) can observe cancellation mid-call — see
// `ToolExecutionContext::with_cancel`'s doc.
fn tool_execution_context(
    session: &AgentSession,
    ctx: &TurnInputContext,
    cancel: &CancelToken,
) -> CoreResult<ToolExecutionContext> {
    let sandbox = session
        .spawn_context()
        .map(|context| context.sandbox.clone())
        .ok_or_else(|| CoreError::MissingToolExecutionContext {
            session_id: session.id().to_string(),
        })?;
    Ok(
        ToolExecutionContext::new(ctx.session_id.clone(), ctx.turn_id.clone(), sandbox)
            .with_cancel(cancel.clone()),
    )
}

/// Converts the one recoverable authority-boundary rejection into a result
/// the requesting model can observe and react to. The typed error is retained
/// for every other caller; no executor is ever reached without this context.
fn missing_tool_execution_context_result(error: CoreError) -> CoreResult<ToolCallResult> {
    match error {
        missing @ CoreError::MissingToolExecutionContext { .. } => {
            Ok(ToolCallResult::error(missing.to_string()))
        }
        other => Err(other),
    }
}

/// Retrieves immutable child authority. Handoffs are also execution: allowing
/// one without a resolved parent context would let a legacy spawner create a
/// child with ambient permissions.
fn governed_spawn_context(session: &AgentSession) -> CoreResult<SpawnContext> {
    session
        .spawn_context()
        .cloned()
        .ok_or_else(|| CoreError::MissingToolExecutionContext {
            session_id: session.id().to_string(),
        })
}

// ---------------------------------------------------------------------------
// Der eigentliche Turn-Loop
// ---------------------------------------------------------------------------

/// Führt einen kompletten Turn aus: von `Idle` über den Model-/Tool-Zyklus bis
/// zum Turn-Ende — oder bis zu einem Handoff-/Approval-Pausepunkt.
///
/// Schritte: Context → Instructions → Observer → Model-Call → Tool-Call-Loop
/// (Guardrail → Handoff-Detection → Execute → ToolResult → next Model-Call)
/// → Turn-Ende. A normal `ToolExecutor`, including one that wraps a bounded
/// micro-agent, returns its output to this requesting session as a
/// `ToolResult`; the loop then invokes this same session's model again. A
/// `transfer_to_*` call is deliberately different: it transfers ownership and
/// pauses this session for [`resume_after_child`].
pub async fn run_turn(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    input: TurnInput,
) -> CoreResult<TurnOutcome> {
    run_turn_with_approvals(session, model, store, None, input).await
}

/// Runs a turn while durably recording every approval request before it is
/// exposed to a channel. Production channels must use this entrypoint; the
/// plain [`run_turn`] function remains a lightweight test/demo seam.
pub async fn run_turn_durable(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: &ApprovalStore,
    input: TurnInput,
) -> CoreResult<TurnOutcome> {
    run_turn_with_approvals(session, model, store, Some(approvals), input).await
}

/// Rewind and the replacement prompt are one durable frame. Publish the in-memory
/// change only after the durable writer accepts it, then continue the one turn.
async fn replace_prompt(
    session: &mut AgentSession,
    store: &dyn StateStore,
    target: &str,
    text: String,
    images: Vec<harw_protocol::MediaRef>,
    media_check: Option<Arc<RewindMediaCheck>>,
    replacement_id: Option<harw_types::ItemId>,
) -> CoreResult<()> {
    use harw_protocol::items::{ContentPart, TurnItem, UserMessageItem};
    if !images.is_empty() {
        return Err(CoreError::TurnRejected("rewind reuses stored media, not client image payloads".to_owned()));
    }
    if !matches!(
        session.state(),
        crate::session::SessionState::Idle | crate::session::SessionState::Failed(_)
    ) {
        return Err(CoreError::TurnRejected("rewind needs an idle session".to_owned()));
    }
    let mut history = if session.history().is_empty() {
        store.load_history(session.id()).await
            .map_err(|error| state_store_error(error, "rewind history load"))?
    } else {
        session.history().clone()
    };
    let mut matches = history.items().iter().enumerate().filter_map(|(index, item)| {
        matches!(item, harw_protocol::items::TurnItem::UserMessage(m) if m.id.as_str() == target)
            .then_some(index)
    });
    let index = matches.next().ok_or_else(|| CoreError::TurnRejected(
        "rewind target is not a user message of this session".to_owned()
    ))?;
    if matches.next().is_some() {
        return Err(CoreError::TurnRejected("rewind target is ambiguous".to_owned()));
    }
    let original = match &history.items()[index] {
        TurnItem::UserMessage(user) => user,
        _ => return Err(CoreError::TurnRejected("rewind target changed".to_owned())),
    };
    if original.content.is_empty() || original.content.iter().any(|part| matches!(part, ContentPart::ImageUrl { .. })) {
        return Err(CoreError::TurnRejected("rewind target contains unavailable prompt media".to_owned()));
    }
    let refs = original.content.iter().filter_map(|part| match part {
        ContentPart::Media { media, .. } => Some(media.clone()),
        _ => None,
    }).collect::<Vec<_>>();
    if !refs.is_empty() {
        let check = media_check.ok_or_else(|| CoreError::TurnRejected(
            "rewind cannot verify the original stored images".to_owned(),
        ))?;
        let intact = tokio::task::spawn_blocking(move || refs.iter().all(|reference| check(reference)))
            .await.unwrap_or(false);
        if !intact {
            return Err(CoreError::TurnRejected("rewind original images are unavailable".to_owned()));
        }
    }
    let content = if text.is_empty() {
        original.content.clone()
    } else {
        let mut retained = original.content.iter().filter(|part| matches!(part, ContentPart::Media { .. }))
            .cloned().collect::<Vec<_>>();
        retained.push(ContentPart::Text { text });
        retained
    };
    let id = replacement_id.unwrap_or_default();
    if history.items().iter().any(|item| {
        matches!(item, TurnItem::UserMessage(user) if user.id == id)
    }) {
        return Err(CoreError::TurnRejected("rewind replacement identity already exists".to_owned()));
    }
    history.truncate(index);
    history.extend([TurnItem::UserMessage(UserMessageItem { id, content })]);
    store.save_history_with_rewind_boundary(session.id(), &history, Some(target)).await
        .map_err(|error| state_store_error(error, "rewind history save"))?;
    *session.history_mut() = history;
    Ok(())
}

async fn run_turn_with_approvals(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    mut input: TurnInput,
) -> CoreResult<TurnOutcome> {
    if let Some(target) = input.rewind_before.as_deref() {
        let text = input.user_text.take().ok_or_else(|| CoreError::TurnRejected(
            "rewind requires an explicit edit or retry prompt".to_owned(),
        ))?;
        if !text.is_empty() && text.trim().is_empty() {
            return Err(CoreError::TurnRejected("rewind edit is blank".to_owned()));
        }
        replace_prompt(
            session, store, target, text, std::mem::take(&mut input.user_images),
            input.control.rewind_media(),
            input.control.rewrite_replacement_id(),
        ).await?;
        input.control.notify_history_replaced();
    }
    let handle = session
        .try_start_turn()
        .map_err(|r| CoreError::TurnRejected(r.to_string()))?;
    let turn_id = handle.turn_id.clone();
    let session_id = handle.session_id.clone();
    // Siehe Moduldoku „Siebter Nachtrag": der von `TurnInput::with_control`
    // gesetzte Steuerblock wird hier zusätzlich auf der Session hinterlegt
    // (`AgentSession::set_active_turn_control`), damit `resume_after_child`/
    // `resume_after_approval` ihn nach einer Handoff-/Rückfrage-Pause
    // zurücklesen können, statt sich einen frischen, unbegrenzten Block zu
    // bauen.
    // Ein Aufrufer ohne eigene Grenzen (TUI, Gateway: `TurnControl::new()`)
    // bekommt die Vorgabe-Grenzen der Session (aus dem Budget des Laufs,
    // `AgentSession::with_default_turn_limits`); explizit gesetzte Grenzen
    // haben Vorrang.
    let mut control = match session.default_turn_limits() {
        Some(limits) if *input.control.limits() == TurnLimits::unlimited() => {
            input.control.clone().with_limits(limits)
        }
        _ => input.control.clone(),
    };
    session.set_active_turn_control(control.clone());

    // Outer span covering the entire turn's lifecycle.
    let turn_span = tracing::info_span!(
        "agent.turn",
        turn_id = %turn_id,
        session_id = %session_id,
    );
    let _turn_guard = turn_span.enter();

    let ctx = TurnInputContext {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        metadata: input.metadata,
    };

    // A freshly-created in-memory session may be resuming a durable session.
    // Hydrate it before appending the new user item so the model sees the
    // complete transcript and the live session remains the single append
    // authority for the rest of this turn.
    let has_user_input = input.user_text.is_some() || !input.user_images.is_empty();
    if has_user_input && session.history().is_empty() {
        let history = match store.load_history(session.id()).await {
            Ok(history) => history,
            Err(error) => {
                let error = state_store_error(error, "history load");
                transition_after_turn_failure(session, &ctx, &error);
                return Err(error);
            }
        };
        *session.history_mut() = history;
        // PL-97 H7: wie `hydrate_from_store` auch den Skill-Injektor.
        session.restore_skill_injector_from_store(store).await;
    }

    // User-Input in den Verlauf spielen und persistieren.
    if has_user_input {
        if let Some(human) = control.title_seed_human() {
            // Actual UserMessage rows only; the fourth is enough to disqualify any incomplete
            // first-three index. Let canonical persistence proceed even if its optional index fails.
            let preceding_users = session.history().items().iter()
                .filter(|item| matches!(item, TurnItem::UserMessage(_)))
                .take(4)
                .count();
            let pending = match store.begin_title_seed(
                &ctx.session_id, &ctx.turn_id, human, preceding_users,
            ).await {
                Ok(pending) => pending,
                Err(_) => {
                    tracing::warn!("optional title source admission was not confirmed");
                    false
                }
            };
            control.set_title_seed_pending(pending);
            session.set_active_turn_control(control.clone());
        }
        let text = input.user_text.unwrap_or_default();
        if input.user_images.is_empty() {
            session.history_mut().push_user_text(text);
        } else {
            session
                .history_mut()
                .push_user_message(text, input.user_images);
        }
        // Request replay is handled by the transport's submission ID. Equal
        // content is not equal identity: collapsing legitimately repeated
        // messages destroys the durable anchors used by fork/edit/retry.
        let persisted = if control.title_seed_pending() {
            match persist_title_seed_last(session, store).await {
                Ok(location) => { bind_title_user(store, &ctx, location).await; Ok(()) }
                Err(error) => Err(error),
            }
        } else {
            persist_last(session, store).await
        };
        if let Err(error) = persisted {
            transition_after_turn_failure(session, &ctx, &error);
            return Err(error);
        }
    }

    // Addendum D: harte Verdichtung am Beginn eines neuen Auftrags-Turns
    // (Orchestrator-Schicht) — vor der ersten Modellrunde dieses Turns, aber
    // nach dem Einspielen des User-Inputs, damit die Schätzung den vollen,
    // aktuellen Verlauf sieht. `resume_after_child`/`resume_after_approval`
    // laufen nicht durch diese Funktion und sind damit keine Auftragsgrenze.
    if input.rewind_before.is_none() {
        maybe_hard_compact_at_turn_start(session, model, store).await;
    }

    notify_turn_start(
        session,
        &TurnStartInput {
            session_id,
            turn_id,
        },
    );

    let title_seed_pending = control.title_seed_pending();
    let result = drive_turn(session, model, store, approvals, &ctx, handle, control).await;
    if title_seed_pending { settle_title_seed_failure(store, &ctx, &result).await; }
    if let Err(error) = &result {
        transition_after_turn_failure(session, &ctx, error);
    }
    result
}

/// Returns the session to `Idle` after a provider-declared retryable failure,
/// so a caller can submit the same valid session again. All other failures are
/// terminal because they may indicate a broken invariant or unsafe execution
/// boundary.
///
/// # Description
/// Retryability is delegated to [`crate::model::ModelError::is_retryable`]
/// rather than hardcoded here: any [`CoreError::Model`] whose inner
/// [`crate::model::ModelError`] reports itself as retryable (currently
/// `Transient`, `Timeout`, `RateLimited`) returns the session to `Idle`.
/// Everything else — including non-`Model` `CoreError` variants and
/// non-retryable `ModelError` variants such as `Refusal` or `Auth` — calls
/// [`AgentSession::fail`], moving the session to the terminal `Failed` state.
fn transition_after_turn_failure(
    session: &mut AgentSession,
    ctx: &TurnInputContext,
    error: &CoreError,
) {
    let retryable = matches!(
        error,
        CoreError::Model(model_error) if model_error.is_retryable()
    );
    // Jeder Modellfehler (auch HTTP 400/401) beendet nur diesen Turn: Die Session kehrt nach
    // `Idle` zurück, damit die nächste Nachricht normal läuft, statt als „session not idle:
    // Failed" abgewiesen zu werden. Die Fehlermeldung bleibt trotzdem sichtbar.
    let recover_to_idle = retryable || matches!(error, CoreError::Model(_));

    if recover_to_idle {
        // `transition_after_turn_failure` liefert `()` zurück, ein `?` ist
        // hier also nicht möglich — der Fehler wird stattdessen mit vollem
        // Kontext protokolliert. Die Session bleibt in diesem seltenen Fall
        // in ihrem bisherigen Zustand hängen (weder `Idle` noch `Failed`),
        // statt einen zweiten, hier nicht belegbaren Fehler zu erfinden.
        if let Err(complete_error) = session.complete_turn(
            TurnHandle {
                turn_id: ctx.turn_id.clone(),
                session_id: ctx.session_id.clone(),
            },
            harw_types::TokenUsage::default(),
        ) {
            tracing::error!(
                turn_id = %ctx.turn_id,
                session_id = %ctx.session_id,
                original_error = %error,
                complete_error = %complete_error,
                "turn.retry_complete_failed",
            );
        }
    } else {
        session.fail(error.to_string());
    }

    // Der Fehler wird Teil des Verlaufs (sichtbar in Export/Resume) und
    // zusätzlich als Session-Ereignis gemeldet.
    session
        .history_mut()
        .push_error(error.to_string(), retryable);
    session.report_error(error.to_string(), retryable);

    emit(
        session,
        TurnEvent::TurnFailed {
            turn_id: ctx.turn_id.clone(),
            reason: error.to_string(),
            retryable,
        },
    );
}

/// Nimmt einen pausierten Turn nach Abschluss eines Child-Handoffs wieder auf.
///
/// Das Child-Ergebnis wird als `ToolResult` in den Eltern-Verlauf gespielt
/// (Agents-SDK-Handoff-Semantik), dann läuft der Model-/Tool-Zyklus weiter.
pub async fn resume_after_child(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    child: SessionId,
    call_id: ToolCallId,
    child_result: ToolCallResult,
) -> CoreResult<TurnOutcome> {
    resume_after_child_with_approvals(session, model, store, None, child, call_id, child_result)
        .await
}

/// Durable counterpart to [`resume_after_child`]. Use it when the parent was
/// started through [`run_turn_durable`] so any subsequent approval pause is
/// persisted in the same authority store.
pub async fn resume_after_child_durable(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: &ApprovalStore,
    child: SessionId,
    call_id: ToolCallId,
    child_result: ToolCallResult,
) -> CoreResult<TurnOutcome> {
    resume_after_child_with_approvals(
        session,
        model,
        store,
        Some(approvals),
        child,
        call_id,
        child_result,
    )
    .await
}

async fn resume_after_child_with_approvals(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    child: SessionId,
    call_id: ToolCallId,
    child_result: ToolCallResult,
) -> CoreResult<TurnOutcome> {
    let turn_id = session
        .current_turn()
        .cloned()
        .ok_or_else(|| CoreError::TurnRejected("resume without an active turn".to_owned()))?;
    let session_id = session.id().clone();

    // Re-enter the same logical turn span after child completion.
    let turn_span = tracing::info_span!(
        "agent.turn",
        turn_id = %turn_id,
        session_id = %session_id,
    );
    let _turn_guard = turn_span.enter();

    let ctx = TurnInputContext {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        metadata: serde_json::Value::Null,
    };

    let call_id_for_completion = call_id.clone();
    // Runde 5, Teil K: ein im Hintergrund weiterlaufendes Kind ist nicht
    // fertig — sein `ChildCompleted` meldet der Hintergrund-Treiber beim
    // echten Ende.
    let runs_in_background = session
        .registry()
        .spawner()
        .is_some_and(|spawner| spawner.child_runs_in_background(&child));
    let child_outcome = match &child_result {
        ToolCallResult::Success { .. } => "completed",
        _ => "failed",
    };
    let child_duration_ms = session.take_handoff_elapsed_ms();
    // Runde 5, Teil O: das echte Ergebnis mit echter Dauer (vorher 0 ms) —
    // und unten ein `ToolCallCompleted` für den Handoff-Aufruf. Ohne dieses
    // Ereignis blieb die Werkzeugzelle der TUI für `transfer_to_*` auf
    // „läuft“ stehen und wurde beim nächsten Turn-Abbruch nachträglich als
    // „unvollständig (abgebrochen)“ exportiert, obwohl das Kind längst
    // erfolgreich geendet hatte.
    let completed_event = TurnEvent::ToolCallCompleted {
        turn_id: turn_id.clone(),
        call_id: call_id.clone(),
        result: child_result.clone(),
        duration_ms: child_duration_ms,
        placement: None,
    };
    session
        .history_mut()
        .push_tool_result(call_id, child_result, child_duration_ms);
    if let Err(error) = persist_last(session, store).await {
        transition_after_turn_failure(session, &ctx, &error);
        return Err(error);
    }
    // Runde 5, Teil O: erst nach dem Persistieren melden (wie jedes andere
    // Werkzeugergebnis).
    emit(session, completed_event);

    // Do not release the child's admission until its terminal ToolResult is
    // durable in the parent transcript. A failed write leaves the lease intact
    // for recovery. The durable path lets the spawner durably complete that
    // lease before releasing it; the legacy path retains child_finished.
    session.child_completed(&child, &call_id_for_completion)?;

    // Erst hier melden: das Ergebnis ist persistiert *und* die Session hat die
    // Korrelation gegen ihren offenen Handoff bestätigt. Vorher wäre es die
    // Meldung eines Kindes, das gar nicht das erwartete sein muss.
    //
    // `duration_ms` misst ab `begin_handoff` (Wanduhr des Eltern-Turns);
    // `outcome` folgt dem Ergebnis-Typ des Kindes.
    if !runs_in_background {
        emit(
            session,
            TurnEvent::ChildCompleted {
                turn_id: turn_id.clone(),
                child: child.clone(),
                outcome: child_outcome.to_owned(),
                duration_ms: child_duration_ms,
            },
        );
    }
    if let Some(spawner) = session.registry().spawner() {
        if approvals.is_some() {
            let completion = spawner
                .child_completed(&child, jiff::Timestamp::now())
                .map_err(|error| CoreError::HandoffFailed {
                    role: "child completion".to_owned(),
                    reason: error.to_string(),
                });
            if let Err(error) = completion {
                transition_after_turn_failure(session, &ctx, &error);
                return Err(error);
            }
        } else {
            spawner.child_finished(&child);
        }
    }

    let handle = TurnHandle {
        turn_id,
        session_id,
    };

    // Moduldoku „Siebter Nachtrag": der Steuerblock des ursprünglichen Turns
    // überlebt jetzt die Handoff-Pause auf der Session
    // (`AgentSession::set_active_turn_control`) — nur wenn dort wider
    // Erwarten nichts hinterlegt ist (Session nie über `run_turn`/
    // `run_turn_durable` gestartet, z. B. direkter `try_start_turn` in
    // Tests), fällt dies auf einen frischen, unbegrenzten Block zurück.
    let control = session
        .active_turn_control()
        .cloned()
        .unwrap_or_else(TurnControl::new);
    let title_seed_pending = control.title_seed_pending();
    let result = drive_turn(session, model, store, approvals, &ctx, handle, control).await;
    if title_seed_pending { settle_title_seed_failure(store, &ctx, &result).await; }
    if let Err(error) = &result {
        transition_after_turn_failure(session, &ctx, error);
    }
    result
}

/// Resume an approval pause using the exact tool call captured by the session.
/// This prevents an approval callback from substituting different arguments.
pub async fn resume_after_approval(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    actor: ApprovalActor,
    resolution: ApprovalResolution,
) -> CoreResult<TurnOutcome> {
    resume_after_approval_with_store(session, model, store, None, actor, resolution).await
}

/// Resumes an approval only after atomically consuming the durable request.
/// Duplicate callbacks, mismatched actors, and post-restart replays therefore
/// cannot execute the stored tool call a second time.
pub async fn resume_after_approval_durable(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: &ApprovalStore,
    actor: ApprovalActor,
    resolution: ApprovalResolution,
) -> CoreResult<TurnOutcome> {
    resume_after_approval_with_store(session, model, store, Some(approvals), actor, resolution)
        .await
}

async fn resume_after_approval_with_store(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    actor: ApprovalActor,
    resolution: ApprovalResolution,
) -> CoreResult<TurnOutcome> {
    if let Some(approvals) = approvals {
        let pending = session.pending_approval().ok_or_else(|| {
            CoreError::TurnRejected("durable approval resume without a pending request".to_owned())
        })?;
        let decision = match &resolution {
            ApprovalResolution::Approve => ReviewDecision::Approved,
            ApprovalResolution::Reject { .. } => ReviewDecision::Rejected,
        };
        let comment = match &resolution {
            ApprovalResolution::Approve => None,
            ApprovalResolution::Reject { reason } => Some(reason.clone()),
        };
        approvals.resolve(
            session.id(),
            &pending.request,
            decision,
            comment,
            &actor,
            &SystemClock,
        )?;
    }
    let pending = session.resolve_approval(&actor)?;
    let turn_id = session.current_turn().cloned().ok_or_else(|| {
        CoreError::TurnRejected("approval resume without an active turn".to_owned())
    })?;
    let session_id = session.id().clone();

    // Re-enter the logical turn span after approval resolution.
    let turn_span = tracing::info_span!(
        "agent.turn",
        turn_id = %turn_id,
        session_id = %session_id,
    );
    let _turn_guard = turn_span.enter();

    let ctx = TurnInputContext {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        metadata: serde_json::Value::Null,
    };
    let handle = TurnHandle {
        turn_id,
        session_id,
    };
    // Moduldoku „Siebter Nachtrag": wie in `resume_after_child_with_approvals`
    // — der Steuerblock des ursprünglichen Turns überlebt die Approval-Pause
    // auf der Session; nur ohne hinterlegten Block (Session nie über
    // `run_turn`/`run_turn_durable` gestartet) entsteht hier ein frischer.
    let control = session
        .active_turn_control()
        .cloned()
        .unwrap_or_else(TurnControl::new);

    // Fix A (Moduldoku „Achter Nachtrag"): jeder Fehlerausstieg dieser
    // Funktion nach erfolgreich abgeschlossenem `resolve_approval` (oben) lief
    // bisher direkt aus der Funktion heraus (`?`, `return …`), ohne
    // `transition_after_turn_failure` zu durchlaufen — die Session blieb dann
    // dauerhaft in `Running` hängen, obwohl `run_turn_with_approvals` und
    // `resume_after_child_with_approvals` dieses Muster bereits korrekt
    // anwenden. Der gesamte Auflösungspfad läuft deshalb jetzt in einem
    // inneren async-Block: `return`/`?` darin beenden nur diesen Block
    // (dieselbe Scope-Regel wie bei Closures), nicht die Funktion, sodass
    // genau ein `Result` unten geprüft und — im Fehlerfall genau einmal — die
    // Übergangsfunktion aufgerufen wird, bevor es zurückgegeben wird.
    let title_seed_pending = control.title_seed_pending();
    let outcome: CoreResult<TurnOutcome> = async {
        match resolution {
            ApprovalResolution::Reject { reason } => {
                let denied_result = ToolCallResult::error(format!("denied by user: {reason}"));
                notify_tool_outcome(
                    session,
                    pending.call.name.as_str(),
                    &pending.call.arguments,
                    &denied_result,
                );
                notify_tool_progress(session);
                session
                    .history_mut()
                    .push_tool_result(pending.call.id, denied_result, 0);
                persist_last(session, store).await?;
                drive_turn(session, model, store, approvals, &ctx, handle, control).await
            }
            ApprovalResolution::Approve => {
                // Zulassung beim `ToolCallMeter` (falls registriert): der nun
                // genehmigte Aufruf wird gebucht, bevor er ausgeführt oder
                // gespawnt wird — derselbe Haken wie im sequenziellen Pfad von
                // `drive_turn` (Schritt a2). Eine Abweisung beantwortet den
                // einen wartenden Call und beendet den Turn mit
                // `CancelReason::Budget`. Der Permit (Lease-Zaun, siehe
                // `crate::tool_call_meter`) lebt, bis das Ergebnis dieses
                // Aufrufs feststeht, und fällt auf jedem Weg VOR dem
                // Weiterlaufen des Turns (`drive_turn`): sonst hielte er die
                // Lease über weitere Modellrunden und Zulassungen hinweg.
                let permit = match admit_tool_call(&control, pending.call.name.as_str()).await {
                    Ok(permit) => permit,
                    Err(refused) => {
                        notify_tool_outcome(
                            session,
                            pending.call.name.as_str(),
                            &pending.call.arguments,
                            &refused,
                        );
                        notify_tool_progress(session);
                        session
                            .history_mut()
                            .push_tool_result(pending.call.id, refused, 0);
                        persist_last(session, store).await?;
                        return cancel_turn_with_pending_calls(
                            session,
                            store,
                            handle,
                            harw_types::TokenUsage::default(),
                            CancelReason::Budget,
                            Vec::new(),
                        )
                        .await;
                    }
                };
                // Wie im normalen Dispatch (`delegation_step`): der Katalog
                // wird lokal beantwortet, eine Delegation prüft auch nach der
                // Freigabe Sichtbarkeit, Plan-Modus und Background-only, bevor
                // ein Kind entsteht. Ein erzeugtes Delegationswerkzeug, das
                // während der Freigabe abgeschaltet wurde, bleibt
                // unerreichbar. Andere Operationen (auch `agent.message`)
                // bleiben auf dem reinen Ausführerpfad.
                let (handoff, local_result) =
                    if crate::child_approval::is_generated_delegation_tool(&pending.call.name) {
                        if find_executor(session, &pending.call.name).is_none()
                            && !generated_delegation_tool_available(session, &pending.call.name)
                        {
                            (
                                None,
                                Some(ToolCallResult::error(format!(
                                    "{}: Werkzeug ist für diese Sitzung nicht verfügbar",
                                    pending.call.name
                                ))),
                            )
                        } else {
                            delegation_step(session, &pending.call)
                        }
                    } else {
                        (None, None)
                    };
                if let Some(result) = local_result {
                    // Nichts gespawnt: das Ergebnis steht fest.
                    drop(permit);
                    notify_tool_outcome(
                        session,
                        pending.call.name.as_str(),
                        &pending.call.arguments,
                        &result,
                    );
                    notify_tool_progress(session);
                    session
                        .history_mut()
                        .push_tool_result(pending.call.id, result, 0);
                    persist_last(session, store).await?;
                    return drive_turn(session, model, store, approvals, &ctx, handle, control)
                        .await;
                }
                if let Some((role, handoff_arguments)) = handoff {
                    let spawner = session.registry().spawner().cloned().ok_or_else(|| {
                        CoreError::HandoffFailed {
                            role: role.clone(),
                            reason: "no AgentSpawner registered".to_owned(),
                        }
                    })?;
                    // Vor dem Verschieben der Argumente in den SpawnInput lesen.
                    let question = child_question(&handoff_arguments);
                    let input = SpawnInput {
                        parent_session_id: session.id().clone(),
                        handoff_call_id: pending.call.id.clone(),
                        instructions: handoff_instructions(&handoff_arguments),
                        context: handoff_arguments,
                        // Hereditär, nie neu erfunden: dieser Handoff deklariert
                        // selbst keine eigene Kontextdecke (`call.arguments`
                        // trägt keine), also reicht dieses Feld exakt die bereits
                        // geschnittene Decke des laufenden Turns durch — dieselbe
                        // Regel, mit der `ManagedAgentSpawner::admit`
                        // (`child_controller.rs`) Sandbox und Trace im selben
                        // Schritt vererbt. `ContextCeiling::default()` stünde
                        // hier für eine erfundene Grenze, die nichts mit der
                        // tatsächlichen Autorität dieser Sitzung zu tun hätte.
                        ceiling: session
                            .spawn_context()
                            .and_then(|spawn_context| spawn_context.ceiling.clone()),
                    };
                    let context = match governed_spawn_context(session) {
                        Ok(context) => context,
                        Err(error) => {
                            drop(permit);
                            let result = missing_tool_execution_context_result(error)?;
                            session
                                .history_mut()
                                .push_tool_result(pending.call.id, result, 0);
                            persist_last(session, store).await?;
                            return drive_turn(session, model, store, approvals, &ctx, handle, control)
                                .await;
                        }
                    };
                    let child = match spawner
                        .spawn_child(&role, input, context.sandbox, context.suggestions)
                        .await
                    {
                        Ok(child) => child,
                        // Runde 5, Teile J/K: eine Orchestrierungsgrenze oder
                        // abgelehnte Fortsetzung ist eine Antwort an das Modell,
                        // kein Turn-Abbruch — ebenso jede andere abgelehnte
                        // Admission (HARW-ORCH-001), damit ein Orchestrator
                        // nachsteuern kann statt seinen ganzen Turn zu verlieren.
                        Err(error) => {
                            drop(permit);
                            session.history_mut().push_tool_result(
                                pending.call.id,
                                ToolCallResult::error(spawn_rejection_text(&role, &error)),
                                0,
                            );
                            persist_last(session, store).await?;
                            return drive_turn(session, model, store, approvals, &ctx, handle, control)
                                .await;
                        }
                    };
                    // Ein genehmigungspflichtiger Handoff wird ausschließlich hier
                    // gespawnt (`drive_turn` kehrt vorher mit `AwaitingApproval`
                    // zurück). Ohne dieses Event bliebe ein solches Kind für jeden
                    // Beobachter unsichtbar; doppelt gemeldet wird es nicht.
                    emit(
                        session,
                        TurnEvent::ChildSpawned {
                            turn_id: ctx.turn_id.clone(),
                            child: child.clone(),
                            role: role.clone(),
                            question: question.clone(),
                        },
                    );

                    let Some(result) =
                        detach_admitted_child(session, &spawner, &child, &role, question.as_deref())
                            .await
                    else {
                        session.begin_handoff(child.clone(), pending.call.id.clone(), role.clone())?;
                        return Ok(TurnOutcome::AwaitingChild {
                            child,
                            call_id: pending.call.id,
                            role,
                        });
                    };
                    // Seiteneffekt-Grenze: Kind gespawnt und übergeben.
                    drop(permit);
                    notify_tool_outcome(
                        session,
                        pending.call.name.as_str(),
                        &pending.call.arguments,
                        &result,
                    );
                    notify_tool_progress(session);
                    session
                        .history_mut()
                        .push_tool_result(pending.call.id.clone(), result.clone(), 0);
                    persist_last(session, store).await?;
                    emit(
                        session,
                        TurnEvent::ToolCallCompleted {
                            turn_id: ctx.turn_id.clone(),
                            call_id: pending.call.id,
                            result,
                            duration_ms: 0,
                            placement: None,
                        },
                    );
                    drive_turn(session, model, store, approvals, &ctx, handle, control).await
                } else {
                    let tool_name = pending.call.name.to_string();
                    let tool_span = tracing::info_span!("tool.call", tool_name = %tool_name);
                    emit(
                        session,
                        TurnEvent::ToolCallRequested {
                            turn_id: ctx.turn_id.clone(),
                            call_id: pending.call.id.clone(),
                            tool_name: tool_name.clone(),
                            arguments: pending.call.arguments.clone(),
                        },
                    );
                    // Fix B (Moduldoku „Siebter Nachtrag"): dieser Rückfrage-
                    // Fortsetzungspfad hatte bis hierher keine `was_cancelled`-
                    // Weiche — ein hier hängender Ausführer war gegen Cancel
                    // vollständig blind. Struktur jetzt identisch zum
                    // sequenziellen Pfad oben: der Aufruf selbst racet gegen
                    // `control`s `CancelToken`, und ein Treffer geht über
                    // denselben `cancel_turn_with_pending_calls`-Weg wie ein
                    // Prüfpunkt-Treffer.
                    // R18 D-D: das dritte Tupelfeld ist die Platzierung aus den
                    // Ausführer-Metadaten (`ToolExecutor::execute_placed`).
                    let (result, was_cancelled): (PlacedCallResult, bool) = async {
                        match find_executor(session, &pending.call.name) {
                            Some(executor) => {
                                let (result, duration_ms, cancelled, placement) = match tool_execution_context(
                                    session,
                                    &ctx,
                                    control.cancel_token(),
                                ) {
                                    Ok(execution_context) => {
                                        let execution_context = execution_context.with_lifetime_guard(permit.lifetime_guard());
                                        let started = std::time::Instant::now();
                                        let (outcome, placement) = tokio::select! {
                                            biased;
                                            () = control.cancel_token().cancelled() => (Err(ToolsError::Cancelled), None),
                                            placed = executor.traced_execute_placed(&execution_context, &pending.call) => {
                                                (placed.output, wire_placement(placed.placement))
                                            }
                                        };
                                        let duration_ms = started.elapsed().as_millis() as u64;
                                        let cancelled = matches!(outcome, Err(ToolsError::Cancelled));
                                        let result = match outcome {
                                            Ok(output) => output_to_result(output),
                                            Err(e) => ToolCallResult::error(e.to_string()),
                                        };
                                        (result, duration_ms, cancelled, placement)
                                    }
                                    Err(error) => {
                                        (missing_tool_execution_context_result(error)?, 0, false, None)
                                    }
                                };
                                tracing::info!(
                                    duration_ms = duration_ms,
                                    status = if result.is_success() { "ok" } else { "err" },
                                    "tool.execute",
                                );
                                Ok::<_, CoreError>(((result, duration_ms, placement), cancelled))
                            }
                            None => {
                                tracing::info!(duration_ms = 0u64, status = "err", "tool.execute",);
                                Ok::<_, CoreError>((
                                    (
                                        ToolCallResult::error(format!(
                                            "no executor for tool '{}'",
                                            pending.call.name
                                        )),
                                        0u64,
                                        None,
                                    ),
                                    false,
                                ))
                            }
                        }
                    }
                    .instrument(tool_span)
                    .await?;
                    // Seiteneffekt-Grenze: das Werkzeug ist gelaufen (oder
                    // abgebrochen), sein Ergebnis steht fest.
                    drop(permit);
                    if was_cancelled {
                        // Derselbe Weg wie im sequenziellen Pfad oben: der
                        // laufende Call bekommt sein eigenes (bereits als
                        // Cancel-Fehler geformtes) Ergebnis, dann endet der Turn
                        // über `cancel_turn_with_pending_calls`. Ein
                        // Rückfrage-Resume setzt genau einen Call fort — es gibt
                        // keine Geschwister-Calls derselben Modellantwort, die
                        // hier noch anstünden (die pausierende `AskUser`-Antwort
                        // verließ die sequenzielle Schleife oben sofort mit
                        // `AwaitingApproval`, ohne die übrigen Calls der Antwort
                        // überhaupt in den Verlauf zu schreiben — siehe
                        // `ApprovalDecision::AskUser` im sequenziellen Pfad).
                        // `remaining: Vec::new()` ist deshalb korrekt, nicht nur
                        // bequem.
                        let (result_value, result_duration_ms, result_placement) = result;
                        emit(
                            session,
                            TurnEvent::ToolCallCompleted {
                                turn_id: ctx.turn_id.clone(),
                                call_id: pending.call.id.clone(),
                                result: result_value.clone(),
                                duration_ms: result_duration_ms,
                                placement: result_placement,
                            },
                        );
                        notify_tool_outcome(
                            session,
                            &tool_name,
                            &pending.call.arguments,
                            &result_value,
                        );
                        notify_tool_progress(session);
                        session
                            .history_mut()
                            .push_tool_result(pending.call.id, result_value, result_duration_ms);
                        persist_last(session, store).await?;
                        return cancel_turn_with_pending_calls(
                            session,
                            store,
                            handle,
                            harw_types::TokenUsage::default(),
                            control.cancel_token().reason().unwrap_or(CancelReason::User),
                            Vec::new(),
                        )
                        .await;
                    }
                    let (result_value, result_duration_ms, result_placement) = result;
                    emit(
                        session,
                        TurnEvent::ToolCallCompleted {
                            turn_id: ctx.turn_id.clone(),
                            call_id: pending.call.id.clone(),
                            result: result_value.clone(),
                            duration_ms: result_duration_ms,
                            placement: result_placement,
                        },
                    );
                    notify_tool_outcome(session, &tool_name, &pending.call.arguments, &result_value);
                    notify_tool_progress(session);
                    session
                        .history_mut()
                        .push_tool_result(pending.call.id, result_value, result_duration_ms);
                    persist_last(session, store).await?;
                    drive_turn(session, model, store, approvals, &ctx, handle, control).await
                }
            }
        }
    }
    .await;

    if title_seed_pending { settle_title_seed_failure(store, &ctx, &outcome).await; }
    if let Err(error) = &outcome {
        transition_after_turn_failure(session, &ctx, error);
    }
    outcome
}

/// Der Model-/Tool-Zyklus. Setzt `Running`-State voraus.
///
/// Emits the `model.request`, `model.response`, `tool.call`, `tool.execute`,
/// and `approval.wait` tracing events and spans described in the module doc.
/// Must be called while an `agent.turn` span is already entered by the caller.
async fn drive_turn(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    ctx: &TurnInputContext,
    handle: TurnHandle,
    control: TurnControl,
) -> CoreResult<TurnOutcome> {
    // Ein Provider kann trotz expliziter Fortsetzungsanweisung erneut am
    // Ausgabelimit enden. Die feste Obergrenze verhindert einen stillen,
    // kostenpflichtigen Endlos-Loop; danach erhält der Aufrufer weiterhin den
    // ehrlichen `Truncated`-Ausgang mitsamt bereits persistiertem Teiltext.
    const MAX_AUTOMATIC_CONTINUATIONS: u32 = 3;
    const CONTINUATION_INSTRUCTION: &str = "Die unmittelbar vorherige Assistant-Antwort wurde wegen eines Ausgabelimits abgeschnitten. Setze exakt an ihrer letzten Stelle fort, ohne Text zu wiederholen oder neu anzufangen. Schließe den ursprünglichen Auftrag eigenständig ab.";
    const TOOL_CALL_SPLIT_INSTRUCTION: &str = "Deine unmittelbar vorherigen Werkzeugaufrufe wurden wegen des Ausgabelimits abgeschnitten und NICHT ausgeführt. Wiederhole sie kleiner: weniger Aufrufe pro Antwort, weniger Ziele pro delegate_wave, kürzere Aufträge und Argumente.";

    let mut total_usage = harw_types::TokenUsage::default();
    // Zählt *aufeinanderfolgende* Abschneidungen: jede angenommene Runde, die
    // nicht mit `MaxTokens` endet, setzt Zähler und Anweisung zurück. Früher
    // galt die Grenze pro Turn und die Fortsetzungsanweisung hing ab der
    // ersten Abschneidung an *jeder* weiteren Runde — lange
    // Orchestrator-Turns endeten so grundlos als `Truncated`, und das Modell
    // las in normalen Tool-Runden „setze exakt an der letzten Stelle fort".
    let mut automatic_continuations = 0u32;
    let mut continuation_instruction: Option<&'static str> = None;
    // Fortlaufende Modell-Runden-Nummer dieses `drive_turn`-Aufrufs, für
    // `UsageRound::round` — bei jedem Modellaufruf inkrementiert, bevor die
    // Runde persistiert wird.
    let mut round: u32 = 0;
    // Nutzung der zuletzt abgeschlossenen Modell-Runde — für `maybe_compact`
    // an beiden Call-Sites (innerhalb der Schleife und nach Turn-Ende, wo
    // `response` bereits außer Scope ist). Kein Default-Vorbelegungswert:
    // jeder Lesezugriff (Zeile ~2536, nach der Schleife) liegt hinter der
    // ersten Zuweisung unten (nach dem ersten Modellaufruf dieser Runde);
    // ein Default hier wäre vor jedem Lesezugriff unbedingt überschrieben
    // und damit ein toter Store.
    let mut last_round_usage: harw_types::TokenUsage;
    // Welle 3 — Kontextfenster-Wächter:
    // - `context_retry_used`: die einmalige Notfall-Verdichtung + Wiederholung
    //   nach `ModelError::ContextLength` / `StopReason::ContextWindowExceeded`
    //   ist für den aktuellen Request verbraucht.
    // - `max_tokens_retry_used` / `max_output_override`: die einmalige
    //   Wiederholung mit verdoppeltem Ausgabelimit nach `MaxTokens` mitten in
    //   Tool-Calls.
    // - `last_compaction_tokens_after`: Ergebnis der letzten Verdichtung
    //   dieses Aufrufs, für die Hysterese in `maybe_compact`.
    let mut context_retry_used = false;
    let mut max_tokens_retry_used = false;
    let mut max_output_override: Option<u64> = None;
    let mut last_compaction_tokens_after: Option<u64> = None;
    // Verlaufslänge direkt nach der zuletzt angenommenen Modellantwort — was
    // danach angehängt wird (Tool-Ergebnisse), deckt `last_round_usage` noch
    // nicht ab. Wie `last_round_usage` erst nach der ersten Antwort belegt.
    let mut history_mark: usize;
    // Wanduhr-Nullpunkt dieses Aufrufs (siehe Moduldoku „Fünfter Nachtrag").
    // Wiederholte Aufrufe (weiterer Schleifendurchlauf) sind ein No-op — nur
    // der erste zählt.
    control.start();

    // Turn-Wächter (Addendum F+G): nur erzeugt, wenn die Session-Policy sie
    // einschaltet — `TurnGuard` lebt ausschließlich für die Dauer dieses
    // `drive_turn`-Aufrufs (siehe Moduldoku „Fünfter Nachtrag" zur analogen
    // Lücke bei `TurnControl`: ein pausierter und über `resume_*`
    // fortgesetzter Turn bekommt hier ebenfalls einen frischen Wächter ohne
    // Turn-übergreifenden Zustand).
    let guard_policy = session.guard_policy();
    let mut guard: Option<TurnGuard> = guard_policy
        .enabled
        .then(|| TurnGuard::new(guard_policy, session.id()));
    // Signaturen (Tool-Name + kanonische Argumente) erfolgreicher Aufrufe,
    // bereits in einer früheren Runde dieses Turns gesehen — für die
    // Fortschritts-Erkennung „mindestens ein erfolgreicher Tool-Aufruf mit in
    // diesem Turn neuer Signatur" (Vertrag `TurnGuard::observe_round_end`).
    let mut turn_seen_success_signatures: HashSet<String> = HashSet::new();
    // Hinweistext eines `Warn`-Befunds, der keinem eigenen Tool-Ergebnis
    // dieser Runde mehr zugeordnet werden konnte (z. B. ein
    // Runden-Ende-Befund nach der letzten Werkzeugausführung der Runde) —
    // wird dem nächsten Tool-Ergebnis des Turns vorangestellt, sobald eines
    // entsteht.
    let mut pending_guard_hint: Option<String> = None;

    // Einmal je Turn, vor dem ersten Model-Aufruf: das Kassenbuch des
    // `context.load`-Ausführers vorbelegen, sofern die Sitzung ein
    // `ContextProgram` deklariert (siehe `seed_context_load_ledger`-Doku).
    // Bewusst außerhalb der Schleife unten — `ContextLoadExecutor::seed_turn`
    // überschreibt das Kassenbuch eines Turns vollständig (auch
    // `loads_used`), ein Aufruf je Model-Runde würde also innerhalb desselben
    // Turns bereits verbuchte `context.load`-Aufrufe verwerfen.
    seed_context_load_ledger(session, &ctx.turn_id);

    // Werkzeug-Budget (Kind): Aufrufe einer Antwort über dem Rest werden
    // nicht ausgeführt; nach der Runde mit den erlaubten endet der Turn.
    let mut over_tool_budget: Option<(Vec<ToolCall>, String)> = None;
    let final_title_location;

    loop {
        if let Some((calls, message)) = over_tool_budget.take() {
            return skip_calls_over_tool_budget(
                session,
                store,
                handle,
                total_usage,
                calls,
                &message,
            )
            .await;
        }
        // Prüfpunkt vor jedem Modellaufruf (Moduldoku „W4a A-LOOP"): Abbruch,
        // Modellrunden, Ausgabe-Tokens, Wanduhr. Vor der Context-/Instructions-
        // Montage, damit ein bereits erschöpftes Budget diese Arbeit nicht
        // mehr verursacht.
        if let Some(reason) = control.model_checkpoint() {
            return cancel_turn(session, handle, total_usage, reason).await;
        }
        // Runde 9, E6: ein live gewechselter Modus des Baums gilt ab dieser
        // Runde (Werkzeugfläche neu geschnitten, Notiz im Verlauf).
        let _ = session.sync_live_mode();

        // 1./2./4a. Context + Instructions + Request-Montage (siehe
        // `assemble_round_request`). Eine Wiederholung nach Notfall-
        // Kompaktierung baut den Request über denselben Weg neu.
        // Abschlussmeldungen von Hintergrund-Kindern eines Orchestrator-Kindes
        // gehen in diese Runde (die UIA-Wurzel bekommt ihre über die TUI).
        inject_background_notices(session, store).await?;
        let continuation = continuation_instruction;
        let mut request = assemble_round_request(
            session,
            ctx,
            &control,
            &handle.turn_id,
            &total_usage,
            continuation,
            max_output_override,
        )
        .await?;

        // 4b. Pre-flight-Wächter (Welle 3): geschätzte Request-Größe gegen
        // das Fenster des aktiven Modells. Droht der Request das Fenster zu
        // sprengen (oder hat ein Modellwechsel eine Verdichtung vorgemerkt),
        // wird VOR dem Senden notfallverdichtet und der Request neu gebaut.
        // Passt er danach immer noch nicht, endet der Turn mit einem klaren
        // „Kontext erschöpft"-Fehler statt einem sicheren Provider-Fehler.
        let budget = RoundBudget::of(session);
        let mut estimated_tokens =
            crate::context_budget::estimate_request_tokens(&request, session.token_calibration());
        let needs_emergency = budget.window_tokens > 0
            && (session.pending_compaction()
                || budget.overflows(estimated_tokens)
                || session
                    .auto_compact()
                    .is_some_and(|policy| policy.must_compact_before_send(estimated_tokens)));
        if needs_emergency {
            tracing::warn!(
                estimated_tokens,
                window_tokens = budget.window_tokens,
                reserve_tokens = budget.reserve_tokens,
                pending = session.pending_compaction(),
                "turn_loop.preflight_emergency_compaction",
            );
            let target = budget.emergency_target(EMERGENCY_PREFLIGHT_TARGET_PERCENT);
            let overhead = request_overhead_tokens(&request, session.token_calibration());
            if let Some(outcome) = run_budget_compaction(
                session,
                model,
                store,
                target,
                overhead,
                crate::auto_compact::CompactDecision::Emergency,
            )
            .await
            {
                last_compaction_tokens_after = Some(outcome.tokens_after);
            }
            request = assemble_round_request(
                session,
                ctx,
                &control,
                &handle.turn_id,
                &total_usage,
                continuation,
                max_output_override,
            )
            .await?;
            estimated_tokens = crate::context_budget::estimate_request_tokens(
                &request,
                session.token_calibration(),
            );
        }
        // Nicht-Historien-Anteil dieses Requests (System, Fragmente,
        // Werkzeuge) — für jede weitere Notfall-Verdichtung dieser Runde.
        let request_overhead = request_overhead_tokens(&request, session.token_calibration());
        if budget.overflows(estimated_tokens) {
            return Err(context_exhausted_error(
                estimated_tokens,
                budget.reserve_tokens,
                budget.window_tokens,
            ));
        }
        let history_items_dropped = request.context_assembly.history_items_dropped;

        // Emit model.request event: geschätzte Request-Größe (System-Prompt,
        // Instruktions-Fragmente, Datenblock, Verlauf, Tool-Schemas) — dieselbe
        // Byte-Zahl, mit der die Kalibrierung nach der Antwort nachgeführt wird.
        let estimated_bytes = crate::context_budget::estimate_request_bytes(&request);
        tracing::info!(
            size_bytes = estimated_bytes,
            estimated_tokens,
            "model.request"
        );

        // Races the model call itself against `control`'s `CancelToken`
        // (W4a A-LOOP Moduldoku: "Der Modellaufruf selbst läuft gegen
        // `CancelToken::cancelled`"). `biased` ensures an already-cancelled
        // token wins even if the model future also happens to be ready —
        // matching `model_checkpoint()`'s own cancel-first precedence.
        // `request.cancel` (`with_cancel_token` above) lets a
        // cancel-aware provider (e.g. `RetryingProvider::race_respond`)
        // abort its own retry loop early; this `select!` is the backstop
        // for every provider, cancel-aware or not.
        let respond_session = session.id().clone();
        let response = tokio::select! {
            biased;
            _ = control.cancel_token().cancelled() => {
                return cancel_turn(
                    session,
                    handle,
                    total_usage,
                    control.cancel_token().reason().unwrap_or(CancelReason::User),
                )
                .await;
            }
            result = crate::model::with_current_session(respond_session, model.respond(request)) => result,
        };
        // `ModelError::Cancelled` can also surface from inside `respond()`
        // itself (a cancel-aware provider observed the token before this
        // `select!` did) — routed through the identical `cancel_turn` path
        // rather than the ordinary `?`-propagated `CoreError::Model(...)`.
        let mut response = match response {
            Ok(response) => response,
            Err(crate::model::ModelError::Cancelled) => {
                return cancel_turn(
                    session,
                    handle,
                    total_usage,
                    control
                        .cancel_token()
                        .reason()
                        .unwrap_or(CancelReason::User),
                )
                .await;
            }
            // Welle 3: der Provider meldet eine Kontextüberschreitung — genau
            // eine Notfall-Verdichtung (Ziel 50 %) plus ein erneuter Versuch.
            // Scheitert auch der, oder bewirkt die Verdichtung nichts, geht
            // der Fehler regulär an den Aufrufer.
            Err(error @ crate::model::ModelError::ContextLength { .. }) => {
                if context_retry_used {
                    return Err(error.into());
                }
                context_retry_used = true;
                tracing::warn!(
                    %error,
                    estimated_tokens,
                    "turn_loop.context_length_recovery"
                );
                let target = budget.recovery_target(estimated_tokens);
                match run_budget_compaction(
                    session,
                    model,
                    store,
                    target,
                    request_overhead,
                    crate::auto_compact::CompactDecision::Emergency,
                )
                .await
                {
                    Some(outcome) if !outcome.no_op => {
                        last_compaction_tokens_after = Some(outcome.tokens_after);
                        continue;
                    }
                    _ => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        };
        control.record_model_round();
        control.record_usage(&response.usage);
        notify_round_progress(session, &response.usage);
        total_usage.add(&response.usage);
        round += 1;
        last_round_usage = response.usage.clone();
        // Kalibrierung Bytes→Tokens mit der tatsächlichen Prompt-Belegung
        // nachführen (EMA, siehe `TokenCalibration::observe`).
        session
            .token_calibration_mut()
            .observe(estimated_bytes, response.usage.prompt_tokens());
        // Live-Stand nach abgeschlossener Runde (auch der Pro-Runde-Fallback
        // nicht streamender Provider landet hier).
        emit(
            session,
            TurnEvent::UsageUpdated {
                turn_id: handle.turn_id.clone(),
                round: response.usage.clone(),
                turn_total: total_usage.clone(),
                final_round: true,
            },
        );
        emit(
            session,
            TurnEvent::ContextUpdated {
                turn_id: handle.turn_id.clone(),
                used_tokens: response
                    .usage
                    .prompt_tokens()
                    .saturating_add(response.usage.output_tokens),
                window_tokens: budget.window_tokens,
                history_items_dropped: u32::try_from(history_items_dropped).unwrap_or(u32::MAX),
                estimated_next_tokens: (budget.window_tokens > 0).then_some(estimated_tokens),
                threshold_tokens: budget.threshold_tokens,
                reserve_tokens: (budget.window_tokens > 0).then_some(budget.reserve_tokens),
            },
        );

        // Nutzung dieser Runde persistieren (best effort — ein Store-Fehler
        // darf den Turn niemals scheitern lassen, siehe Vertrag
        // `StateStore::record_usage`).
        let usage_round = UsageRound {
            round,
            provider_id: session.active_provider().map(|p| p.as_str().to_owned()),
            model_id: session.active_model().map(|m| m.as_str().to_owned()),
            usage: response.usage.clone(),
            cache_strategy: None,
        };
        if let Err(error) = store.record_usage(session.id(), &usage_round).await {
            tracing::warn!(%error, "turn_loop.record_usage_failed");
        }

        // Emit model.response event: size_bytes from tool_calls JSON +
        // optional message length; tool_call_count for scheduling insight.
        let response_size_bytes: usize = response.message.as_deref().map_or(0, |m| m.len())
            + serde_json::to_vec(&response.tool_calls)
                .map(|v| v.len())
                .unwrap_or(0);
        tracing::info!(
            size_bytes = response_size_bytes,
            tool_call_count = response.tool_calls.len(),
            "model.response",
        );

        // Welle 3: Wiederholungen VOR jeder Übernahme der Antwort in den
        // Verlauf — eine verworfene Antwort darf weder Text noch Tool-Calls
        // hinterlassen (sonst stünde der Teiltext nach der Wiederholung
        // doppelt im Verlauf). Nutzung/Kalibrierung sind oben bereits
        // verbucht: die Tokens wurden tatsächlich verbraucht.
        //
        // (a) `ContextWindowExceeded` als Stop-Grund: dieselbe einmalige
        //     Notfall-Verdichtung (Ziel 50 %) + Wiederholung wie beim
        //     `ModelError::ContextLength` oben. Bleibt die Verdichtung
        //     wirkungslos, gilt der bisherige `Truncated`-Pfad unten.
        if matches!(
            response.stop,
            crate::model::StopReason::ContextWindowExceeded
        ) && !context_retry_used
        {
            context_retry_used = true;
            tracing::warn!(
                estimated_tokens,
                "turn_loop.context_window_exceeded_recovery"
            );
            let target = budget.recovery_target(estimated_tokens);
            if let Some(outcome) = run_budget_compaction(
                session,
                model,
                store,
                target,
                request_overhead,
                crate::auto_compact::CompactDecision::Emergency,
            )
            .await
                && !outcome.no_op
            {
                last_compaction_tokens_after = Some(outcome.tokens_after);
                continue;
            }
        }
        // (b) `MaxTokens` mitten in Tool-Calls: die Argumente sind
        //     abgeschnitten und dürfen nie ausgeführt werden. Statt sofort
        //     `Truncated` zu melden, wird die Runde einmal mit verdoppeltem
        //     Ausgabelimit wiederholt (gedeckelt auf 2 × Reserve und die
        //     Hälfte des Fensters).
        if matches!(response.stop, crate::model::StopReason::MaxTokens)
            && !response.tool_calls.is_empty()
            && !max_tokens_retry_used
        {
            let current = max_output_override
                .or(session.max_output_tokens())
                .unwrap_or(budget.reserve_tokens);
            let raised = budget.raised_output_limit(current);
            if raised > current {
                max_tokens_retry_used = true;
                max_output_override = Some(raised);
                tracing::info!(
                    previous_max_output_tokens = current,
                    max_output_tokens = raised,
                    "turn_loop.retry_after_max_tokens_with_tool_calls",
                );
                continue;
            }
        }
        if !matches!(response.stop, crate::model::StopReason::MaxTokens) {
            automatic_continuations = 0;
            continuation_instruction = None;
        }
        // Antwort angenommen: die Einmal-Wiederholungen gelten wieder für
        // den nächsten Request, das angehobene Ausgabelimit nur für die
        // eine wiederholte Runde.
        context_retry_used = false;
        max_tokens_retry_used = false;
        max_output_override = None;
        // PL-95 S3: die Skill-Auszüge dieses Requests haben das Modell
        // erreicht — erst jetzt gilt eine Auswahl als Lieferung. Ein vorher
        // verworfener Request (Neubau nach Notfall-Verdichtung, Wiederholung
        // nach Kontextüberschreitung) hinterlässt so kein Ereignis, und jede
        // Lieferung wird genau einmal festgehalten.
        record_skill_injections(session, store, &handle.turn_id).await;
        // Ab hier angehängte Tool-Ergebnisse sind in `last_round_usage`
        // noch nicht enthalten — `maybe_compact` schätzt sie ab dieser Marke.
        history_mark = session.history().len();

        // Defense in depth: provider adapters should already remove partial
        // tools and reasoning on an interrupted stream. Never persist or
        // execute either even if an alternative adapter returned them.
        if matches!(
            response.stop,
            crate::model::StopReason::StreamInterrupted
                | crate::model::StopReason::StreamTransportInterrupted
                | crate::model::StopReason::IncompleteUnknown
        ) {
            response.tool_calls.clear();
            response.reasoning = None;
        }

        // Für die Fortschritts-Erkennung des reinen Text-Zweigs unten
        // („Assistant-Text ohne Tool-Aufrufe") vor dem Move gesichert.
        let response_had_text = response.message.is_some();
        let response_had_no_tool_calls = response.tool_calls.is_empty();
        let response_stop = response.stop.clone();
        // Fortschritt durch erfolgreiche Tool-Aufrufe mit neuer Signatur wird
        // unten im Tool-Call-Loop gesetzt.
        let mut round_progressed_by_tools = false;

        // Reasoning persistieren (Runde 2): für JEDE Session, sofern sich
        // lesbarer Denktext extrahieren ließ (redacted/verschlüsseltes
        // Reasoning bleibt unsichtbar). VOR der AssistantMessage eingefügt —
        // Denken kommt vor der Antwort. Vom Modell-Input bleibt es
        // unabhängig von der Session-Art ausgeschlossen:
        // `ConversationHistory::to_model_messages` (history.rs) überspringt
        // `TurnItem::Reasoning` bedingungslos, und alle Provider bauen ihre
        // Nachrichten ausschließlich darüber.
        if let Some(text) = response.reasoning.as_ref().and_then(extract_thinking_text) {
            let reasoning_id = harw_types::ItemId::new();
            session
                .history_mut()
                .push(TurnItem::Reasoning(ReasoningItem {
                    id: reasoning_id.clone(),
                    summary_text: vec![text.clone()],
                    raw_content: Vec::new(),
                }));
            persist_last(session, store).await?;
            emit(
                session,
                TurnEvent::ItemAdded {
                    turn_id: handle.turn_id.clone(),
                    item: TurnItem::Reasoning(ReasoningItem {
                        id: reasoning_id,
                        summary_text: vec![text],
                        raw_content: Vec::new(),
                    }),
                },
            );
        }

        let mut title_location = None;
        if let Some(text) = response.message {
            let phase = if response.tool_calls.is_empty() {
                Some(harw_types::MessagePhase::FinalAnswer)
            } else {
                Some(harw_types::MessagePhase::Commentary)
            };
            let text_for_event = text.clone();
            session.history_mut().push_assistant_text(text, phase);
            if control.title_seed_pending() && response_had_no_tool_calls &&
                matches!(&response_stop, crate::model::StopReason::EndTurn | crate::model::StopReason::StopSequence)
            {
                title_location = persist_title_seed_last(session, store).await?;
            } else {
                persist_last(session, store).await?;
            }
            emit(
                session,
                TurnEvent::ItemAdded {
                    turn_id: handle.turn_id.clone(),
                    item: TurnItem::AssistantMessage(AssistantMessageItem {
                        id: harw_types::ItemId::new(),
                        content: vec![ContentPart::Text {
                            text: text_for_event,
                        }],
                        phase,
                    }),
                },
            );
        }

        let unreliable_end = match &response_stop {
            crate::model::StopReason::StreamInterrupted => Some(ModelStopOutcome::StreamInterrupted),
            crate::model::StopReason::StreamTransportInterrupted => Some(ModelStopOutcome::StreamTransportInterrupted),
            crate::model::StopReason::IncompleteUnknown => Some(ModelStopOutcome::IncompleteUnknown),
            crate::model::StopReason::Other(code) => Some(ModelStopOutcome::ProviderStopCode(code.clone())),
            _ => None,
        };
        if let Some(stop) = unreliable_end {
            return finish_model_stop(
                session,
                store,
                handle,
                total_usage,
                pending_guard_hint.clone(),
                stop,
            )
            .await;
        }

        // Ein Provider darf nie dazu verleitet werden, abgeschnittene oder
        // gefilterte Tool-Argumente auszuführen. Die Antwort ist trotzdem
        // bereits als Text erhalten, damit ein Nutzer den sichtbaren Teil
        // nicht verliert.
        if !response_had_no_tool_calls {
            match response_stop {
                // Auch nach der Wiederholung mit verdoppeltem Limit zu lang:
                // die Calls werden verworfen (nie ausgeführt) und das Modell
                // aufgefordert, sie kleiner zu schneiden — statt den ganzen
                // (Orchestrator-)Turn als `Truncated` zu beenden.
                crate::model::StopReason::MaxTokens
                    if automatic_continuations < MAX_AUTOMATIC_CONTINUATIONS =>
                {
                    automatic_continuations += 1;
                    continuation_instruction = Some(TOOL_CALL_SPLIT_INSTRUCTION);
                    tracing::info!(
                        continuation = automatic_continuations,
                        dropped_tool_calls = response.tool_calls.len(),
                        "turn_loop.auto_continue_after_truncated_tool_calls"
                    );
                    let appended_tokens = appended_tokens_since(session, history_mark);
                    maybe_compact(
                        session,
                        model,
                        store,
                        &last_round_usage,
                        appended_tokens,
                        false,
                        &mut last_compaction_tokens_after,
                    )
                    .await;
                    continue;
                }
                crate::model::StopReason::MaxTokens
                | crate::model::StopReason::ContextWindowExceeded => {
                    return finish_model_stop(
                        session,
                        store,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        if response_stop == crate::model::StopReason::ContextWindowExceeded {
                            ModelStopOutcome::ContextLimit
                        } else {
                            ModelStopOutcome::OutputLimit
                        },
                    )
                    .await;
                }
                crate::model::StopReason::Refusal { detail } => {
                    return finish_model_stop(
                        session,
                        store,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        ModelStopOutcome::Refused { detail },
                    )
                    .await;
                }
                crate::model::StopReason::ContentFilter => {
                    return finish_model_stop(
                        session,
                        store,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        ModelStopOutcome::ContentFiltered,
                    )
                    .await;
                }
                _ => {}
            }
        }

        // 6. Keine Tool-Calls mehr ⇒ Turn fertig.
        if response_had_no_tool_calls {
            // Runden-Ende-Beobachtung (Addendum F+G): diese Runde bestand nur
            // aus Assistant-Text — Fortschritt genau dann, wenn tatsächlich
            // Text kam (Vertrag: „Assistant-Text ohne Tool-Aufrufe").
            if let Some(g) = guard.as_mut() {
                match g.observe_round_end(response_had_text) {
                    GuardVerdict::Continue => {}
                    GuardVerdict::Warn { event, hint } => {
                        report_drift(session, store, &event).await;
                        // Keine Werkzeugausführung mehr in dieser Runde, an
                        // die der Hinweis sofort angehängt werden könnte —
                        // er wird dem nächsten Tool-Ergebnis des Turns
                        // vorangestellt, falls noch eines entsteht.
                        pending_guard_hint = Some(match pending_guard_hint.take() {
                            Some(existing) => format!("{existing}\n\n{hint}"),
                            None => hint,
                        });
                    }
                    GuardVerdict::Abort { event, hint } => {
                        report_drift(session, store, &event).await;
                        tracing::warn!(hint = %hint, "turn_loop.guard_abort");
                        note_guard_stop(session, &event);
                        return cancel_turn(session, handle, total_usage, CancelReason::Budget)
                            .await;
                    }
                }
            }
            match response_stop {
                crate::model::StopReason::MaxTokens
                    if automatic_continuations < MAX_AUTOMATIC_CONTINUATIONS =>
                {
                    automatic_continuations += 1;
                    continuation_instruction = Some(CONTINUATION_INSTRUCTION);
                    tracing::info!(
                        continuation = automatic_continuations,
                        "turn_loop.auto_continue_after_max_tokens"
                    );
                    let appended_tokens = appended_tokens_since(session, history_mark);
                    maybe_compact(
                        session,
                        model,
                        store,
                        &last_round_usage,
                        appended_tokens,
                        false,
                        &mut last_compaction_tokens_after,
                    )
                    .await;
                    continue;
                }
                crate::model::StopReason::MaxTokens
                | crate::model::StopReason::ContextWindowExceeded => {
                    return finish_model_stop(
                        session,
                        store,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        if response_stop == crate::model::StopReason::ContextWindowExceeded {
                            ModelStopOutcome::ContextLimit
                        } else {
                            ModelStopOutcome::OutputLimit
                        },
                    )
                    .await;
                }
                crate::model::StopReason::Refusal { detail } => {
                    return finish_model_stop(
                        session,
                        store,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        ModelStopOutcome::Refused { detail },
                    )
                    .await;
                }
                crate::model::StopReason::ContentFilter => {
                    return finish_model_stop(
                        session,
                        store,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        ModelStopOutcome::ContentFiltered,
                    )
                    .await;
                }
                _ => {
                    // Ein Orchestrator-Kind endet nicht vor seinen eigenen
                    // Hintergrund-Kindern: deren Ergebnisse kämen sonst nie an
                    // (die TUI liefert sie nur der UIA-Wurzel).
                    match await_background_results(session, store, &control).await? {
                        BackgroundWait::Delivered => continue,
                        BackgroundWait::Idle => {
                            final_title_location = title_location;
                            break;
                        }
                        // Abbruch oder Budget-Ende während des Wartens ist
                        // kein Erfolg: ehrlich abbrechen (die Kinder wurden
                        // mit abgebrochen).
                        BackgroundWait::Stopped(reason) => {
                            return cancel_turn(session, handle, total_usage, reason).await;
                        }
                    }
                }
            }
        }

        // Runde 7, Teil B3: optionale Argumente, die das Modell als String
        // `"null"` schickt, werden vor jeder Ausführung (und vor dem Eintrag
        // in den Verlauf) entfernt — gilt für Wurzel- und Kind-Turns.
        normalize_null_string_arguments(session, &mut response.tool_calls);

        // Werkzeug-Budget vor dem Dispatch: höchstens den Rest ausführen.
        over_tool_budget = control.split_over_tool_budget(&mut response.tool_calls);
        if response.tool_calls.is_empty()
            && let Some((calls, message)) = over_tool_budget.take()
        {
            return skip_calls_over_tool_budget(
                session,
                store,
                handle,
                total_usage,
                calls,
                &message,
            )
            .await;
        }

        // Prüfpunkt vor jeder Werkzeugausführung: Abbruch, Aufrufzahl, Wanduhr.
        // Trifft er zu, bekommt jeder noch offene Tool-Call dieser Antwort ein
        // synthetisches Fehlerergebnis, damit der Verlauf provider-gültig
        // bleibt (jeder `tool_call` hat sein `tool_result`) — siehe Moduldoku
        // „W4a A-LOOP".
        if let Some(reason) = control.tool_checkpoint(response.tool_calls.len()) {
            return cancel_turn_with_pending_calls(
                session,
                store,
                handle,
                total_usage,
                reason,
                response.tool_calls,
            )
            .await;
        }
        control.record_tool_calls(response.tool_calls.len());

        // Die Vorprüfung des Parallel-Pfads kann Guardrail-Entscheidungen
        // bereits eingeholt haben. Sie leben genau eine Modellantwort lang und
        // werden unten verbraucht, damit kein Handler doppelt gefragt wird.
        let mut prepared = PreparedApprovals::default();
        match try_execute_parallel_calls(
            session,
            store,
            ctx,
            &response.tool_calls,
            &mut prepared,
            guard.as_mut(),
            &mut turn_seen_success_signatures,
            &mut pending_guard_hint,
            &mut round_progressed_by_tools,
            &control,
        )
        .await?
        {
            ParallelOutcome::NotApplicable => {}
            ParallelOutcome::Executed => continue,
            ParallelOutcome::Aborted(reason) => {
                return cancel_turn(session, handle, total_usage, reason).await;
            }
        }

        // 5. Tool-Call-Loop.
        // `while let` statt `for`: bei einem `TurnGuard`-`Abort` mitten in
        // dieser Runde (Addendum F+G) müssen die noch nicht ausgeführten
        // Calls — der restliche Iterator-Inhalt — an
        // `cancel_turn_with_pending_calls` gehen, sonst bliebe ein
        // `tool_call` ohne `tool_result` im Verlauf zurück (siehe dessen
        // Doku).
        let mut tool_call_iter = response.tool_calls.into_iter().enumerate();
        while let Some((position, call)) = tool_call_iter.next() {
            session.history_mut().push_tool_call(
                call.id.clone(),
                call.name.to_string(),
                call.arguments.clone(),
            );
            persist_last(session, store).await?;
            emit(
                session,
                TurnEvent::ToolCallRequested {
                    turn_id: handle.turn_id.clone(),
                    call_id: call.id.clone(),
                    tool_name: call.name.to_string(),
                    arguments: call.arguments.clone(),
                },
            );

            // a. Guardrail. Eine Entscheidung aus der Parallel-Vorprüfung wird
            // verbraucht statt neu eingeholt: ein Approval-Handler darf zu
            // demselben Call nicht zweimal befragt werden.
            // Runde 7, Teile A2/A5: sitzungsweite Wächter vor der Freigabe —
            // ein über das Lesebudget hinausgehender Aufruf wird abgelehnt,
            // ohne erst eine Freigabe einzuholen.
            let session_denial = apply_session_guard(
                session,
                store,
                &mut pending_guard_hint,
                call.name.as_str(),
                &call.arguments,
            )
            .await;
            let decision = match (session_denial, prepared.take(position)) {
                (Some(message), _) => ApprovalDecision::Deny(message),
                (None, Some(decision)) => decision,
                (None, None) => check_approval(session, &call).await,
            };
            match decision {
                ApprovalDecision::Allow => {}
                ApprovalDecision::Deny(reason) => {
                    let mut denied_result = ToolCallResult::error(format!("denied: {reason}"));
                    let abort_reason = apply_tool_guard(
                        session,
                        store,
                        guard.as_mut(),
                        &mut turn_seen_success_signatures,
                        &mut pending_guard_hint,
                        &mut round_progressed_by_tools,
                        call.name.as_str(),
                        &call.arguments,
                        &mut denied_result,
                    )
                    .await;
                    notify_tool_outcome(
                        session,
                        call.name.as_str(),
                        &call.arguments,
                        &denied_result,
                    );
                    notify_tool_progress(session);
                    session
                        .history_mut()
                        .push_tool_result(call.id.clone(), denied_result, 0);
                    persist_last(session, store).await?;
                    // Diesem Call ist bereits ein Tool-Result gepaart — nur
                    // die noch unangetasteten restlichen Calls des
                    // Iterators brauchen ein synthetisches Ergebnis.
                    if let Some(abort_reason) = abort_reason {
                        let remaining: Vec<ToolCall> =
                            tool_call_iter.map(|(_, call)| call).collect();
                        return cancel_turn_with_pending_calls(
                            session,
                            store,
                            handle,
                            total_usage,
                            abort_reason,
                            remaining,
                        )
                        .await;
                    }
                    continue;
                }
                ApprovalDecision::AskUser(request) => {
                    // Turn pausiert bis zur Nutzer-Entscheidung.
                    let actor = session
                        .spawn_context()
                        .and_then(|context| context.approval_actor.clone())
                        .ok_or_else(|| CoreError::MissingApprovalActor {
                            session_id: session.id().to_string(),
                        })?;

                    // approval.wait span: records the pause point before the
                    // turn suspends; kind is the Display form of the request ID.
                    let approval_span = tracing::info_span!(
                        "approval.wait",
                        approval_kind = %request,
                    );
                    let _approval_guard = approval_span.enter();

                    let paused_at = jiff::Timestamp::now();
                    if let Some(approvals) = approvals {
                        // Mandant der auslösenden Sitzung: die serverseitig
                        // aufgelöste Workspace-Bindung ihres Sandbox-Scopes.
                        // Mandantengebundene Leser (`approval.pending`,
                        // `approval.resolve`) filtern darauf.
                        let tenant = session
                            .spawn_context()
                            .map(|context| context.sandbox.workspace().tenant().clone());
                        approvals.issue(&ApprovalRecord {
                            request: request.clone(),
                            session: session.id().clone(),
                            call_id: call.id.clone(),
                            actor: actor.clone(),
                            issued_at: paused_at,
                            tenant,
                        })?;
                    }
                    session.begin_approval(call.clone(), request.clone(), actor, paused_at)?;
                    return Ok(TurnOutcome::AwaitingApproval {
                        call_id: call.id,
                        request,
                    });
                }
            }

            // a2. Zulassung beim `ToolCallMeter` (falls registriert): der
            // genehmigte Aufruf wird gebucht — und vom Meter festgeschrieben —,
            // BEVOR er ausgeführt wird, gleich ob normales Werkzeug, Handoff
            // oder lokal beantworteter Aufruf. Eine Abweisung beantwortet
            // diesen Call, gibt allen noch offenen Calls der Antwort ein
            // synthetisches Ergebnis und beendet den Turn wie ein Treffer des
            // Werkzeug-Budgets (`CancelReason::Budget`). Der Permit
            // (Lease-Zaun, siehe `crate::tool_call_meter`) lebt, bis das
            // Ergebnis dieses Aufrufs feststeht, und fällt spätestens am Ende
            // dieser Iteration — also vor der Zulassung des nächsten Calls.
            let permit = match admit_tool_call(&control, call.name.as_str()).await {
                Ok(permit) => permit,
                Err(refused) => {
                    notify_tool_outcome(session, call.name.as_str(), &call.arguments, &refused);
                    notify_tool_progress(session);
                    session
                        .history_mut()
                        .push_tool_result(call.id.clone(), refused, 0);
                    persist_last(session, store).await?;
                    let remaining: Vec<ToolCall> = tool_call_iter.map(|(_, call)| call).collect();
                    return cancel_turn_with_pending_calls(
                        session,
                        store,
                        handle,
                        total_usage,
                        CancelReason::Budget,
                        remaining,
                    )
                    .await;
                }
            };

            // b. Handoff-Detection (Runde 9, E3: auch `agent.message` an ein
            // eigenes, beendetes Kind — als Fortsetzung, [`message_resume_handoff`];
            // Plan R9, Teil C: auch `agents.delegate` über denselben Weg).
            // Plan R9, Teil C/E1: `agents.catalog` und eine benannte
            // Ablehnung (nicht delegierbar, Plan-Modus) sind Antworten an das
            // Modell, kein Turn-Abbruch.
            let (handoff, local_result) = delegation_step(session, &call);
            if let Some(mut result) = local_result {
                // Lokal beantwortet: das Ergebnis steht fest.
                drop(permit);
                let abort_reason = apply_tool_guard(
                    session,
                    store,
                    guard.as_mut(),
                    &mut turn_seen_success_signatures,
                    &mut pending_guard_hint,
                    &mut round_progressed_by_tools,
                    call.name.as_str(),
                    &call.arguments,
                    &mut result,
                )
                .await;
                notify_tool_outcome(session, call.name.as_str(), &call.arguments, &result);
                notify_tool_progress(session);
                session.history_mut().push_tool_result(call.id, result, 0);
                persist_last(session, store).await?;
                if let Some(abort_reason) = abort_reason {
                    let remaining: Vec<ToolCall> = tool_call_iter.map(|(_, call)| call).collect();
                    return cancel_turn_with_pending_calls(
                        session,
                        store,
                        handle,
                        total_usage,
                        abort_reason,
                        remaining,
                    )
                    .await;
                }
                continue;
            }
            if let Some((role, handoff_arguments)) = handoff {
                let spawner = session.registry().spawner().cloned().ok_or_else(|| {
                    CoreError::HandoffFailed {
                        role: role.clone(),
                        reason: "no AgentSpawner registered".to_owned(),
                    }
                })?;
                let spawn_input = SpawnInput {
                    parent_session_id: session.id().clone(),
                    handoff_call_id: call.id.clone(),
                    instructions: handoff_instructions(&handoff_arguments),
                    context: handoff_arguments.clone(),
                    // Siehe die Begründung an der Schwester-Konstruktionsstelle
                    // in `resume_after_approval_with_store`: hereditär, nie
                    // neu erfunden.
                    ceiling: session
                        .spawn_context()
                        .and_then(|spawn_context| spawn_context.ceiling.clone()),
                };
                let context = match governed_spawn_context(session) {
                    Ok(context) => context,
                    Err(error) => {
                        drop(permit);
                        let mut result = missing_tool_execution_context_result(error)?;
                        let abort_reason = apply_tool_guard(
                            session,
                            store,
                            guard.as_mut(),
                            &mut turn_seen_success_signatures,
                            &mut pending_guard_hint,
                            &mut round_progressed_by_tools,
                            call.name.as_str(),
                            &call.arguments,
                            &mut result,
                        )
                        .await;
                        notify_tool_outcome(session, call.name.as_str(), &call.arguments, &result);
                        notify_tool_progress(session);
                        session.history_mut().push_tool_result(call.id, result, 0);
                        persist_last(session, store).await?;
                        if let Some(abort_reason) = abort_reason {
                            let remaining: Vec<ToolCall> =
                                tool_call_iter.map(|(_, call)| call).collect();
                            return cancel_turn_with_pending_calls(
                                session,
                                store,
                                handle,
                                total_usage,
                                abort_reason,
                                remaining,
                            )
                            .await;
                        }
                        continue;
                    }
                };
                let spawned = spawner
                    .spawn_child(&role, spawn_input, context.sandbox, context.suggestions)
                    .await;
                // Runde 5, Teil K: eine Orchestrierungsgrenze (`[agents]`) ist
                // eine Antwort an das Modell — Werkzeugfehler statt Turn-Abbruch.
                // Runde 5, Teil J: ebenso eine abgelehnte Fortsetzung
                // (`continue_from`), damit das Modell nachsteuern kann; und
                // jede andere abgelehnte Admission (HARW-ORCH-001).
                if let Err(error) = &spawned {
                    drop(permit);
                    let mut result = ToolCallResult::error(spawn_rejection_text(&role, error));
                    let abort_reason = apply_tool_guard(
                        session,
                        store,
                        guard.as_mut(),
                        &mut turn_seen_success_signatures,
                        &mut pending_guard_hint,
                        &mut round_progressed_by_tools,
                        call.name.as_str(),
                        &call.arguments,
                        &mut result,
                    )
                    .await;
                    notify_tool_outcome(session, call.name.as_str(), &call.arguments, &result);
                    notify_tool_progress(session);
                    session.history_mut().push_tool_result(call.id, result, 0);
                    persist_last(session, store).await?;
                    if let Some(abort_reason) = abort_reason {
                        let remaining: Vec<ToolCall> =
                            tool_call_iter.map(|(_, call)| call).collect();
                        return cancel_turn_with_pending_calls(
                            session,
                            store,
                            handle,
                            total_usage,
                            abort_reason,
                            remaining,
                        )
                        .await;
                    }
                    continue;
                }
                // Jeder Fehler wurde oben als Werkzeug-Antwort beantwortet.
                let Ok(child) = spawned else {
                    continue;
                };
                let question = child_question(&handoff_arguments);
                emit(
                    session,
                    TurnEvent::ChildSpawned {
                        turn_id: handle.turn_id.clone(),
                        child: child.clone(),
                        role: role.clone(),
                        question: question.clone(),
                    },
                );

                // Background-only: admission above established the child
                // authority; the durable job runtime owns its lifecycle. The
                // start is a one-time return; there is no blocking fallback
                // (a missing submitter was refused before the spawn).
                let Some(result) =
                    detach_admitted_child(session, &spawner, &child, &role, question.as_deref())
                        .await
                else {
                    session.begin_handoff(child.clone(), call.id.clone(), role.clone())?;
                    return Ok(TurnOutcome::AwaitingChild {
                        child,
                        call_id: call.id,
                        role,
                    });
                };
                // Seiteneffekt-Grenze: Kind gespawnt und übergeben.
                drop(permit);
                notify_tool_outcome(session, call.name.as_str(), &call.arguments, &result);
                notify_tool_progress(session);
                session
                    .history_mut()
                    .push_tool_result(call.id.clone(), result.clone(), 0);
                persist_last(session, store).await?;
                emit(
                    session,
                    TurnEvent::ToolCallCompleted {
                        turn_id: handle.turn_id.clone(),
                        call_id: call.id,
                        result,
                        duration_ms: 0,
                        placement: None,
                    },
                );
                continue;
            }

            // c. Normale Tool-Ausführung — instrumented with tool.call span.
            let tool_name = call.name.to_string();
            // Wächter-Beratung vor der Ausführung (Addendum F+G): ein
            // Treffer wird sofort gemeldet, der Hinweis aber erst an das
            // Ergebnis DIESES Aufrufs angehängt, sobald es feststeht.
            // Runde 7, Teil B5: der Pitfall-Hinweis wird nicht mehr über
            // `pending_guard_hint` weitergereicht (dort konnte er an das
            // Ergebnis eines ANDEREN Werkzeugs rutschen, etwa wenn der Aufruf
            // abgebrochen wurde oder kein `TurnGuard` aktiv ist), sondern
            // unten direkt an das Ergebnis dieses Aufrufs gehängt — oder
            // verworfen.
            let pitfall_hint =
                apply_pitfall_advice(session, store, &tool_name, &call.arguments).await;
            let tool_span = tracing::info_span!("tool.call", tool_name = %tool_name);
            // Zweites Feld: `true`, wenn der Ausführer `ToolsError::Cancelled`
            // meldete (der `ToolExecutionContext` unten trägt `control`s
            // `CancelToken`, siehe `tool_execution_context`) — ausgewertet
            // NACH `.instrument(tool_span).await?`, um denselben Weg wie ein
            // `tool_checkpoint()`-Treffer zu nehmen statt das Ergebnis als
            // normalen Tool-Fehler ans Modell zurückzuspielen.
            // R18 D-D: das dritte Tupelfeld ist die Platzierung aus den
            // Ausführer-Metadaten (`ToolExecutor::execute_placed`).
            let (result, was_cancelled): (PlacedCallResult, bool) = async {
                match find_executor(session, &call.name) {
                    Some(executor) => {
                        let (result, duration_ms, cancelled, placement) = match tool_execution_context(
                            session,
                            ctx,
                            control.cancel_token(),
                        ) {
                            Ok(execution_context) => {
                                let execution_context = execution_context.with_lifetime_guard(permit.lifetime_guard());
                                let started = std::time::Instant::now();
                                // Fix B (Moduldoku „Siebter Nachtrag"): racet
                                // den Aufruf selbst gegen `control`s
                                // `CancelToken`, exakt wie der Modellaufruf-Race
                                // oben — ein Ausführer, der `ctx.cancel()`
                                // selbst nie abfragt, blockiert den Turn damit
                                // nicht mehr bis zu seinem eigenen Ende.
                                let (outcome, placement) = tokio::select! {
                                    biased;
                                    () = control.cancel_token().cancelled() => (Err(ToolsError::Cancelled), None),
                                    placed = executor.traced_execute_placed(&execution_context, &call) => {
                                        (placed.output, wire_placement(placed.placement))
                                    }
                                };
                                let duration_ms = started.elapsed().as_millis() as u64;
                                let cancelled = matches!(outcome, Err(ToolsError::Cancelled));
                                let result = match outcome {
                                    Ok(output) => output_to_result(output),
                                    Err(e) => ToolCallResult::error(e.to_string()),
                                };
                                (result, duration_ms, cancelled, placement)
                            }
                            Err(error) => {
                                (missing_tool_execution_context_result(error)?, 0, false, None)
                            }
                        };
                        tracing::info!(
                            duration_ms = duration_ms,
                            status = if result.is_success() { "ok" } else { "err" },
                            "tool.execute",
                        );
                        Ok::<_, CoreError>(((result, duration_ms, placement), cancelled))
                    }
                    None => {
                        tracing::info!(duration_ms = 0u64, status = "err", "tool.execute",);
                        Ok::<_, CoreError>((
                            (
                                ToolCallResult::error(format!(
                                    "no executor for tool '{}'",
                                    call.name
                                )),
                                0u64,
                                None,
                            ),
                            false,
                        ))
                    }
                }
            }
            .instrument(tool_span)
            .await?;
            // Seiteneffekt-Grenze: das Werkzeug ist gelaufen (oder
            // abgebrochen), sein Ergebnis steht fest.
            drop(permit);
            if was_cancelled {
                // Derselbe Weg wie ein Vorab-`tool_checkpoint()`-Treffer
                // (Moduldoku „W4a A-LOOP"): der laufende Call bekommt sein
                // eigenes (bereits als Cancel-Fehler geformtes) Ergebnis —
                // sein `tool_call`-Eintrag steht schon im Verlauf (oben, vor
                // dieser Runde) — und alle noch nicht gestarteten Calls
                // derselben Antwort gehen über `cancel_turn_with_pending_calls`
                // denselben Weg wie die übrigen Prüfpunkt-Treffer dieser
                // Schleife.
                let (result_value, result_duration_ms, result_placement) = result;
                emit(
                    session,
                    TurnEvent::ToolCallCompleted {
                        turn_id: handle.turn_id.clone(),
                        call_id: call.id.clone(),
                        result: result_value.clone(),
                        duration_ms: result_duration_ms,
                        placement: result_placement,
                    },
                );
                notify_tool_outcome(session, &tool_name, &call.arguments, &result_value);
                notify_tool_progress(session);
                session
                    .history_mut()
                    .push_tool_result(call.id, result_value, result_duration_ms);
                persist_last(session, store).await?;
                let remaining: Vec<ToolCall> = tool_call_iter.map(|(_, call)| call).collect();
                return cancel_turn_with_pending_calls(
                    session,
                    store,
                    handle,
                    total_usage,
                    control
                        .cancel_token()
                        .reason()
                        .unwrap_or(CancelReason::User),
                    remaining,
                )
                .await;
            }
            let (mut result_value, result_duration_ms, result_placement) = result;
            // Runde 7, Teil B5: Pitfall-Hinweis nur an das Ergebnis desselben
            // Aufrufs (gleiches Werkzeug).
            if let Some(hint) = pitfall_hint.as_deref() {
                append_hint(&mut result_value, hint);
            }
            let abort_reason = apply_tool_guard(
                session,
                store,
                guard.as_mut(),
                &mut turn_seen_success_signatures,
                &mut pending_guard_hint,
                &mut round_progressed_by_tools,
                &tool_name,
                &call.arguments,
                &mut result_value,
            )
            .await;
            let call_id_for_event = call.id.clone();
            emit(
                session,
                TurnEvent::ToolCallCompleted {
                    turn_id: handle.turn_id.clone(),
                    call_id: call_id_for_event,
                    result: result_value.clone(),
                    duration_ms: result_duration_ms,
                    placement: result_placement,
                },
            );
            // d. ToolResult in History.
            notify_tool_outcome(session, &tool_name, &call.arguments, &result_value);
            notify_tool_progress(session);
            session
                .history_mut()
                .push_tool_result(call.id, result_value, result_duration_ms);
            persist_last(session, store).await?;
            if let Some(abort_reason) = abort_reason {
                let remaining: Vec<ToolCall> = tool_call_iter.map(|(_, call)| call).collect();
                return cancel_turn_with_pending_calls(
                    session,
                    store,
                    handle,
                    total_usage,
                    abort_reason,
                    remaining,
                )
                .await;
            }
        }

        // Runden-Ende-Beobachtung (Addendum F+G): alle Tool-Ergebnisse dieser
        // Runde stehen bereits fest (sequenzieller Pfad; der Parallel-Pfad
        // `try_execute_parallel_calls` springt per `continue` direkt zurück
        // zu Schritt 4 und überspringt diese Stelle ebenso wie die
        // Auto-Compaction unten — dieselbe, bereits bestehende Lücke).
        if let Some(g) = guard.as_mut() {
            match g.observe_round_end(round_progressed_by_tools) {
                GuardVerdict::Continue => {}
                GuardVerdict::Warn { event, hint } => {
                    report_drift(session, store, &event).await;
                    pending_guard_hint = Some(match pending_guard_hint.take() {
                        Some(existing) => format!("{existing}\n\n{hint}"),
                        None => hint,
                    });
                }
                GuardVerdict::Abort { event, hint } => {
                    report_drift(session, store, &event).await;
                    tracing::warn!(hint = %hint, "turn_loop.guard_abort");
                    note_guard_stop(session, &event);
                    return cancel_turn(session, handle, total_usage, CancelReason::Budget).await;
                }
            }
        }

        // Auto-Compaction an einer sicheren Grenze: alle Tool-Ergebnisse
        // dieser Runde sind bereits in der Historie, der nächste Model-
        // Request ist aber noch nicht gebaut — hier mutiert `compact_session`
        // die Historie also nie eine in-flight-Anfrage an. Höchstens einmal
        // je Runde (dieser Call-Site läuft genau einmal pro Schleifendurchlauf).
        //
        // `task_completed` wird vor dem Aufruf in eine eigene Variable
        // gelegt: `maybe_compact` nimmt `session` mutable entgegen, ein
        // verschachtelter `turn_completed_a_plan_step(session)`-Aufruf als
        // Argumentausdruck würde eine gleichzeitige unveränderliche Ausleihe
        // gegen die bereits laufende veränderliche Ausleihe erzeugen.
        // Runde 5, Teil M: Nachrichten von Elternteil/Kindern an der
        // Runden-Grenze einreihen; letzten Assistententext ins Journal.
        deliver_round_boundary(session);
        let task_completed = turn_completed_a_plan_step(session);
        let appended_tokens = appended_tokens_since(session, history_mark);
        maybe_compact(
            session,
            model,
            store,
            &last_round_usage,
            appended_tokens,
            task_completed,
            &mut last_compaction_tokens_after,
        )
        .await;

        // Zurück zu Schritt 4 (nächster Model-Call).
    }

    // Welle FANIN-K: ein Wächter-Hinweis, der am Ende der letzten Runde
    // entstand (`response_had_no_tool_calls`-Zweig oben), aber keinem
    // weiteren Tool-Ergebnis mehr zugeordnet werden konnte, weil die Runde
    // ohne Tool-Aufrufe endete, ging bislang spurlos verloren — er wird
    // stattdessen an das letzte Item der Historie angehängt.
    if let Some(hint) = pending_guard_hint.take() {
        session.history_mut().append_hint_to_last(&hint);
    }

    // 7. Observer + Turn-Ende.
    emit(
        session,
        TurnEvent::TurnCompleted {
            turn_id: handle.turn_id.clone(),
            usage: Some(total_usage.clone()),
        },
    );
    notify_turn_stop(
        session,
        &TurnStopInput {
            session_id: handle.session_id.clone(),
            turn_id: handle.turn_id.clone(),
            token_usage: total_usage.clone(),
        },
    );
    // Auto-Compaction am Ende eines erfolgreich abgeschlossenen Turns — die
    // zweite sichere Grenze neben der innerhalb der Schleife oben: kein
    // in-flight-Request existiert mehr, der nächste Turn beginnt erst mit
    // dem nächsten `run_turn`-Aufruf. `task_completed` wird wie oben vor dem
    // Aufruf ausgewertet (siehe Kommentar an der ersten Call-Site).
    let task_completed = turn_completed_a_plan_step(session);
    let appended_tokens = appended_tokens_since(session, history_mark);
    maybe_compact(
        session,
        model,
        store,
        &last_round_usage,
        appended_tokens,
        task_completed,
        &mut last_compaction_tokens_after,
    )
    .await;

    // Projektgedächtnis: alle Tool-Ergebnisse dieses Turns wurden bereits
    // über `notify_tool_outcome` gemeldet; hier, am erfolgreichen Turn-Ende,
    // erfährt der Beobachter, dass die Runde abgeschlossen ist.
    crate::turn_feedback::notify_turn_texts(session);
    if let Some(observer) = session.tool_outcome_observer().cloned() {
        observer.on_turn_finished(session.id());
    }

    // `drive_turn` liefert `CoreResult<TurnOutcome>` — anders als
    // `transition_after_turn_failure` (das nichts zurückgeben kann und deshalb
    // protokolliert) kann dieser Aufrufer den Fehler ehrlich weiterreichen:
    // ein Turn, der sich nicht nach `Idle` zurückschreiben lässt, ist kein
    // `Completed`. Der `?`-Aufrufer (`run_turn_with_approvals` &co.) fängt den
    // Fehler bereits über `transition_after_turn_failure` ab.
    session.complete_turn(handle, total_usage)?;
    if control.title_seed_pending() &&
        store.finish_title_seed(&ctx.session_id, &ctx.turn_id, true, final_title_location).await.is_err()
    {
        // A pending record left by ambiguous I/O blocks future title evidence.
        // Never fail or replay the already-completed inference because of optional indexing.
        tracing::warn!("title source completion was not confirmed");
    }
    Ok(TurnOutcome::Completed)
}


/// Beendet einen Turn nach einem [`TurnControl`]-Prüfpunkt-Treffer (Abbruch
/// oder Budget), bevor ein Modellaufruf gestartet wurde: keine offenen
/// Tool-Calls, deshalb kein Verlaufs-Nacharbeiten nötig.
///
/// # Beschreibung
/// Spiegelt den regulären Abschluss-Pfad am Ende von [`drive_turn`]
/// (`TurnAborted` statt `TurnCompleted`, sonst identisch): Observer werden
/// benachrichtigt und die Session kehrt über `AgentSession::complete_turn`
/// nach `Idle` zurück — ein Abbruch ist kein stiller Ausstieg, sondern ein
/// regulär abgeschlossener Turn mit `CancelReason` statt Erfolg.
///
/// # Errors
/// Reicht einen Fehler von `AgentSession::complete_turn` durch (z. B. wenn
/// `handle` nicht mehr der aktive Turn der Session ist).
async fn cancel_turn(
    session: &mut AgentSession,
    handle: TurnHandle,
    total_usage: harw_types::TokenUsage,
    reason: CancelReason,
) -> CoreResult<TurnOutcome> {
    cancel_owned_background_children(session, reason);
    emit(
        session,
        TurnEvent::TurnAborted {
            turn_id: handle.turn_id.clone(),
        },
    );
    notify_turn_stop(
        session,
        &TurnStopInput {
            session_id: handle.session_id.clone(),
            turn_id: handle.turn_id.clone(),
            token_usage: total_usage.clone(),
        },
    );
    session.complete_turn(handle, total_usage)?;
    Ok(TurnOutcome::Cancelled { reason })
}

enum ModelStopOutcome {
    OutputLimit,
    ContextLimit,
    IncompleteUnknown,
    ProviderStopCode(String),
    StreamInterrupted,
    StreamTransportInterrupted,
    ContentFiltered,
    Refused { detail: Option<String> },
}

/// Beendet einen Turn nach einem Provider-Stop-Grund, der kein reguläres
/// Turn-Ende ist ([`StopReason::MaxTokens`] nach ausgeschöpften
/// Auto-Continuations, [`StopReason::ContextWindowExceeded`],
/// [`StopReason::StreamInterrupted`], [`StopReason::IncompleteUnknown`], [`StopReason::Refusal`],
/// [`StopReason::ContentFilter`]).
///
/// # Beschreibung
/// Spiegelt den regulären Abschluss-Pfad am Ende von [`drive_turn`]
/// (Guard-Hinweis, Observer, `complete_turn`), endet aber mit
/// dem ehrlichen [`TurnOutcome::Truncated`]- bzw. [`TurnOutcome::Refused`]-Ausgang
/// statt `Completed`. Die bereits erhaltene Assistant-Antwort (inklusive
/// abgeschnittener Teiltext) ist zu diesem Zeitpunkt bereits persistiert; das
/// `complete_turn` hier sorgt dafür, dass die Session nach `Idle` zurückkehrt
/// und der nächste Turn sauber starten kann.
///
/// # Errors
/// Reicht einen Fehler von `AgentSession::complete_turn` durch (z. B. wenn
/// `handle` nicht mehr der aktive Turn der Session ist).
async fn finish_model_stop(
    session: &mut AgentSession,
    store: &dyn StateStore,
    handle: TurnHandle,
    total_usage: harw_types::TokenUsage,
    pending_guard_hint: Option<String>,
    stop: ModelStopOutcome,
) -> CoreResult<TurnOutcome> {
    let (outcome, incomplete) = match stop {
        ModelStopOutcome::OutputLimit => (
            TurnOutcome::Truncated,
            Some((
                "Antwort unvollständig: Das Ausgabelimit des Anbieters wurde erreicht.".to_owned(),
                true,
            )),
        ),
        ModelStopOutcome::ContextLimit => (
            TurnOutcome::Truncated,
            Some((
                "Antwort unvollständig: Das Kontextfenster des Modells wurde überschritten.".to_owned(),
                true,
            )),
        ),
        ModelStopOutcome::IncompleteUnknown => (
            TurnOutcome::Truncated,
            Some((
                "Antwort unvollständig: Der Anbieter meldete ein unvollständiges Ergebnis. Die Ursache ist unbekannt.".to_owned(),
                false,
            )),
        ),
        ModelStopOutcome::ProviderStopCode(code) => {
            let code = code.trim();
            let valid = !code.is_empty() && code.len() <= 64
                && code.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
            let message = if valid {
                format!("Antwort unvollständig: Der Anbieter hat die Antwort mit dem Statuscode {code} beendet.")
            } else {
                "Antwort unvollständig: Der Anbieter hat die Antwort beendet, aber keinen verständlichen Grund übermittelt.".to_owned()
            };
            (TurnOutcome::Truncated, Some((message, false)))
        }
        ModelStopOutcome::StreamInterrupted => (
            TurnOutcome::Truncated,
            Some((
                "Antwort unvollständig: Der Antwortstream endete vor der Abschlussmeldung. Die Ursache ist unbekannt.".to_owned(),
                false,
            )),
        ),
        ModelStopOutcome::StreamTransportInterrupted => (
            TurnOutcome::Truncated,
            Some((
                "Antwort unvollständig: Die Übertragung der Anbieterantwort wurde unterbrochen oder hat zu lange keine Daten geliefert.".to_owned(),
                false,
            )),
        ),
        ModelStopOutcome::ContentFiltered => (
            TurnOutcome::Refused { detail: None },
            Some((
                "Antwort unvollständig: Der Inhaltsfilter des Anbieters hat die Ausgabe gestoppt. Kein genauerer Grund wurde übermittelt.".to_owned(),
                false,
            )),
        ),
        ModelStopOutcome::Refused { detail } => {
            let display_detail = detail
                .as_deref()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
                .take(240)
                .collect::<String>();
            let display_detail = display_detail.trim();
            let message = if display_detail.is_empty() {
                "Antwort unvollständig: Das Modell hat die Antwort abgelehnt. Kein genauerer Grund wurde übermittelt.".to_owned()
            } else {
                format!("Antwort unvollständig: Das Modell hat die Antwort abgelehnt. Grund laut Anbieter: {display_detail}")
            };
            (TurnOutcome::Refused { detail }, Some((message, false)))
        }
    };
    // Welle FANIN-K: ein Hinweis, der am Ende der letzten Runde entstand,
    // aber keinem Tool-Ergebnis mehr zugeordnet werden konnte, geht auch an
    // dieser Endgrenze nicht verloren — er wird ans letzte Verlaufs-Item
    // angehängt (identisch zum regulären Abschluss-Pfad).
    if let Some(hint) = pending_guard_hint {
        session.history_mut().append_hint_to_last(&hint);
    }
    if let Some((message, retryable)) = incomplete {
        // Persist before the terminal event: a client can reattach as soon as
        // it sees that event, missing any subsequent transient host notice.
        session.history_mut().push_error(message.clone(), retryable);
        persist_last(session, store).await?;
        if let Some(item) = session.history().last().cloned() {
            emit(
                session,
                TurnEvent::ItemAdded {
                    turn_id: handle.turn_id.clone(),
                    item,
                },
            );
        }
        emit(
            session,
            TurnEvent::TurnFailed {
                turn_id: handle.turn_id.clone(),
                reason: message,
                retryable,
            },
        );
    } else {
        emit(
            session,
            TurnEvent::TurnCompleted {
                turn_id: handle.turn_id.clone(),
                usage: Some(total_usage.clone()),
            },
        );
    }
    notify_turn_stop(
        session,
        &TurnStopInput {
            session_id: handle.session_id.clone(),
            turn_id: handle.turn_id.clone(),
            token_usage: total_usage.clone(),
        },
    );
    // Kein Auto-Compact an dieser Grenze: ein abgeschnittener oder
    // abgelehnter Turn ist keine sichere Kompaktionsgrenze; die Policy greift
    // spätestens am regulär abgeschlossenen Folge-Turn.
    if let Some(observer) = session.tool_outcome_observer().cloned() {
        observer.on_turn_finished(session.id());
    }
    session.complete_turn(handle, total_usage)?;
    Ok(outcome)
}

/// Wie [`cancel_turn`], aber für einen Treffer am Werkzeug-Prüfpunkt: `calls`
/// sind die noch nicht ausgeführten Tool-Calls der aktuellen Modellantwort.
///
/// # Beschreibung
/// Jeder Call bekommt zuerst seinen Aufruf-Eintrag (der Verlauf muss zeigen,
/// dass das Modell ihn angefordert hat) und dann ein synthetisches
/// Fehlerergebnis im Verlauf — ohne diese Paarung wäre der Verlauf beim
/// nächsten Modellaufruf nicht provider-gültig (ein `tool_call` ohne
/// zugehöriges `tool_result`). Anschließend endet der Turn wie [`cancel_turn`].
///
/// # Errors
/// Reicht Persistenzfehler (`persist_last`) und Fehler von [`cancel_turn`]
/// durch.
async fn cancel_turn_with_pending_calls(
    session: &mut AgentSession,
    store: &dyn StateStore,
    handle: TurnHandle,
    total_usage: harw_types::TokenUsage,
    reason: CancelReason,
    calls: Vec<ToolCall>,
) -> CoreResult<TurnOutcome> {
    for call in calls {
        session.history_mut().push_tool_call(
            call.id.clone(),
            call.name.to_string(),
            call.arguments.clone(),
        );
        persist_last(session, store).await?;
        session.history_mut().push_tool_result(
            call.id,
            ToolCallResult::error(format!("turn cancelled before execution ({reason:?})")),
            0,
        );
        persist_last(session, store).await?;
    }
    cancel_turn(session, handle, total_usage, reason).await
}

/// Beantwortet Werkzeugaufrufe über dem Werkzeug-Budget, ohne sie
/// auszuführen, und beendet den Turn mit `CancelReason::Budget` (siehe
/// [`TurnControl::with_tool_call_budget`]). Jeder `tool_call` bekommt sein
/// `tool_result`, der Verlauf bleibt provider-gültig.
async fn skip_calls_over_tool_budget(
    session: &mut AgentSession,
    store: &dyn StateStore,
    handle: TurnHandle,
    total_usage: harw_types::TokenUsage,
    calls: Vec<ToolCall>,
    message: &str,
) -> CoreResult<TurnOutcome> {
    for call in calls {
        session.history_mut().push_tool_call(
            call.id.clone(),
            call.name.to_string(),
            call.arguments.clone(),
        );
        persist_last(session, store).await?;
        session.history_mut().push_tool_result(
            call.id,
            ToolCallResult::error(message.to_owned()),
            0,
        );
        persist_last(session, store).await?;
    }
    cancel_turn(session, handle, total_usage, CancelReason::Budget).await
}

/// Bucht einen genehmigten Werkzeugaufruf beim [`ToolCallMeter`] des
/// Steuerblocks, bevor irgendetwas davon ausgeführt wird.
///
/// # Beschreibung
/// Ohne registrierten Meter ein No-op ([`ToolCallPermit::unguarded`]). Weist
/// der Meter den Aufruf ab, wird der Grund als Stop-Grund des Turns vermerkt
/// ([`TurnControl::stop_detail`]) und als Fehlerergebnis mit
/// [`TOOL_CALL_REFUSED_PREFIX`] zurückgegeben. Der Aufrufer führt den Aufruf
/// dann **nicht** aus, beantwortet ihn mit diesem Ergebnis und beendet den
/// Turn mit `CancelReason::Budget`.
///
/// # Returns
/// `Ok(permit)`, wenn der Aufruf laufen darf: der Aufrufer hält `permit`,
/// bis das Ergebnis dieses Aufrufs feststeht (Lease-Zaun, siehe
/// [`crate::tool_call_meter`]); `Err(result)`, wenn er abgewiesen wurde.
async fn admit_tool_call(
    control: &TurnControl,
    tool: &str,
) -> Result<ToolCallPermit, ToolCallResult> {
    let Some(meter) = control.tool_call_meter() else {
        return Ok(ToolCallPermit::unguarded());
    };
    match meter.admit_tool_call(tool).await {
        Ok(permit) => Ok(permit),
        Err(reason) => {
            tracing::warn!(tool = %tool, reason = %reason, "turn_loop.tool_call_refused");
            control.note_stop_detail(format!("Werkzeugaufruf abgewiesen: {reason}"));
            Err(ToolCallResult::error(format!(
                "{TOOL_CALL_REFUSED_PREFIX}: {reason}"
            )))
        }
    }
}

/// Execute a complete model response concurrently only when every call is an
/// explicitly parallel-safe ordinary tool and every guardrail allows it. Any
/// ambiguity falls back to the sequential path below; handoffs and approvals
/// are never raced.
///
/// # Beschreibung
/// Die Bedingungen werden in dieser Reihenfolge geprüft, und die Reihenfolge ist
/// Absicht:
/// 1. Mindestens zwei Calls, kein Handoff darunter.
/// 2. Für **jeden** Call ein `parallel_safe` markierter Executor
///    ([`find_parallel_executor`]).
/// 3. Ein aufgelöster Sandbox-Kontext.
/// 4. Erst danach die Approval-Vorprüfung über [`check_approval`].
///
/// Die Guardrail-Prüfung steht bewusst zuletzt: sie ist der einzige Schritt mit
/// Außenwirkung (Nutzer-Interaktion, Audit-Eintrag, vergebene `ItemId`). Würde
/// sie vor einer Bedingung stehen, die den Parallel-Pfad noch verwirft, hätte
/// ein Handler umsonst gefragt.
///
/// Früher genügte ein einziger registrierter `ApprovalHandler`, um die
/// Parallelisierung komplett abzuschalten. Da `harw-registry-defaults`
/// produktiv immer eine Policy registriert, war der `JoinSet`-Pfad damit nie
/// aktiv. Statt des Pauschalausschlusses prüft die Vorprüfung nun Call für
/// Call: nur wenn **alle** `Allow` liefern, wird parallel ausgeführt.
///
/// # Invariante (kein doppelter Handler-Aufruf)
/// Im Parallel-Pfad läuft [`check_approval`] genau einmal je Call und danach
/// nicht mehr — der sequenzielle Loop wird gar nicht erst betreten. Liefert ein
/// Call `Deny` oder `AskUser`, bricht die Vorprüfung ab und legt die bereits
/// eingeholten Entscheidungen in `prepared` ab; der sequenzielle Pfad verbraucht
/// sie dort, statt dieselben Handler ein zweites Mal zu fragen. In beiden
/// Fällen gilt: höchstens ein Handler-Aufruf pro Call und Modellantwort.
///
/// # Arguments
/// - `prepared` (`&mut PreparedApprovals`): Ausgabekanal für die Entscheidungen
///   der Vorprüfung. Wird nur beschrieben, wenn Schritt 4 erreicht wurde.
/// - `control` (`&TurnControl`): liefert den `CancelToken`, der an den
///   gemeinsamen `ToolExecutionContext` aller Jobs dieser Antwort angehängt
///   wird (siehe `tool_execution_context`). Ein Job, der daraufhin
///   `ToolsError::Cancelled` meldet, geht in der Ergebnis-Auslieferung
///   denselben `Aborted`-Weg wie ein `TurnGuard`-Abbruch, statt als normaler
///   Tool-Fehler ans Modell zurückgespielt zu werden.
///
/// # Returns
/// `true`, wenn die Antwort vollständig parallel ausgeführt und persistiert
/// wurde; `false`, wenn der Aufrufer den sequenziellen Pfad nehmen muss.
/// Ergebnis von [`try_execute_parallel_calls`] für die Turn-Wächter-Anbindung
/// (Addendum F+G): der Aufrufer (`drive_turn`) kennt `handle`/`total_usage`
/// und beendet einen `Aborted`-Turn deshalb selbst über [`cancel_turn`].
enum ParallelOutcome {
    /// Der Parallel-Pfad war nicht anwendbar — der Aufrufer muss den
    /// sequenziellen Pfad nehmen.
    NotApplicable,
    /// Alle Calls wurden parallel ausgeführt und persistiert.
    Executed,
    /// Ein `TurnGuard`-`Abort` ist mitten in der Ergebnisauslieferung
    /// aufgetreten; alle Ergebnisse sind bereits (ggf. synthetisch) im
    /// Verlauf gepaart.
    Aborted(CancelReason),
}

#[allow(clippy::too_many_arguments)]
async fn try_execute_parallel_calls(
    session: &mut AgentSession,
    store: &dyn StateStore,
    ctx: &TurnInputContext,
    calls: &[ToolCall],
    prepared: &mut PreparedApprovals,
    mut guard: Option<&mut TurnGuard>,
    seen_success_signatures: &mut HashSet<String>,
    pending_hint: &mut Option<String>,
    round_progressed: &mut bool,
    control: &TurnControl,
) -> CoreResult<ParallelOutcome> {
    if calls.len() < 2
        || calls.iter().any(|call| {
            handoff_role(&call.name).is_some()
                || call.name.as_str() == AGENTS_DELEGATE_TOOL
                || call.name.as_str() == AGENTS_CATALOG_TOOL
                || message_resume_handoff(session, call).is_some()
        })
    {
        return Ok(ParallelOutcome::NotApplicable);
    }
    // Runde 7, Teile A2/A5: Lesebudget und Polling-Erkennung zählen je
    // Aufruf in Reihenfolge — solche Antworten laufen sequenziell.
    if calls
        .iter()
        .any(|call| session_guard_applies(session, call.name.as_str()))
    {
        return Ok(ParallelOutcome::NotApplicable);
    }
    let mut jobs = Vec::with_capacity(calls.len());
    for call in calls {
        let Some(executor) = find_parallel_executor(session, &call.name) else {
            return Ok(ParallelOutcome::NotApplicable);
        };
        jobs.push((call.clone(), executor));
    }
    let execution_context = match tool_execution_context(session, ctx, control.cancel_token()) {
        Ok(context) => context,
        Err(error) => {
            missing_tool_execution_context_result(error)?;
            return Ok(ParallelOutcome::NotApplicable);
        }
    };
    if !preflight_approvals(session, calls, prepared).await {
        return Ok(ParallelOutcome::NotApplicable);
    }

    for (call, _) in &jobs {
        session.history_mut().push_tool_call(
            call.id.clone(),
            call.name.to_string(),
            call.arguments.clone(),
        );
        persist_last(session, store).await?;
        emit(
            session,
            TurnEvent::ToolCallRequested {
                turn_id: ctx.turn_id.clone(),
                call_id: call.id.clone(),
                tool_name: call.name.to_string(),
                arguments: call.arguments.clone(),
            },
        );
    }

    // Fix C (Moduldoku „Siebter Nachtrag"): Metadaten je Position VOR dem
    // Spawn-Loop mitschneiden, der `jobs` konsumiert — der spätere
    // Cancel-Fill-in unten braucht `call_id`/`tool_name`/`arguments` je
    // Slot, kann sie nach `jobs.into_iter()` aber nicht mehr aus `jobs`
    // lesen.
    let call_meta: Vec<(ToolCallId, String, serde_json::Value)> = jobs
        .iter()
        .map(|(call, _executor)| {
            (
                call.id.clone(),
                call.name.to_string(),
                call.arguments.clone(),
            )
        })
        .collect();
    let mut results: Vec<ParallelCallSlot> = vec![None; calls.len()];
    // Zulassung beim `ToolCallMeter` (falls registriert), Call für Call in
    // Antwort-Reihenfolge und unmittelbar vor dem Start des jeweiligen
    // Jobs: jeder Aufruf ist gebucht, bevor er läuft. Die davor zugelassenen
    // Jobs laufen weiter; der erste abgewiesene und alle folgenden werden
    // nicht gestartet, sondern bekommen das Abweisungsergebnis. Ein Meter
    // wird nach der ersten Abweisung nicht erneut befragt. Jeder gestartete
    // Job bekommt seinen eigenen Permit (Lease-Zaun, siehe
    // `crate::tool_call_meter`) als Wert und lässt ihn nach seiner
    // Ausführung fallen; ein Abbruch (`abort_all`) lässt ihn mit dem Job
    // fallen.
    let mut refusal: Option<ToolCallResult> = None;
    let mut joins = tokio::task::JoinSet::new();
    for (position, (call, executor)) in jobs.into_iter().enumerate() {
        let mut permit = None;
        if refusal.is_none() {
            match admit_tool_call(control, call.name.as_str()).await {
                Ok(admitted) => permit = Some(admitted),
                Err(refused) => refusal = Some(refused),
            }
        }
        // Ohne Permit startet kein Job: dieser oder ein früherer Aufruf wurde
        // abgewiesen. Fehlte `refusal` dennoch, bliebe der Slot leer und die
        // Auslieferung unten scheiterte geschlossen.
        let Some(permit) = permit else {
            if let (Some(refused), Some(slot)) = (&refusal, results.get_mut(position)) {
                *slot = Some((
                    call.id.clone(),
                    refused.clone(),
                    0,
                    call.name.to_string(),
                    call.arguments.clone(),
                    false,
                    None,
                ));
            }
            continue;
        };
        let execution_context = execution_context
            .clone()
            .with_lifetime_guard(permit.lifetime_guard());
        let tool_name = call.name.to_string();
        let tool_span = tracing::info_span!("tool.call", tool_name = %tool_name);
        let arguments_for_capture = call.arguments.clone();
        let tool_name_for_capture = tool_name.clone();
        joins.spawn(
            async move {
                let started = std::time::Instant::now();
                let placed = executor
                    .traced_execute_placed(&execution_context, &call)
                    .await;
                // Seiteneffekt-Grenze dieses Jobs: das Werkzeug ist gelaufen.
                drop(permit);
                let placement = wire_placement(placed.placement);
                let output = placed.output;
                let cancelled = matches!(output, Err(ToolsError::Cancelled));
                let result = output
                    .map(output_to_result)
                    .unwrap_or_else(|error| ToolCallResult::error(error.to_string()));
                let duration_ms = started.elapsed().as_millis() as u64;
                tracing::info!(
                    duration_ms = duration_ms,
                    status = if result.is_success() { "ok" } else { "err" },
                    "tool.execute",
                );
                (
                    position,
                    call.id,
                    result,
                    duration_ms,
                    tool_name_for_capture,
                    arguments_for_capture,
                    cancelled,
                    placement,
                )
            }
            .instrument(tool_span),
        );
    }
    // Fix C (Moduldoku „Siebter Nachtrag"): dieser Join-Loop wartete bisher
    // ungeraced auf **alle** Aufrufe, bevor überhaupt auf Cancel reagiert
    // wurde — ein Ausführer, der `ctx.cancel()` selbst nie abfragt, blockierte
    // die gesamte Antwort bis zu seinem eigenen Ende. Jetzt racet die
    // Reaper-Schleife selbst gegen `control`s `CancelToken`: ein Treffer
    // bricht alle noch laufenden Jobs über `joins.abort_all()` ab, statt
    // weiter auf sie zu warten.
    let mut cancel_hit = false;
    loop {
        tokio::select! {
            biased;
            () = control.cancel_token().cancelled(), if !cancel_hit => {
                cancel_hit = true;
                joins.abort_all();
            }
            joined = joins.join_next() => {
                match joined {
                    Some(Ok((position, call_id, result, duration_ms, tool_name, arguments, cancelled, placement))) => {
                        results[position] = Some((
                            call_id,
                            result,
                            duration_ms,
                            tool_name,
                            arguments,
                            cancelled,
                            placement,
                        ));
                    }
                    Some(Err(error)) => {
                        // `abort_all()` selbst kann noch laufende Jobs als
                        // `Err(JoinError)` zurückliefern — erwartet, sobald
                        // `cancel_hit` bereits `true` ist: ihr `None`-Slot
                        // wird gleich unten synthetisch befüllt, das Ergebnis
                        // hier wird verworfen. Ohne vorherigen Cancel-Treffer
                        // ist ein `JoinError` (z. B. ein echter Panic) weiterhin
                        // ein echter Fehler dieser Funktion.
                        if !cancel_hit {
                            return Err(CoreError::ToolFailed(error.to_string()));
                        }
                    }
                    None => break,
                }
            }
        }
    }
    if cancel_hit {
        for (position, slot) in results.iter_mut().enumerate() {
            if slot.is_none() {
                let (call_id, tool_name, arguments) = call_meta[position].clone();
                *slot = Some((
                    call_id,
                    ToolCallResult::error(format!(
                        "turn cancelled before delivery ({:?})",
                        control
                            .cancel_token()
                            .reason()
                            .unwrap_or(CancelReason::User)
                    )),
                    0,
                    tool_name,
                    arguments,
                    true, // cancelled
                    None, // nie ausgeliefert: Platzierung unbekannt
                ));
            }
        }
    }
    // `while let` über den Positions-Iterator statt `for`: bei einem
    // `TurnGuard`-`Abort` mitten in der Auslieferung (Addendum F+G) brauchen
    // die noch nicht ausgelieferten, aber bereits abgeschlossenen Ergebnisse
    // weiterhin ein synthetisches `tool_result` — ihr `tool_call` steht schon
    // im Verlauf (oben, vor dem Start der parallelen Ausführung).
    let mut results_iter = results.into_iter();
    let mut aborted: Option<CancelReason> = None;
    for result in results_iter.by_ref() {
        let (call_id, mut value, duration_ms, tool_name, arguments, cancelled, placement) = result
            .ok_or_else(|| {
                CoreError::ToolFailed("parallel tool scheduler lost a completed call".to_owned())
            })?;
        // `ToolsError::Cancelled` (der `ToolExecutionContext` trägt `control`s
        // `CancelToken`, siehe der Konstruktion oben) geht denselben Weg wie
        // ein `TurnGuard`-`Abort`: kein normaler Tool-Fehler ans Modell,
        // sondern derselbe Prüfpunkt-Treffer-Pfad — die Wächter-Beratung
        // (`apply_tool_guard`) wird für diesen Call übersprungen, sein bereits
        // korrekt geformtes Cancel-Ergebnis (`value`) wird unverändert
        // ausgeliefert.
        let abort_reason = if cancelled {
            Some(
                control
                    .cancel_token()
                    .reason()
                    .unwrap_or(CancelReason::User),
            )
        } else {
            apply_tool_guard(
                session,
                store,
                guard.as_deref_mut(),
                seen_success_signatures,
                pending_hint,
                round_progressed,
                &tool_name,
                &arguments,
                &mut value,
            )
            .await
        };
        emit(
            session,
            TurnEvent::ToolCallCompleted {
                turn_id: ctx.turn_id.clone(),
                call_id: call_id.clone(),
                result: value.clone(),
                duration_ms,
                placement,
            },
        );
        notify_tool_outcome(session, &tool_name, &arguments, &value);
        notify_tool_progress(session);
        session
            .history_mut()
            .push_tool_result(call_id, value, duration_ms);
        persist_last(session, store).await?;
        if let Some(reason) = abort_reason {
            aborted = Some(reason);
            break;
        }
    }
    if let Some(reason) = aborted {
        for result in results_iter {
            let (call_id, _value, _duration_ms, _tool_name, _arguments, _cancelled, _placement) =
                result.ok_or_else(|| {
                    CoreError::ToolFailed(
                        "parallel tool scheduler lost a completed call".to_owned(),
                    )
                })?;
            session.history_mut().push_tool_result(
                call_id,
                ToolCallResult::error(format!("turn cancelled before delivery ({reason:?})")),
                0,
            );
            persist_last(session, store).await?;
        }
        return Ok(ParallelOutcome::Aborted(reason));
    }
    // Der Meter hat Aufrufe abgewiesen: alle Ergebnisse sind gepaart, der Turn
    // endet wie bei einem Treffer des Werkzeug-Budgets.
    if refusal.is_some() {
        return Ok(ParallelOutcome::Aborted(CancelReason::Budget));
    }
    Ok(ParallelOutcome::Executed)
}

/// Holt für jeden Call die Guardrail-Entscheidung ein, bis eine davon nicht
/// `Allow` ist.
///
/// # Beschreibung
/// Die Entscheidung je Call stammt aus [`check_approval`] und ist damit
/// dieselbe Aggregation über **alle** Handler (`Deny` > `AskUser` > `Allow`)
/// wie im sequenziellen Pfad — ein später registriertes `Deny` kann auch hier
/// nicht von einem früheren `AskUser` verdeckt werden.
///
/// Jede eingeholte Entscheidung — auch die abschlägige — wird in `prepared`
/// hinterlegt, damit der sequenzielle Fallback sie verbrauchen kann statt
/// denselben Handler erneut zu fragen. Ohne registrierten `ApprovalHandler`
/// liefert [`check_approval`] `Allow`, ohne dass irgendetwas aufgerufen wird
/// — außer der DoD-Taint-Fallback greift (siehe Abschnitt DoD-Taint an
/// [`check_approval`]); sonst ist die Vorprüfung dann ein reiner Durchlauf.
///
/// # Arguments
/// - `calls` (`&[ToolCall]`): die Calls der Modellantwort in Originalreihenfolge.
/// - `prepared` (`&mut PreparedApprovals`): wird überschrieben.
///
/// # Returns
/// `true`, wenn **alle** Calls `Allow` bekommen haben.
async fn preflight_approvals(
    session: &AgentSession,
    calls: &[ToolCall],
    prepared: &mut PreparedApprovals,
) -> bool {
    prepared.decisions = Vec::with_capacity(calls.len());
    // PL-100 Phase 4 (B22): ein `dod.*`-Aufruf taintet erst nach seiner
    // Abfertigung. Mischt die Antwort ihn mit einem taint-pflichtigen
    // Aufruf, läuft sie sequenziell (keine Entscheidung vorab), damit der
    // spätere Aufruf den Taint sieht.
    if crate::dod_taint::batch_mixes_dod_and_gated(calls) {
        return false;
    }
    for call in calls {
        let decision = check_approval(session, call).await;
        let allowed = matches!(decision, ApprovalDecision::Allow);
        prepared.decisions.push(Some(decision));
        if !allowed {
            return false;
        }
    }
    true
}


// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

