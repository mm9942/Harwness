# Vermittelte Prozessausführung und Sandbox-Module

## Ziel

`/bin/sh`, Cargo und tmux sind keine normalen Modellwerkzeuge. Sie werden nur
durch dedizierte Ausführungsworker benutzt, deren Prozessrecht der Parent
fallweise und mit einem begrenzten Auftrag ausstellt. Ein Planer,
Explorer, UIA-Agent oder gewöhnlicher Coding-Worker erhält niemals direkt
`shell.exec`.

Die Trennung ist absichtlich zweistufig:

1. Die **Delegationscapability** bestimmt, ob ein Parent einen bestimmten
   Ausführungsworker anfordern darf.
2. Ein **ExecutionPermit** bestimmt, welchen einzelnen Prozessauftrag dieses
   Kind ausführen darf.

Eine Rolle allein und eine Parent-Behauptung im Modelltext reichen niemals aus.

## Worker-Klassen

| Definition | Umgebung | Erlaubter Zweck | Direkter Hostzugriff |
| --- | --- | --- | --- |
| `sandbox-shell-worker` | strikte Bubblewrap-Sandbox | begrenzte Shell-Aufträge im Projekt | nie |
| `cargo-worker` | Bubblewrap mit ausdrücklich aktiviertem Toolchain-Modul | Cargo/Rustfmt/Rustc innerhalb des Projekts | nie |
| `tmux-inspector-worker` | Bubblewrap mit einem einzelnen, validierten tmux-User-Socket | `list-sessions`, `capture-pane`, ausdrücklich zugelassene tmux-Abfragen | nie |
| `host-process-worker` | separat, lokal und nur nach bestätigtem Host-Prozess-Modus | unvermeidbare lokale Host-Prozesse | nur für den genehmigten Auftrag |

Der `host-process-worker` ist kein allgemeiner Fluchtweg aus der Sandbox. Er
wird weder an Remote-/Gateway-/MCP-Einstiegen noch durch Child-Worker
registriert. Seine Capability kann ausschließlich die lokale UIA nach einer
klaren Nutzerbestätigung an den Root weiterreichen.

## ExecutionPermit

Vor einer Prozessausführung prüft die Runtime einen nicht vom Modell
konstruierbaren Permit:

```text
ExecutionPermit {
  parent_session,
  worker_definition,
  command_digest,
  sandbox_profile,
  approved_modules,
  workspace_and_scope,
  expires_at,
  remaining_uses = 1,
  approval_proof,
}
```

Der Permit wird an die konkrete kanonische Befehlsanfrage gebunden. Ein
Ausführungsworker darf weder einen zweiten noch einen abgewandelten Befehl
unter demselben Permit starten. Ablauf, Wiederverwendung, andere
Worker-Definition, anderes Workspace oder breiteres Sandboxprofil werden
fail-closed abgewiesen.

Ein Parent kann den Permit nur erzeugen, wenn seine eigene Sandbox und
Authority alle angefragten Rechte bereits enthalten. Der Child-Sandboxplan
bleibt ein echter Kind-Schnitt des Parent-Plans.

## Cargo-Modul

Das Cargo-Modul bindet nur die validierte Toolchain sowie die erforderlichen
Cargo-/Rustup-Verzeichnisse unter festen Sandbox-Zielen ein. Es erweitert weder
Home noch `/tmp` noch Netzwerk pauschal. Fetch benötigt zusätzlich den bereits
gesondert geprüften Proxy-Netzmodus und einen begrenzten Network-Scope.

## tmux-Modul

Das tmux-Modul akzeptiert ausschließlich einen beim lokalen UIA-Approval
validierten Socket des aktuellen Benutzers. Es bindet nicht ganz `/tmp` und
nicht `$HOME`. Der Permit enthält Socket-Identität, erlaubte tmux-Operationen
und eine kurze Laufzeit. Schreibende tmux-Aktionen sind ein eigener,
zustimmungspflichtiger Modulmodus; der Standard ist Inspektion.

## Semantisch angefragter Host-Modus

> **Ist-Stand (2026-09)**: Es gibt inzwischen einen neuen Ausführungsworker
> `uia-shell-worker` (Rolle `uia-worker`, `shell.exec` plus read-only
> `fs.*`, immer `SandboxProfile::Host`, Permit weiterhin erforderlich) sowie
> `InteractionMode::Shell` (`--mode shell`, `/mode shell`). Dieser
> Interaktionsmodus **gewährt keine Berechtigung** — er wählt in der lokalen
> Permit-Ansicht nur die Sitzungsphasen-Variante (Variante 2 unten) vor. Die
> untenstehende Aussage, der Host-Modus sei „kein Slash-Befehl", gilt damit
> mit einer Einschränkung fort: der **Modus** ist über `/mode shell`
> auswählbar, die **Berechtigung** selbst bleibt weiterhin ausschließlich an
> die unveränderbare lokale Bestätigungsansicht gebunden, nicht an den
> Slash-Befehl.

Der Host-Modus ist **kein** CLI-Flag und keine Startoption. Ein Prozess kann
ihn daher weder beim Programmstart noch durch eine wiederholbare
Kommandozeile voreinstellen.

> **Nutzerentscheidung (2026-09-21)**: Seither existiert ein direktes
> Modell-Tool, `sandbox-lease` (`harw-ops/src/sandbox_lease.rs`,
> `model_tool` ohne Zusatz-Approval — der Dialog *ist* die Freigabe;
> Aktionen `request`/`status`/`revoke`, Argument `reason`). Ein `request`
> löst einen `HostPermitPrompt` an die lokale UI aus (`worker_definition =
> "sandbox-lease"`, `command = reason`, Vorauswahl `SessionLease`) und
> wartet bis zu 300 s auf eine Entscheidung:
>
> - **`SessionLease`** — `mark_global_approval` (TTL-befristet): ab
>   Bestätigung laufen alle `shell.exec`-Aufrufe dieser harw-Sitzung **und
>   aller ihrer Kind-Agenten** auf dem Host — ohne bwrap, mit der von harw
>   geerbten Nutzerumgebung (inklusive PATH, HOME,
>   `CARGO_HOME`/`RUSTUP_HOME`), `cwd` = Workspace-Wurzel, weiterhin unter
>   den bestehenden `prlimit`-Limits; die Tool-Ausgabe trägt zur
>   Unterscheidung `"executed_on": "host"`. Der Slash-Befehl
>   `/sandbox-lease revoke` (`busy = "immediate"`) beendet die Freigabe
>   sofort — prozessweit, siehe „Nachtrag (2026-09-21) — prozessweite
>   Freigabe" unten —, danach läuft der nächste Aufruf wieder in der
>   strikten Sandbox.
> - **`SingleExecution`** — `mark_global_single_use`: nur der unmittelbar
>   nächste `shell.exec`-Aufruf **einer beliebigen Session dieses
>   Prozesses** läuft auf dem Host, danach gilt automatisch wieder das
>   strikte Standardprofil.
> - Ablehnung oder Timeout liefern dem Modell einen Fehlertext statt einer
>   Freigabe; es entsteht keine Berechtigung.
>
> Damit gilt die Aussage „auch ein Modell besitzt keine Operation, die den
> Modus unmittelbar aktiviert" nur noch für jeden anderen Weg (CLI-Flag,
> Startoption, ein `/mode`-artiger, selbst aktivierender Slash-Befehl);
> `sandbox-lease` ist der eine bewusst geschaffene direkte Weg. Das Tool
> aktiviert dabei selbst nichts — es sendet nur die strukturierte Anfrage an
> dieselbe unveränderbare lokale Bestätigungsansicht, die auch die indirekte
> Klassifikation weiter unten auslöst; die Freigabe bleibt ausschließlich
> ein bewusstes UI-`Ja` (siehe „Nutzerzustimmung" unten). `/sandbox-lease`
> selbst (Command `status`/`revoke`) aktiviert ebenfalls nichts — es liest
> nur den Status oder beendet eine bestehende Freigabe.
>
> Kind-Registries (`build_registry`, `harw-runtime/src/children.rs`)
> bekommen dieselbe Permit-Verdrahtung jetzt auch als Kinder, nicht nur an
> der Root — `uia-shell-worker` und `host-process-worker` können also selbst
> unter einem aktiven Lease auf dem Host ausführen. Die TUI pollt
> `host_permit_prompts` jetzt auch während eines laufenden Turns
> (`drive_turn_animated`s `select!`), der Dialog erscheint also nicht mehr
> ausschließlich zwischen Turns.
>
> **Nachtrag (2026-09-21) — prozessweite Freigabe statt nur Host-Profil:**
> Ein realer Lauf zeigte zwei Lücken. Erstens hängte
> `harw-registry-defaults/src/profile.rs::build_shell_provider` Ledger,
> Sitzungs-Registry und Fragekanal-Sender nur an einen `ShellToolProvider`
> mit `sandbox_profile.is_host()` — die Root-Session läuft aber mit
> `SandboxProfile::Strict`, sodass ihr `ShellExecutor.host_permit_registry`
> immer `None` blieb und `determine_effective_host`
> (`harw-tool-shell/src/exec.rs`) für sie nie `true` liefern konnte, egal
> welche Freigabe erteilt wurde. Behoben: `build_shell_provider` hängt die
> Verdrahtung jetzt an **jeden** gebauten `ShellToolProvider`, unabhängig vom
> `sandbox_profile` — für Strict/Cargo/Tmux bleibt die Sandbox trotzdem die
> Grenze, weil `determine_effective_host` für ein nicht-Host-Profil
> weiterhin ausschließlich die Registry-Freigabe prüft, nie den Ledger
> selbst. Zweitens galt eine Freigabe bis dahin nur für exakt die
> Session-ID, die sie beantragt hatte — `shell.exec`-Aufrufe aus einer
> Kind-Session (anderer Session-ID, z. B. `uia-shell-worker`) sahen sie
> nicht. Nutzerwunsch: die Freigabe soll für den **ganzen harw-Prozess**
> gelten (Root-Session **und** alle Kind-Agenten), bis TTL-Ablauf oder
> `/sandbox-lease revoke`. Dazu trägt
> [`harw_sandbox::HostPermitSessionRegistry`] jetzt zusätzlich einen
> *globalen* Freigabezustand (`mark_global_approval`/
> `global_approval_remaining`/`mark_global_single_use`/
> `has_global_single_use`/`revoke_global_approval`), den `is_session_approved`,
> `take_single_use`, `has_single_use` und `session_approval_remaining`
> zusätzlich zur sitzungseigenen Freigabe berücksichtigen (globale
> Einmalfreigabe wird atomar zuerst nach der sitzungseigenen verbraucht).
> `/sandbox-lease request` setzt seither ausschließlich noch die globale
> Freigabe (nicht mehr `mark_session_approved`/`mark_single_use`), `revoke`
> entfernt beide Zustände.

Der Benutzer darf den Wunsch natürlichsprachlich und indirekt äußern, etwa
„zeig mir bitte meine laufende tmux-Session“ oder „das muss auf meinem echten
System laufen“. Die UIA beziehungsweise der Root-Orchestrator darf diese
Äußerung als **Host-Modus-Kandidaten** klassifizieren. Diese Klassifikation hat
aber ausschließlich die Wirkung, eine strukturierte Anfrage an die lokale UI
zu senden; sie ist keine Berechtigung und schaltet noch nichts um.

Die lokale UI bietet bei jeder solchen Anfrage zwei auswählbare
Freigabevarianten an. Der Agent darf eine passende Variante **vorschlagen**,
aber weder auswählen noch aktivieren. Bei indirekter oder mehrdeutiger
Nutzerabsicht darf er ausschließlich Variante 1 anfragen. Variante 2 darf er
nur vorschlagen, wenn die Nutzeräußerung die längere Host-Arbeitsphase
unmissverständlich verlangt; sie ist niemals seine erste oder automatische
Empfehlung:

1. **Einmalig für diesen Auftrag.** Ein `ExecutionPermit` ist an einen
   kanonischen Auftrag beziehungsweise Befehls-Hash gebunden und nach einer
   Nutzung verbraucht.
2. **Begrenzte Host-Arbeitsphase.** Ein sitzungsgebundener
   `HostModeLease` erlaubt mehrere einzeln durch Approval und Scope geprüfte
   `host-process-worker`-Aufträge bis zum Ende der lokalen Sitzung oder bis der
   Benutzer die Isolation ausdrücklich wieder aktiviert. Der Lease ist nicht
   auf andere Sessions, Parents, Worker, Workspaces oder Remote-Einstiege
   übertragbar.

Auch Variante 2 ist kein globales „Sandbox aus“: Sie erlaubt nur die schon
bestätigte Klasse lokaler Host-Ausführungsworker. Dateisystem-, Netz-,
Secrets- und Prozessgrenzen werden weiterhin pro Auftrag geprüft; ein
HostModeLease kann keinen Zugriff über die Rechte des lokalen Benutzers hinaus
und keine neue Delegationscapability schaffen. Die UI zeigt während der Phase
permanent und unübersehbar `HOST-MODUS AKTIV` sowie die verbleibende Scope- und
Sitzungsbindung.

Nur ein bewusstes UI-`Ja` erzeugt einen einmaligen,
sitzungsgebundenen Host-ExecutionPermit oder HostModeLease. Abbruch, Timeout,
Disconnect, Auftragsende (Variante 1) und Sitzungsende verwerfen die Anfrage
beziehungsweise die Freigabe. Der Benutzer kann Variante 2 jederzeit über die
UI ausdrücklich beenden; danach gilt sofort wieder das strikte
Standard-Sandboxprofil.

Damit gilt die gewünschte Sicherheitskette:

```text
natürliche Nutzerabsicht
→ Agent erkennt Host-Bedarf und schlägt eine Freigabevariante vor
→ unveränderbare lokale Bestätigungsansicht
→ Benutzer wählt einmalig oder Host-Arbeitsphase und bestätigt explizit
→ begrenzter Permit beziehungsweise Lease
→ spezialisierter Host-Worker
```

Der Agent darf also Unsicherheit offenlegen und um die Bestätigung bitten, kann
aber eine Fehlklassifikation niemals selbst in Host-Ausführung verwandeln.

## Nutzerzustimmung

Das Aktivieren eines Moduls zeigt vor der Freigabe mindestens:

- den Worker-Typ;
- die effektive Sandbox (strikt, Cargo oder tmux-Socket);
- die genaue Ressource, etwa Toolchain-Pfade oder Socket;
- die erlaubte Operation und Laufzeit;
- ob ein Host-Prozessmodus verlangt wird.

Jede Freigabe wird als Sitzungsereignis persistiert. Sie ist nicht auf andere
Sitzungen, Geschwister oder Remote-Einstiege übertragbar und kann von Kindern
niemals erweitert werden.

## Umsetzung

Die modulare Sandbox ist wie folgt umgesetzt:

### SandboxProfile (harw-sandbox/src/profile.rs)

`SandboxProfile` ist ein Enum mit vier Varianten:

- `Strict` — hermetische Bubblewrap-Sandbox, Standard.
- `Cargo(CargoSandboxProfile)` — isolierte Sandbox mit Toolchain.
- `Tmux(TmuxSandboxProfile)` — isolierte Sandbox mit einem Socket.
- `Host` — lokale Host-Ausführung, erfordert ProcessPermit.

Das Profil ist ein vertrauenswürdiger Runtime-Input: es wird beim Aufbau der
Runtime aus Konfiguration und UI-Freigaben gebildet, nie aus einem Tool-Aufruf.

### TmuxSandboxProfile (harw-sandbox/src/tmux.rs)

Validiert einen einzelnen Socket-Pfad (absolut, normal, existent, Unix-Socket).
Bindet nur diesen Socket unter `/run/harw/tmux.sock` in die Sandbox. `Inspect`
bindet read-only, `Write` bindet read-write.

### BwrapLauncher (harw-sandbox/src/bwrap.rs)

- `with_profile(&SandboxProfile)` setzt alle Module in einem Aufruf.
- `with_cargo_profile()` und `with_tmux_profile()` bleiben für einzelnen Zugriff.
- `plan()` bindet Cargo-Toolchain und/oder tmux-Socket anhand des Profils.
- **Nutzerwunsch (2026-09-21):** `plan()` setzt für **alle** Profile
  `--unshare-user` sowie `--uid`/`--gid` auf die effektiven IDs des
  harw-Prozesses (aus `metadata("/proc/self")`, injizierbar für Tests) und
  bindet `/etc/passwd`, `/etc/group` sowie `/etc/nsswitch.conf` read-only per
  `--ro-bind-try`; `USER`/`LOGNAME` werden aus der harw-Umgebung gesetzt.
  Damit funktionieren `whoami` und `id` auch in der strikten Sandbox. Die
  Host-Ausführung (siehe „Semantisch angefragter Host-Modus" oben) läuft
  ohnehin bereits als Nutzerprozess und ist davon nicht betroffen.
- Neuer Builder `with_host_path(path: &str)`: bindet für jedes im
  übergebenen PATH existierende Verzeichnis, das nicht bereits unter
  `/usr /bin /lib /lib64` oder dem Workspace liegt, `--ro-bind-try dir dir`
  (ausgenommen `/` und exakt `$HOME`) und setzt `--setenv PATH <host PATH>`.
  Enthält der PATH `~/.cargo/bin`, wird zusätzlich `$RUSTUP_HOME` (Default
  `~/.rustup`) und `$CARGO_HOME` (Default `~/.cargo`) per `--ro-bind-try`
  gebunden und `RUSTUP_HOME`/`CARGO_HOME` gesetzt, damit die
  rustup-Proxy-Binaries funktionieren. Ohne Aufruf bleibt das Verhalten
  unverändert (hermetischer Minimal-PATH). Genutzt vom `!`-Befehl der TUI
  (Teil C, siehe ShellExecutor/ShellToolProvider unten) — die normale
  Modell-`shell.exec` in der Projekt-Sandbox ruft `with_host_path` nicht auf
  und bleibt hermetisch.

### ShellExecutor/ShellToolProvider (harw-tool-shell/src/exec.rs)

- `sandbox_profile: SandboxProfile` und `permit_ledger: Option<Arc<ProcessPermitLedger>>`
  als Felder von ShellExecutor und ShellToolProvider.
- `with_sandbox_profile()` und `with_permit_ledger()` Builder.
- Host-Profil ohne Ledger → fail-closed.
- Strict/Cargo/Tmux ohne Ledger → normal (Sandbox ist die Grenze).
- **Nutzerentscheidung (2026-09-21):** `run_command` bildet
  `effective_host = sandbox_profile.is_host() ||
  registry.is_session_approved(session_id) ||
  registry.take_single_use(session_id)`. Ist `effective_host` wahr, startet
  der Aufruf nach `authorize_host_command` **ohne bwrap**:
  `tokio::process::Command::new("/bin/sh").args(["-c", cmd])`, `cwd` =
  Workspace-Wurzel, Umgebung von harw vollständig geerbt (also der
  zsh-Kontext des Nutzers inklusive PATH/HOME/CARGO_HOME), weiterhin unter
  den bestehenden `prlimit`-Limits. Timeout, Cancel und
  `terminate()`/Output-Kappung bleiben dieselben Pfade wie im
  Sandbox-Fall. Modell-`shell.exec` **ohne** aktive Sitzungs- oder
  Einzelfreigabe bleibt weiterhin hermetisch in bwrap mit Minimal-PATH —
  `effective_host` wird nur durch `SandboxProfile::Host`, einen aktiven
  `sandbox-lease` oder eine verbrauchte Einzelfreigabe wahr, nie durch den
  Tool-Aufruf selbst. Diese Formel selbst ist unverändert; `is_session_approved`
  und `take_single_use` fragen seit dem Nachtrag unten intern zusätzlich
  einen *prozessweiten* Freigabezustand ab (`HostPermitSessionRegistry`), den
  `session_id` gar nicht selbst gesetzt haben muss — `ShellExecutor` kennt
  diesen Unterschied nicht, er fragt nur "ist diese `session_id` erlaubt".
  Ob er die Frage überhaupt stellen kann, hängt an
  `self.host_permit_registry.is_some()` — siehe „Nachtrag (2026-09-21) —
  prozessweite Freigabe" oben für die zugehörige Verdrahtungslücke in
  `profile.rs`.
- Neuer Builder `with_host_path()` auf `ShellToolProvider`, reicht an
  `BwrapLauncher::with_host_path` durch (siehe oben). Aufrufer ist der
  `!`-Befehl der TUI (`harw-tui/src/command_exec.rs::execute_shell`), der
  den beim Programmstart gelesenen `PATH` der harw-eigenen zsh-Umgebung
  übergibt — unabhängig von einem `sandbox-lease`. `!`-Befehle laufen also
  immer mit dem zsh-PATH des harw-Prozesses (PATH-Verzeichnisse per
  `--ro-bind-try`, `~/.cargo/bin` zusätzlich mit
  `RUSTUP_HOME`/`CARGO_HOME`), bleiben aber in bwrap — sie laufen nicht auf
  dem Host, auch nicht bei aktivem Lease.

### Registry (harw-registry-defaults/src/profile.rs)

- `profile_tool_providers()` erhält `sandbox_profile: &SandboxProfile`.
- `assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile()`
  nimmt das Profil explizit entgegen; die bestehende Funktion delegiert mit
  `Strict` als Default.
- **Nachtrag (2026-09-21):** `build_shell_provider` (in
  `profile_tool_providers`) hängt Ledger, Sitzungs-Registry und
  Fragekanal-Sender aus `HostPermitWiring` jetzt an **jeden** gebauten
  `ShellToolProvider`, sobald `host_permits` übergeben wurde — unabhängig
  davon, ob `sandbox_profile.is_host()` gilt. Vorher geschah das nur für ein
  `SandboxProfile::Host`-Profil; die Root-Session läuft aber mit `Strict`,
  sodass ein `/sandbox-lease` sie nie erreichte
  (`ShellExecutor.host_permit_registry` blieb `None`,
  `determine_effective_host` lieferte immer `false`). Sicherheitsgrenze
  bleibt unverändert: für Strict/Cargo/Tmux prüft `determine_effective_host`
  weiterhin ausschließlich die Registry-Freigabe (nie den Permit-Ledger
  selbst), ohne aktive Freigabe bleibt die Sandbox die Grenze.

### Runtime (harw-runtime/src/assembly.rs)

- `sandbox_profile_from_config()` baut das Profil aus `[sandbox]`-Konfiguration.
- Cargo → `Cargo(CargoSandboxProfile::new(...))`, Tmux →
  `Tmux(TmuxSandboxProfile::new(...))`, sonst `Strict`.
- Bei Validierungsfehler: warn! und Rückfall auf Strict (fail-safe).

### Konfiguration (harw-config/src/harness_config.rs)

```toml
[sandbox.cargo]
mode = "build_offline"
cargo_bin = "/opt/harw/toolchain/bin/cargo"
rustup_home = "/opt/harw/rustup"
cargo_home = "/var/cache/harw/cargo"

[sandbox.tmux]
mode = "inspect"
socket_path = "/tmp/tmux-1000/default"
```

Alle Sektionen mit `deny_unknown_fields`. Fehlt eine Sektion, bleibt die Sandbox
hermetisch.

### Worker-Definitionen (harw-registry-defaults/agents/)

- `sandbox-shell-worker.toml` — Strict-Profil.
- `cargo-worker.toml` — Cargo-Profil.
- `tmux-inspector-worker.toml` — Tmux-Profil.
- `host-process-worker.toml` — Host-Profil, Permit-Pflicht.

Alle extenden `worker-base@1`, haben `shell.exec` als einziges Werkzeug und
`max_depth = 0`.
