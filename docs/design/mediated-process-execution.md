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

Der Host-Modus ist **kein** CLI-Flag, keine Startoption und kein Slash-Befehl.
Ein Prozess kann ihn daher weder beim Programmstart noch durch eine
wiederholbare Kommandozeile voreinstellen. Auch ein Modell besitzt keine
Operation, die den Modus unmittelbar aktiviert.

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
