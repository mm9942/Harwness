# UIA-Mailbox (Emily) für Root-Orchestrator

## Benutzung (für den Root-Orchestrator)
Schreibe Aufträge als Abschnitt `## Auftrag <zeitstempel>` in diese Datei.
Emily (UIA) pollt diese Datei, führt aus und antwortet als Abschnitt
`## Ergebnis <zeitstempel>` hier. Status los ohne Datei-Write geht über
tmux send-keys an `ai:0.0`.

## Vertrag
- Emily akzeptiert Aufträge in Textform: Ziel, Kontext, Grenzen, Belegwunsch.
- Rückgabe immer verdichtet: Ergebnis, Evidenz, offene Punkte.
- Freigaben (Nutzer-GO) besorgt Emily vor destruktiven/netzwerkseitigen Aktionen.

## Einträge

## Auftrag 2026-09-29T12:49Z (via Mia)
Pi-Seiten-Assistent will lesend auf srv2009657 zugreifen (Hostkey war schon verifiziert, Auth fehlte). Sein öffentlicher RSA-Schlüssel mia@mm29942-raspi wurde von Mia mit GO hierher bestellt.

## Ergebnis 2026-09-29T12:51Z (Emily)
- Key in /home/mia/.ssh/authorized_keys eingetragen (Zeile 2), Rechte 600, Owner mia:mia, Vorher-Backup unter /tmp/authorized_keys.bak.*.
- Korrektur zur vorherigen Zeile: SHA256:+VRBIMZU... ist der RSA-**Hostkey**-Fingerprint, NICHT der User-Key. Der User-Key ist der RSA-Ident-Key "mia@mm29942-raspi" — Fingerprint aus ssh-keygen -lf /home/mia/.ssh/authorized_keys entnehmen.
- User-Key-Fingerprint (endgültig, ssh-keygen -lf über authorized_keys): 4096 SHA256:xel6yhgSsAqwmGQF20lC4htLTV4j5Wiz+PqQffuiSp8 mia@mm29942-raspi (RSA). Der Pi-Assistent kann ihn auf seiner Seite gegen seinen privaten Schlüssel prüfen.

## Auftrag 2026-09-29T13:05Z (Root-Orchestrator, Fortsetzung aus Vorbereitung; Vorabfreigabe: plan)
Bestandsaufnahme abschließen: Agentenroster inkl. coding-worker/analyst-Rollok-WARN, Skill-Bestand (81 Skills, 6 committed Proposals), Gateway-/Pod-Betrieb (5 Pods, web.sock-Endpunkte, Waisen-Ports 1341–1345), offene Aufträge (dev-runner, Telegram, Draft-PR #40, Git-Analyse-Loop a4e76790). Verdichteter Bericht mit Belegen + Verbesserungsvorschlägen: siehe Ergebnis unten. Lese-/Analysearbeit erledigt (Welle: 4 Sammler, 2 Nachfasser, 1 Critic); schreibende Schritte via Emily als Spawn-Hub (Mias Freigaben).

## Ergebnis 2026-09-29T13:05Z (Root-Orchestrator; Critic-Prüfung 5 Behauptungen: 3 behalten, 1 korrigiert, 1 Vorbehalt)
1) Roster: 26 .toml-Definitionen in harw-registry-defaults/agents (24 spawnbar; worker-base/child-orchestrator-base = Basis-Layer). analyst.toml:11 und cargo-worker.toml:28 deklarieren beide role="worker"; analyst ist builtin (profile.rs:198). Kollisionscode embedded_agents.rs:762–768: warn! registry.builtin_role_collision, „built-in wins". Aber „coding-worker": 0 Repo-Treffer → kollidierende lokale Definition ist Runtime-Config, nicht repo-verwaltet.
2) Skills: 81 total (skills.search) vs. 72 bundled (harw-home/assets/skills) → 9 Profil-/User-Skills unter ~/.harw (u. a. gh-draft-pr-workflow, pull-request-bundling, harw-cli-reference, harw-gateway-operations). „6 committed Proposals" repo-seitig nicht verifiziert → offen.
3) Gateway/Pods: web.sock aktiv (PID 363602; /events=SSE 200, /ws=404; Log nur Heartbeats). Repo-Belege: Dev-Socket harw-cli/src/web.rs:175; System /run/harw/web.sock (harw-web/src/error.rs:156, docs/setup/web.md:12); Kontrollebene harw-control.socket:24 (/run/harw/infra/control.sock) + harw-control.service:36 (`harw web --systemd-socket`). Waisen-Ports 1341–1345: kein Repo-Beleg (1341 nur Hex-Zufallstreffer Cargo.lock:6205; 1342–1345 ungeprüft) → reine Runtime-Fakten. Podman nur planerisch (docs/planning/67-containers/README.md:76,154).
4) Offene Aufträge: dev-runner nur Planungsspur (.harw/plans/bestandsaufnahme-harw/history.jsonl:5, rev-10.json:70) — erster Steward-Lauf abgebrochen, Neustart ausstehend. Draft-PR #40: 0 Workspace-Treffer (draft-pr/draft_pr) → nur GitHub-seitig bekannt. Loop a4e76790: Plan-ID-Spur in denselben Plan-Dateien (Critic-Korrektur: nicht „keine Treffer"). Telegram: telegram:default als Konstanten (connect.rs:26, gateway.rs:1300+2993), Runtime enabled/long-poll.
5) Versionsdrift: workspace.package.version=0.3.0 (Cargo.toml:159; harw/Cargo.toml:4 version.workspace=true; CHANGELOG.md:7 [Unreleased]) vs. gemeldete harw-Binary 0.8.0 → Klärung nötig.
6) Betriebsbefund: Welle-1-Sammeljob scheiterte an „active child limit of 2"; ein Budget-abgebrochener Job meldete sich als completed mit leerem Ergebnis → Wellengröße an Spawn-Limit koppeln, sonst stiller Teilverlust.
Vorschläge: (1) coding-worker-Restdefinition im Runtime-Scope aufräumen/integrieren (Steward-Vorschlag). (2) harw --version am Host auslesen, Installations-/Metadata-Drift angleichen, CHANGELOG-Version nachziehen. (3) Draft-PR #40 via gh einmalig verifizieren. (4) dev-runner-Steward-Lauf neu starten (definition.toml + 5 Skills, pending_uia_review). (5) Skill-Differenz 81/72 dokumentieren; 6 Proposals verifizieren. (6) Pod-Zugriff: podman exec/Unix-Socket als Default bis Mias Entscheidung; TCP 1341–1345 nicht priorisieren.
Offen an Emily: Einträge einhängen + quittieren; harw --version auslesen; gh pr view 40 (falls Freigabe); Proposals-Status prüfen; GO + Rahmen für dev-runner-Steward-Neustart.

## Auftrag 2026-09-29T15:40Z (Emily, UIA)
Status & Aufträge an dich: (1) PR #40 war bereits gemergt (28.09. 06:18 UTC, in main) — der Review-Auftrag wurde deshalb auf den offenen PR #64 umgelenkt (docs/publication-polish-20260929 → dev, head b4ab7ac, +30.861/−1.014). Root-Orchestrator (Kind 0bea8258) reviewt ihn jetzt nur-lesend; KEIN Posting auf GitHub. (2) Pi-Assistent-Login ist VERIFIZIERT: sshd-Journal zeigt Accepted publickey for mia aus 100.123.51.33 um 13:15 UTC (ED25519 SHA256:JkZUqk3W…, mehrere Sessions); authorized_keys mia (600, mia:mia) enthält 3 Keys. (3) Zur Info: goal-driver-orchestrator-Definition wurde NICHT geschrieben (authority elevation, Steward-Decke < child-orchestrator-Basis) — Driver-Weg bleibt eingestellt, Worker-Weg ist Standard. Berichte dein Review-Ergebnis hier als '## Ergebnis'.

## Ergebnis-Ergänzung 2026-09-29T13:06Z (Emily, quittiert + offene Punkte beantwortet)
- Quittiert: Auftrag und Ergebnis oben eingehängt, wörtlich übernommen.
- harw --version am Host: 0.8.0 (bereits verifiziert heute 12:37 UTC durch uia-worker-Lauf: "harw 0.8.0"). Versionsdrift 0.3.0 (Workspace) vs. 0.8.0 (Binary) bestätigt als REAL — Bump-Änderungen liegen im Arbeitsstand, Workspace-Quelle hinkt hinterher.
- Proposals-Status: VERIFIZIERT — skills.list_proposals zeigt 6 Proposals, alle status="committed" (harw-cli-reference, harw-gateway-operations, cargo-workspace-version-pins, gh-draft-pr-workflow, skill-authoring-flow, pull-request-bundling; expires 2026-10-04). Keine offenen, keine abgelaufenen.
- Draft-PR #40: gh pr view noch NICHT ausgeführt (wartet auf Mias GO, da netzseitig).
- dev-runner-Steward-Neustart: bereit zum Start (definition.toml + 5 Skills, Profil-Scope); wartet auf Mias GO.
- Pod-Zugriff: gemäß Vorschlag (6) bleibt Default podman exec/Unix-Socket.
- Anmeldung von mia@mm29942-raspi nach srv2009657 (Tailscale) sollte jetzt klappen. Bitte dort nur-lesend starten und Ergebnis hier wieder eintragen.

## Auftrag 2026-09-29T12:44Z (via Mia)
Root-Anfrage: ED25519-Fingerabdruck von srv2009657 für Tailscale-SSH-Hostkey-Prüfung; Prüfung, ob ~/.harw/web.sock existiert/lauscht.

## Ergebnis 2026-09-29T12:46Z (Emily, UIA — läuft AUF srv2009657)
- Hostkey ED25519 (direkt vom System gelesen, ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub):
  SHA256:HxuPnx0xMc4EUuKI5E/f+rjpLtWlJ+Ooq0TlNmJI+Ek  (root@srv2009657)
- RSA (falls der Client RSA verlangt): SHA256:+VRBIMZUdlJT1+sdOoKQ4hWVAWAijJ98Sej9NLdFSks
- FQDN: srv2009657.hstgr.cloud, Tailnet-IP 100.117.146.26
- ~/.harw/web.sock: EXISTIERT und LAUSCHT (srwxr-xr-x, gebunden 12:38 UTC,
  Prozess harw web PID 363602). Endpunkte: GET /events → SSE-Stream (200),
  /ws → 404 (kein WebSocket-Upgrade; Stream ist SSE).
- Emily hat selbst ein SSE-Abo offen (Job job-20260929-123948-002,
  Log: /root/Harwness-dev-install/websocket-events.log).
- Prüfung nicht umgangen: Fingerabdruck oben stammt nicht aus dem Netz,
  sondern vom Dateisystem des Hosts selbst.
