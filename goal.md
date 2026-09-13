Gesamturteil

Harwness besitzt viele gut entworfene Leaf-Crates, aber noch keine durchgehend geschlossene Produktruntime. Die größten Probleme liegen nicht in den einzelnen Algorithmen, sondern an den Übergängen zwischen den Crates:

1. Die Benutzeroberfläche bewirbt fünf Tools, aber TUI und One-shot erzeugen keine SpawnContext. Jeder Modell-Tool-Call endet deshalb in MissingToolExecutionContext.


2. Wird nur dieser Fehler behoben, würde shell.exec direkt /bin/sh -c auf dem Host ausführen. Der vorhandene Bubblewrap-Plan wird dort nicht verwendet.


3. Modell- und Provider-Wechsel verändern Session-Metadaten, der reale HTTP-Provider ignoriert diese jedoch.


4. Der robuste harw-session-store existiert, während TUI, One-shot und Telegram ausschließlich InMemoryStateStore verwenden.


5. Der sichere Telegram-Adapter mit Pairing und Admission existiert, doch harw gateway umgeht ihn mit eigenem, ungeschütztem Longpoll-Code.


6. Agent-DSL, Plan-System, Catalog, Managed Spawner, Core Bridge und Job Runtime bilden weitgehend fertige Teilwelten, erreichen aber den Nutzerpfad nicht.



Analysiert wurde der aktuelle Root-Workspace aus Cargo.toml: 36 Crates, 127 workspace-interne Abhängigkeitskanten und 8 Tiefenebenen. Den älteren, zusätzlich eingebetteten harwness/-Baum habe ich nicht als kanonisch behandelt.

Da in der Umgebung weder cargo noch rustc installiert sind, ist dies eine statische Manifest-, Quellcode- und Testquellenanalyse; ich konnte keinen Build oder Testlauf ausführen.

Der tatsächliche Crate-Graph

Ebene	Crates

L0 – tiefste Leaves	harw-agent-dsl, harw-home, harw-macros, harw-memory, harw-plan, harw-types
L1	harw-config, harw-job-runtime, harw-protocol, harw-provider, harw-sandbox, harw-secrets
L2	harw-catalog, harw-install, harw-model-catalog, harw-oauth, harw-session-store, harw-tools
L3	harw-channel, harw-extension-api, harw-knowledge, harw-mcp-server
L4	harw-channel-telegram, harw-core, harw-instructions, harw-operations, harw-project-discovery, harw-tool-fs, harw-tool-shell
L5	harw-core-bridge, harw-ops, harw-provider-http, harw-registry-defaults
L6	harw, harw-tui
L7 – Nutzeroberfläche	harw-cli


Crates ohne workspace-internen Parent sind harw-cli, die öffentliche Fassade harw, sowie die derzeit unkomponierten harw-plan, harw-core-bridge und harw-channel-telegram.


---

L0 – die tiefsten Leaf-Crates

harw-types

Das ist der richtige Gravity Point: 25 andere Crates hängen direkt davon ab. IDs, Rollen, Tool-/Session-/Provider-Typen, Usage, Effort und Impact sind sauber von Runtime-Logik getrennt.

Schwächen:

Viele IDs sind lediglich beliebige, infallible Strings und akzeptieren auch leere Werte.

Es existieren zwei Identitätsstile: UUID-basierte String-IDs und Arc<str>-Wrapper.

Sicherheitsrelevante Impact-Invarianten sind dokumentiert, aber nicht typseitig erzwungen.

Effort endet bei High, während Parent-Crates bereits Xhigh und Max als ausstehenden Backlog erwähnen.


harw-macros

Die Makros für Operation, Tool, FromRawArgs und Fehler reduzieren viel Boilerplate. Besonders das Operation-Makro passt gut zur späteren Surface-Matrix.

Aber:

Das agent-Attribut ist noch ein reines Pass-through-Makro.

Tool-Schema-Inferenz fällt bei unbekannten Rust-Typen pauschal auf JSON-object zurück.

Defaults beeinflussen hauptsächlich required, erscheinen aber nicht als Schema-Default.

Die Operation-Signatur wird nur teilweise validiert; Rückgabetyp und exakter Kontexttyp bleiben offen.

Generierter Code setzt teilweise direkte Dependencies im Consumer voraus.


harw-home

Home-Auflösung, Verzeichnisstruktur und Unix-Modi sind grundsätzlich ordentlich: Home/Profile 0700, Auth-Dateien 0600, idempotentes Scaffolding.

Ein konkreter Profilfehler sitzt in scaffold.rs: Mit HARW_PROFILE=foo wird zwar foo aufgebaut, die erstmalig erzeugte Datei active_profile enthält trotzdem immer default. Sobald die Umgebungsvariable entfällt, startet Harwness in einem anderen Profil.

Außerdem existiert hier eine Layer-Auflösung inklusive Profil, während harw-config::default_config_layers eine andere Layer-Welt erzeugt. Dafür fehlen direkte Tests.

harw-agent-dsl

Die Crate sieht wie ein Compiler aus: TOML → Raw Definition → resolved Definition → executable IR. Parser, Rollen, Authority Ceiling, Family/Org und Provenance sind umfangreich.

Die entscheidenden Grenzen:

Der Resolver behauptet rekursive extends-Auflösung, lädt aber nur genau eine Basisstufe. Siehe resolve.rs.

Versionsangaben in References werden bei der Auswahl nicht wirklich berücksichtigt.

Eine direkte [authority] der Zieldefinition fließt nicht in den Authority-Akkumulator; verarbeitet werden Basis-Authority und Target-Patches.

Basis-Mixins erhalten nicht dieselbe Rollenprüfung wie Target-Mixins.

Der höchste Layer ersetzt die Zieldefinition, statt tiefere Layer als Overlay einzubeziehen.

Beim Lowering werden Spawn Contract, Job Template, Context Policy, Tool Surface, Lifecycle und Return Pipeline leer oder defaulted. Siehe executable.rs.

Die IR-Felder sind öffentlich mutierbar, während snapshot_id nur beim Lowering berechnet wird und danach veralten kann.

Der Core verwendet aus dieser Crate praktisch nur die statische Rollen-/Spawn-Matrix; die executable IR wird nicht zur Laufzeit konsumiert.


Damit ist die DSL heute eher ein gut getestetes Compiler-Subprojekt als die Quelle der tatsächlichen Agentenruntime.

harw-memory

Hier existieren zwei Systeme nebeneinander:

Ein einfaches Memory/FileMemoryStore, das /memory tatsächlich erreicht.

Ein wesentlich reicheres System aus STM, epistemischen Signalen, Summaries, Contradictions, HOT/WARM/COLD und Context Policy.


Die reichere Pipeline ist nicht in den Model Context integriert. Weitere Unstimmigkeiten:

ContextPolicy dupliziert bewusst das Enum aus harw-model-catalog; eine Runtime-Abbildung fehlt.

min_stm_salience wird nicht angewandt.

maintain() markiert Signale als gesehen, bevor der Heartbeat sie zählt; dadurch sieht der Heartbeat gewöhnlich pending_signals = 0.

Der Heartbeat berechnet Promotion und Demotion nur als Report, verändert die Tiers aber nicht.

Ein HOT-Overflow kann bereits beim Lesen einen Fehler auslösen, bevor Maintenance ihn „demoten“ könnte.

Das Signal-Log ist prozessintern gesperrt, aber nicht prozessübergreifend gelockt oder fsynced.


harw-plan

harw-plan ist ein vollständiges, aber vollkommen verwaistes Subsystem: keine andere Workspace-Crate hängt davon ab.

Positiv sind Statusmatrix, Zyklenerkennung, Evidence-Pflicht und Revisionen. Problematisch:

PlanToolConfig.enabled und max_nodes werden nicht verwendet.

AddNode prüft weder eindeutige IDs noch vorhandene Dependencies.

Ein Node kann Ready oder InProgress werden, obwohl Dependencies nicht abgeschlossen sind.

Write-Konflikte vergleichen nur exakte Strings; src und src/lib.rs kollidieren nicht.

Path Admission operiert auf unnormalisierten Strings. DirectoryPrefix("src") akzeptiert daher etwa src/../secret.

Rename enthält nur den Zielpfad; der Quellpfad wird nicht geprüft.

Der FilePlanStore lädt vorhandene Snapshots beim Neustart nicht wieder ein.

Beim Schreiben wird zuerst RAM/Snapshot aktualisiert und erst danach die History angehängt. Schlägt der Append fehl, ist der Zustand trotzdem fortgeschritten.

Es gibt flush, aber kein sync_all plus Parent-Directory-Fsync.


Die Admission-Grenze steht in admission.rs, die Persistenz in file_store.rs.


---

L1 – grundlegende Policies und Runtime-Verträge

harw-config

Die Config-Validierung ist an einigen Stellen erfreulich streng:

MCP nur loopback.

Listener-Pfad und Auth-Principal werden validiert.

Plaintext-Provider-Secrets werden abgewiesen.

Doppelt verwendete Channel-Credentials werden erkannt.


Die Komposition bleibt inkonsistent:

CLI lädt Layer über harw-home, einschließlich aktivem Profil.

Mehrere harw-ops verwenden harw-config::default_config_layers, das das Profil auslässt.

Dadurch können Chat und Slash Commands unterschiedliche Konfigurationen „sehen“.

config.toml ersetzt größere Strukturen komplett, während Catalog-Unterverzeichnisse gemerged werden.

read_dir().flatten() verschluckt I/O-Fehler; fehlende Sortierung macht Duplicate-Winner nondeterministisch.

System- und Skill-Dateien werden einfach mit einem konfigurierbaren String gejoint. Absolute Pfade und ../ können aus dem Agent-/Skill-Verzeichnis ausbrechen; siehe loader.rs.

Unbekannte TOML-Felder werden vielfach akzeptiert, sodass Tippfehler zu stillen Defaults führen.


harw-protocol

Das ist eine schmale DTO-Crate zwischen Core und TUI. Ihre Schwäche ist vor allem fehlende Protokollhärtung:

Keine direkten Tests.

Das jsonrpc-Feld wird beim Deserialisieren übersprungen; ein falscher eingehender Wert wird dadurch nicht validiert.

Die Protokollversion ist optional und wird auf DTO-Ebene nicht erzwungen.

Result<T,E> wird als Rust-/Serde-Enum übertragen, statt als explizit stabiler Wire Contract.

Session- und Turn-Events überlappen semantisch.


harw-job-runtime

Job State Machine, Budget, Lease und Retry sind solide Grundlagen. Der dauerhafte Store prüft später auch Lease-Fencing.

Lücken:

complete() und record_failure() validieren den Ausgangszustand nicht konsequent.

Negative Wall-Durations können dem verbrauchten Budget abgezogen werden.

Null- oder negative Lease-TTLs werden akzeptiert.

Retry-Faktoren wie NaN oder negative Werte sind nicht validiert.

Die erste Backoff-Berechnung wirkt um einen Exponenten verschoben.

Die Crate wird im Gateway-Dream nur als lokales Job-Objekt verwendet, nicht über den dauerhaften JobStore.


harw-provider

Diese Crate enthält Typestate-Registrierung, Failover und Provider-Metadaten, aber nicht den realen Chattransport.

Es gibt zwei Provider-Welten:

harw-provider mit ProviderName, Registry und ProviderInvoker.

harw-provider-http als harw-core::ModelProvider.


Für ProviderInvoker existiert keine Produktionsimplementierung. Die globale Registry wird im CLI-Pfad nicht befüllt; /provider list und /provider switch arbeiten deshalb gegen eine leere oder irrelevante Registry. Sekundärprovider liegen in einer HashMap, wodurch ihre Reihenfolge nicht deterministisch ist.

harw-sandbox

Pfad-Containment, kanonische Workspace-Bindings, Permission-Subset und Child-Reduktion sind sorgfältig gestaltet.

Der entscheidende Unterschied ist jedoch: SandboxSpec ist eine Authority-Beschreibung, keine vollständige Prozessisolation. Der Bubblewrap-Plan wird für MCP-Subprozesse vorbereitet, aber nicht für shell.exec verwendet.

harw-secrets

Kryptographische Envelope-Logik ist vorhanden, insbesondere mit injiziertem ML-KEM-Keypair. Das Produktionssystem fehlt aber:

SecretStore.root wird nicht für dauerhafte Speicherung genutzt.

KEK-Ableitung, Keyring-Zugriff und Rotation liefern NotYetImplemented.

Audit und Checkpoints sind in-memory; Signaturen fehlen.

Store-Operationen schreiben nicht automatisch Audit Events.

Keine Produktionscrate konsumiert diesen Store.

secrets:-Configreferenzen können vom HTTP-Provider nicht aufgelöst werden.



---

L2–L3 – Kataloge, Stores und Integrationsschnittstellen

harw-tools

Eine gute neutrale Boundary aus ToolSpec, ToolCall, ToolOutput, ToolExecutor und ToolExecutionContext. Die per-call übergebene Sandbox ist genau die richtige Autoritätsnaht und require_permission fail-closed.

Kleinere Lücke: ToolName akzeptiert beliebige Strings. Traced Execution ist vorgesehen, aber nicht produktiv zusammengesetzt.

harw-catalog

Immutable Snapshots, Config-Hashes, MCP-Deskriptoren und advisory Suggestions sind sauber. Das Design vermeidet automatische Privilegienausweitung.

In der Runtime fehlt jedoch der Producer/Consumer-Kreislauf: Capability Snapshots und SkillWorkspace erreichen keinen real aufgebauten Child Agent.

harw-model-catalog

Mit Abstand eine der umfangreichsten und bestgetesteten Crates: Modelldeskriptoren, Provider Specs, Provenance, Runtime Profiles, Routing, Credential Detection und models.dev-Enrichment.

Der Core verwendet davon fast nichts:

Kein Runtime Profile steuert Context Budget, Retry, Parallelität oder Delegation.

Der Turn Loop routet nicht über den Catalog.

/model validiert gegen statische Bootstrap-Modelle, nicht zwingend gegen die tatsächlich konfigurierte Providerliste.

resolve() liefert primär Bootstrap-Deskriptoren.

Das separate ContextPolicy-System in Memory bleibt unverbunden.


harw-install

Service-, Doctor-, Update-, Migration- und Uninstall-Bausteine sind vorhanden.

Die Nutzersemantik ist teilweise irreführend:

harw update --check führt keinen Remote-Check aus. Es schreibt die eigene Paketversion als „neueste bekannte Version“ plus aktuellen Zeitstempel; siehe lifecycle.rs.

doctor validiert hauptsächlich Config und zählt Einträge.

health prüft OS, Bubblewrap-Verfügbarkeit, Service Manager und Home-Modi, erkennt aber weder den fehlenden Tool-Kontext noch die unisolierte Shell.

Vorhandene Migrationen werden im CLI-Startpfad nicht automatisch ausgeführt.

Der installierte Service startet harw gateway und übernimmt damit dessen Telegram-Risiken.


harw-oauth

Der Anthropic-PKCE-Paste-Flow ist funktional angelegt, aber die CSRF-Prüfung ist falsch verdrahtet: Bei code#state wird der eingefügte State nicht mit dem erwarteten State verglichen, sondern direkt an den Token Exchange weitergereicht. Siehe auth.rs.

Zusätzlich:

Fallback-Secret-Eingabe läuft über normales stdin und kann sichtbar sein.

Nach erfolgreicher Anmeldung wird ein kompletter Export-Befehl mit Token ausgegeben.

Token-Dateien werden nicht atomar geschrieben; chmod erfolgt erst nach Erstellung.

Nur Anthropic ist implementiert.


harw-session-store

Diese Crate ist deutlich stärker als ihre tatsächliche Verwendung vermuten lässt:

Transcript Append: Advisory Lock + sync_data.

Rewrite: synchronisierte Temp-Datei + atomarer Persist.

Job-, Approval- und Child-Lease-Stores: Revisionen, Locking, Fencing und atomare Zustände.


Der Fehler liegt im Parent: harw-core::StateStore ist ein separates Trait mit Future<()>, kann also nicht einmal Persistenzfehler zurückgeben. Es gibt nur InMemoryStateStore; kein Adapter verbindet den robusten TranscriptStore mit dem Core-StateStore.

harw-extension-api

Gute statische Extension-Seams für Tools, Context, Instructions, Approvals, Observer und Spawner. Das Problem ist die konkrete Registry: Sie enthält nur Tools, Instructions und Project Context; Approval Handler und Spawner bleiben leer.

harw-channel

PairingStore, Admission, Session Keys, Replay Claiming und Adapterverträge sind sinnvoll und dauerhaft implementiert. Einziger Parent ist harw-channel-telegram; der reale CLI-Gateway-Pfad hängt von keiner der beiden Crates ab.

harw-knowledge

Knowledge Store, Index, BM25, Palace, Diary, Dream, Workbench und Kanban sind reichhaltig.

Vor einer Runtime-Komposition müssen zwei Grenzen geschlossen werden:

visible_to_caller schützt nur OperatorOnly. SelfOnly, DescendantTree und ExplicitlyGranted liefern ohne externe Policy immer true; siehe visibility.rs.

Der rekursive Markdown-Index folgt über Path::is_dir() Verzeichnis-Symlinks, ohne Root- oder Cycle-Guard.


Der Gateway nutzt ohnehin nur einfache Pfadfunktionen und schreibt rohe Dream-/Diary-Markdowntexte; Index und Recall gelangen nicht in den Model Context.

harw-mcp-server

Transportseitig ist dies eine der besseren Grenzen: Loopback, Origin-Prüfung, Bearer-Vergleich, Session/Principal-Binding, TTL, Capacity und Bodylimit.

Die produktive Surface besteht aber nur aus:

harw_job_status

harw_job_cancel


Es gibt kein Submit-Tool, und die CLI dokumentiert selbst, dass kein Produktions-DurableJobRunner Jobs erzeugt. Außerdem wird bind_with_supervisor statt der Event-Bus-Variante verwendet. Der Server schützt daher gut einen Jobbestand, der im normalen Produktpfad kaum entstehen kann.


---

L4–L6 – Runtime, Tools und UI-Komposition

harw-core

Der Core enthält eine ernstzunehmende providerneutrale State Machine, Model-/Tool-Zyklus, Usage, Parallel-Safety, Approvals, Handoffs, durable Job Admission und Child Management.

Hier sitzt der wichtigste funktionale Fehler:

TUI/One-shot
  → Registry mit fs.* und shell.exec
  → AgentSession ohne SpawnContext
  → Modell fordert Tool an
  → tool_execution_context()
  → MissingToolExecutionContext
  → Session wird Failed

Die Ablehnung selbst ist korrekt fail-closed. Falsch ist, dass die Composition Root fünf Tools bewirbt, ohne den erforderlichen Kontext zu setzen. Belege: turn_loop.rs, chat.rs und TUI app.rs.

Weitere Core-Befunde:

Alle Model-/Toolfehler setzen die Session terminal auf Failed.

Die TUI versucht bei HTTP 429 dieselbe Session erneut; der zweite Turn wird wegen Failed statt Idle abgewiesen.

Der StateStore kann keine Fehler melden und verschluckt sogar vergiftete Mutexes.

Model Requests tragen model_id und provider_id, aber die konkreten Provider ignorieren sie.

Ohne Approval Handler laufen Schreib-/Shell-Tools nach Behebung des Spawn-Kontexts ohne Approval-Pause.

Die reiche ManagedAgentSpawner-Implementierung wird nirgends produktiv gebaut.

DSL-IR und Model Runtime Profiles werden nicht konsumiert.

Handoffs erkennen transfer_to_*, aber solche Tools werden nicht registriert.

Im durable Child-Resume-Pfad wird der in-memory Child geschlossen, nicht offensichtlich auch die dauerhafte Lease.


harw-instructions und harw-project-discovery

Die Baseline Instructions beschreiben genau fünf Coding-Tools und eine statische Agentenidentität. Config- oder DSL-Agentenanweisungen werden nicht eingebunden.

Project Discovery besitzt vernünftige Anzahl- und Größenlimits. Allerdings wird eine Datei erst vollständig gelesen und danach gekürzt; außerdem kann Byte-Truncation mitten in einem UTF-8-Codepoint enden. Die Discovery wird einmal beim Sessionstart eingefroren.

harw-tool-fs

Die vier FS-Tools verwenden Workspace-Containment und Permission Checks korrekt. Schreiben ist jedoch ein direktes std::fs::write, nicht atomar, und Parent-Verzeichnisse müssen bereits existieren.

harw-tool-shell

Das ist die kritischste latente Sicherheitsgrenze. exec.rs startet direkt:

/bin/sh -c <model-generierter Befehl>
cwd = Sandbox-Workspace
Umgebung/Netzwerk/Host-Dateisystem = geerbt

ExecuteProcess ist die einzige praktische Sperre; Bubblewrap wird nicht eingesetzt. Ein cwd ist keine Dateisystemisolation.

Weitere Punkte:

Der vollständige Modellbefehl wird auf Debug-Level geloggt.

--log-sensitive wird nur in harw-cli gespeichert; die Tool-Crate konsultiert das Flag nicht.

UTF-8-lossy Output wird anschließend per Byteindex geschnitten und kann bei Multibyte-Zeichen panicen.

stdout und stderr erhalten jeweils das volle Limit, nicht ein gemeinsames Gesamtlimit.


Aktuell verhindert der fehlende Spawn-Kontext die Ausführung im Standardpfad. Deshalb muss Shell-Isolation vor oder gleichzeitig mit dem Tool-Kontext-Fix erfolgen.

harw-channel-telegram

Dieser Adapter implementiert genau die Policies, die man im Gateway erwartet: Pairing, Allowlist, Mention Gate, Topic Sessions, Replay Claim, Tenant Resolution und reduzierte Sandbox.

Aber run_ingress() liefert immer IngressUnavailable, und die CLI verwendet diese Crate überhaupt nicht. Siehe telegram.rs.

harw-operations und harw-core-bridge

harw-operations stellt gute Metadaten, Surface-Typen, Permission Tiers und Adapter bereit. Der ModelToolAdapter wird aber nicht in die Default-Registry eingebaut.

Der AgentToolAdapter wurde nach harw-core-bridge verschoben. Diese Bridge hat keinen Parent und wird nicht produktiv konsumiert; zusätzlich liegt in harw-operations noch eine nicht eingebundene alte agent_tool.rs.

harw-ops

Es werden exakt 18 Operationen registriert; siehe harw-ops/src/lib.rs.

Tatsächlich brauchbar sind vor allem:

/help

/status

/quit

/memory

/effort


Eingeschränkt oder irreführend:

/model verändert den Session Controller, nicht das reale Provider-Modell.

/provider arbeitet gegen die unbefüllte globale Provider Registry.


Explizite Platzhalter sind:

/new, /work, /ps, /attach, /stop

/diff, /agent, /skills, /plugins

/permissions, /compact


Die TUI führt lokale Slash Commands außerdem ohne Enforcement des deklarierten PermissionTier aus. Die Metadaten beschreiben Policy, sie setzen sie nicht durch.

harw-provider-http

OpenAI Responses/Chat und Anthropic Messages sind echte HTTP-Provider. Request-/Response-Mapping und Tool-Call-Parsing sind relativ umfangreich getestet.

Architektonische Blockade:

Provider und Modell werden beim Aufbau in self.model eingefroren.

ModelRequest.model_id und .provider_id werden ignoriert.

Die TUI besitzt genau eine Box<dyn ModelProvider>, keinen Router.

/model switch und /provider switch können daher nicht das Backend wechseln.


Siehe OpenAI-Implementierung und Anthropic-Implementierung.

Weitere Risiken:

Kein expliziter Request-Timeout.

Provider-Fehler enthalten den vollständigen Response Body.

keyring: und secrets: werden nicht unterstützt; nur Env-/Dateireferenzen.

Anthropic Effort/Reasoning ist noch nicht verdrahtet.


harw-registry-defaults

Die Registry baut genau:

fs.read

fs.write

fs.list

fs.search

shell.exec

plus Baseline Instructions und Project Context.


Keine Approvals, kein Child Spawner, keine Operation-ModelTools, keine Observer.

Hinzu kommt ein Root-Mismatch: Registry und Prompt verwenden das echte Start-cwd/Projekt, die von der CLI erzeugte TUI-Sandbox zeigt dagegen auf <HARW_HOME>/workspace. Selbst mit repariertem Spawn-Kontext würde das Modell also über Projekt A informiert, während seine Tools in Workspace B arbeiten.

harw

Die Fassade reexportiert SDK-Bausteine und besitzt Compile-orientierte Tests. harw-cli hängt aber nicht von ihr ab. Sie ist eine parallele Public API, nicht die Composition Root des Produkts.

harw-tui

Die UI selbst ist umfangreich: Editor, Streaming, History Cells, Popups, Slash Completion, Setup Wizard und Tool-Events.

Die Runtime-Konstruktion bleibt minimal:

AgentSession ohne Spawn Context.

InMemoryStateStore.

Genau ein fest aufgebauter Provider.

Sandbox nur im Slash-Command-ChatApp, nicht in der AgentSession.

Fallback auf eine leere Registry, wenn Project Discovery scheitert.

429-Retry gegen eine bereits terminal fehlgeschlagene Session.



---

L7 – was der Nutzer tatsächlich erlebt

Nutzerweg	Tatsächliche Wirkung

harw	Startet TUI mit festem Provider oder Echo-Fallback. Reiner Textchat kann funktionieren.
TUI-Tool-Call	Scheitert deterministisch an fehlendem SpawnContext.
harw PROMPT	One-shot-Text funktioniert; Tool-Call hat denselben Kontextfehler.
harw run …	Expliziter lokaler Echo-Pfad; funktioniert, ist aber keine echte Agentenruntime.
/model switch	Session-Metadatum ändert sich, HTTP-Provider bleibt beim Startmodell.
/provider switch	Kein realer Providerrouter; Registry ist produktiv unbefüllt.
Session-Neustart	Verlauf verloren, weil nur InMemoryStateStore verwendet wird.
harw gateway	Eigenständiger Telegram-Longpoll, nicht der sichere Channel-Adapter.
Gateway Dream	Erhält weder letzte Unterhaltung noch Knowledge Recall und kann deshalb „letzte wichtige Dinge“ nur erfinden.
harw serve	Stellt Status/Cancel für dauerhafte Jobs bereit, erzeugt selbst aber keine Jobs.
harw update --check	Stempelt die aktuelle lokale Version als „latest“, ohne Netzwerkprüfung.
harw doctor	Erkennt Configfehler, nicht die gebrochenen End-to-End-Verbindungen.


Besonders kritisch: Telegram Gateway

Der Live-Gateway-Code in gateway.rs:

verwendet kein Pairing;

keine Tenant-/Sender-Allowlist;

kein Mention Gate;

keine Topic-separaten Sessions;

keinen ReplayStore;

keine persistente Session;

keine reduzierte Channel-Sandbox;

prüft HTTP-Statuscodes nicht mit error_for_status;

hält Offset und Sessions nur im RAM.


Jeder Telegram-Nutzer, der den Bot erreichen kann, kann damit potenziell kostenpflichtige Modellaufrufe auslösen. Tools sind dort zwar durch eine leere Extension Registry deaktiviert, aber Admission und Kostenkontrolle fehlen.

Der Dream-Job ist ebenfalls nur teilweise governt:

lokales Job-Objekt statt dauerhafter Store;

keine echte Gesprächs- oder Knowledge-Eingabe;

Tokenverbrauch wird geschätzt und anschließend per .min(max) so gedeckelt, dass eine Überschreitung nie sichtbar wird;

Wall- und Tool-Budget werden nicht real gebucht;

der Report ist „review-gated“, die gleiche Reflexion wird aber automatisch ins Diary geschrieben.


Die eigentliche Architektursituation

Es existieren momentan drei weitgehend parallele Welten:

1. Die intendierte, governte Welt
Agent DSL → Catalog → Capability Snapshot → Managed Spawner → durable Jobs → Channel Admission.


2. Der reale Chatpfad
CLI/TUI → statische Default Registry → Core → fester HTTP-Provider → In-memory History.


3. Die operative Welt
Operations, JobStore, MCP Status/Cancel, Knowledge und Memory – jeweils teilweise umgesetzt, aber ohne gemeinsamen Producer/Session Manager.



Die einzelnen Crates sind häufig stärker als das resultierende Produkt. Der zentrale fehlende Baustein ist eine einzige, autoritative Composition Root, die Sandbox, Session, Providerrouter, Persistenz, Agent IR, Jobs, Channels und Operations aus denselben aufgelösten Snapshots konstruiert.

Empfohlene Reparaturreihenfolge

1. Tool Boundary gemeinsam reparieren

Ein kanonisches Workspace Binding erzeugen.

Denselben SpawnContext an AgentSession und Slash-Command-Kontext geben.

shell.exec bis zur echten Bubblewrap-/Container-Ausführung deaktivieren.

Approval Handler für Write und Execute registrieren.



2. Persistenz zum Core-Vertrag machen

StateStore auf Result umstellen.

Adapter auf TranscriptStore implementieren.

/new, /attach, /compact und Session Resume daran anbinden.



3. Provider und Modell wirklich routen

Einen RoutingModelProvider statt einer fixen Box einführen.

Model Catalog als gemeinsame Quelle verwenden.

/model und /provider erst nach erfolgreichem Backend-Wechsel committen.



4. Telegram auf den vorhandenen Channel-Stack migrieren

HTTP-Ingress in harw-channel-telegram implementieren.

Pairing, Replay, Tenant, Topic und Admission verpflichtend machen.

Offset und Channel Sessions dauerhaft speichern.



5. Jobs vollständig schließen

Einen Produktions-DurableJobRunner als Producer starten.

/work, /ps, /stop, MCP und Event Bus auf denselben JobStore setzen.

Dream aus realen Transcripts/Knowledge speisen und echte Usage buchen.



6. Agentenruntime anschließen

Executable Agent IR vervollständigen.

IR → Catalog Snapshot → ManagedAgentSpawner → AgentToolAdapter verdrahten.

harw-plan entweder in diese Kette integrieren oder vorerst aus dem Produktversprechen entfernen.



7. Grenzen härten

OAuth-State vergleichen.

Config-/Plan-/Knowledge-Pfade normalisieren und Root-Containment erzwingen.

Secrets persistent plus Audit integrieren.

Config-Layer-Auflösung vereinheitlichen.



8. End-to-End-Contract-Tests hinzufügen

TUI-Textturn.

TUI-FS-Tool innerhalb desselben Projektroots.

Shell außerhalb des Roots muss scheitern.

Model-/Provider-Wechsel muss den tatsächlichen HTTP-Request verändern.

Session Resume nach Prozessneustart.

Unpaired Telegram Update muss abgewiesen werden.

Submit → Worker → MCP Status/Cancel → Event Stream.




Die wichtigste Schlussfolgerung: Wir sollten jetzt nicht weitere Leaf-Crates ausbauen. Die nächste Entwicklungswelle sollte gezielt die fünf Produktpfade schließen: Tool-Ausführung, Persistenz, Providerrouting, Telegram-Admission und durable Jobs. Erst danach werden DSL, Plan, Memory und Knowledge für Nutzer tatsächlich wertvoll.
