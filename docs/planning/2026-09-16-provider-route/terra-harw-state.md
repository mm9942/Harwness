# Lokaler Harw-Zustand: Ziele, Plan, Memory und offene Arbeit

Stand: 2026-09-16. Ausgewertet wurden ausschließlich `.harw/`, `.remember/`
und der unveränderte Referenzcheckout `.harw/pi-xai-oauth/`. Keine
Credential-Inhalte sind dokumentiert.

## Aktives Ziel und Revisionen

Das einzige Ziel `models-management` ist seit Revision 1 aktiv und auch in
Revision 25 aktiv; `evidence=[]`. Sein Name ist historisch: Die Aussage ging
von Modellverwaltung (rev 1–13) über Provider/OAuth (rev 14) zur modularen
Prozess-Sandbox (rev 20–25). Rev 15 ergänzte Token-Effizienz, rev 16 band Plan
`arbeite` rev 0, rev 17–19 ergänzten Turn-Disziplin, Auto-Compact und
Rate-Limit-Härtung. Quelle: `.harw/goals/default/history.jsonl` und
`.harw/goals/default/rev-{1..25}.json`.

Die aktuelle Zielaussage verlangt Strict-, Cargo-, tmux- und Host-Profile,
Permit-gesteuerte Prozessausführung und spezialisierte Worker. Invarianten:

1. UIA-Worker besitzen keine Schreib-, Shell-, Credential- oder Spawn-Autorität.
2. Angekündigte Änderungen werden im selben Turn umgesetzt und geprüft.
3. Sandbox-Lockerung erfordert lokale, vom Modell nicht formulierbare
   Bestätigung und einen sitzungsgebundenen Permit; Kinder dürfen sie nicht
   erzwingen.

## Plan rev-280: alle Knoten und offener Rest

`models-management-plan/rev-280.json` enthält 111 Knoten: 89 `superseded`,
21 `completed` und einen offenen Knoten, `contract-oauth` mit Status `draft`.
Es gibt keine `ready`- oder `in_progress`-Knoten.

| Status | Knoten |
|---|---|
| completed | `oauth-contracts`; `tmux-profile`, `sandbox-profile-enum`, `shell-permit`, `shell-profile`, `registry-profile`, `runtime-profile`, `config-sandbox`, `worker-definitions`, `tests-integration`; `explore-tmux`, `explore-config`, `explore-profile-enum`, `explore-shell`, `explore-registry`, `explore-runtime`, `explore-workers`, `explore-token-flow`, `explore-resume-export`, `explore-resume-visuals`, `explore-usage-meta` |
| draft / offen | `contract-oauth` |
| superseded | `architecture`, `provider-api-research`, `contracts`, `role-model-fanout`, `models-cli-tui`, `verify`, `uia-worker-role`, `tui-chat-visuals`, `provider-oauth-research`, `xai-oauth-contract`, `xai-oauth-implementation`, `provider-oauth-matrix`, `provider-credentials`, `provider-transports`, `provider-install-verification`, `research-harw-browser`, `research-harw-fsutil`, `research-harw-macros`, `research-harw-types`, `research-harw-browser-thirtyfour`, `research-harw-code-graph`, `research-harw-dod-cap`, `research-harw-dod-warden-proto`, `research-harw-home`, `research-harw-lens-types`, `research-harw-observe`, `research-harw-plan`, `research-harw-protocol`, `research-harw-provider`, `research-harw-research`, `research-harw-sandbox`, `research-harw-context`, `research-harw-dod-signals`, `research-harw-egress`, `research-harw-job-runtime`, `research-harw-lens-chunk`, `research-harw-lens-embed`, `research-harw-lens-rank`, `research-harw-lens-store`, `research-harw-observe-file`, `research-harw-observe-otlp`, `research-harw-observe-prom`, `research-harw-secrets`, `research-harw-agent-dsl`, `research-harw-dod-rules`, `research-harw-lens-index`, `research-harw-session-store`, `research-harw-tools`, `research-harw-channel`, `research-harw-config`, `research-harw-dod-escalate`, `research-harw-lens-query`, `research-harw-mcp-server`, `research-harw-catalog`, `research-harw-channel-telegram`, `research-harw-install`, `research-harw-lens-federation`, `research-harw-model-catalog`, `research-harw-oauth`, `research-harw-channel-telegram-transport`, `research-harw-extension-api`, `research-harw-knowledge`, `research-harw-instructions`, `research-harw-lens-source`, `research-harw-memory`, `research-harw-operations`, `research-harw-project-discovery`, `research-harw-tool-browser`, `research-harw-tool-deps`, `research-harw-tool-fs`, `research-harw-tool-shell`, `research-harw-tool-web`, `research-harw-core`, `research-harw-lens`, `research-harw-web`, `research-harw-core-bridge`, `research-harw-plan-bridge`, `research-harw-provider-http`, `research-harw-tool-lens`, `research-harw-registry-defaults`, `research-harw-ops`, `research-harw-runtime`, `research-harw-tui`, `research-harw-cli`, `analyze-synthesis`, `contracts-test-consistency`, `dod-migration-consistency`, `contracts-gap-audit`, `auto-compact-policy` |

Die `completed`-Markierung ist Store-Status und kein neuer Codebeweis.

## Übernommene Akzeptanzkriterien und Constraints

Die 13 Kriterien aus Rev 25 bleiben belegpflichtig: Luna für Explorer,
Fanout 5; `harw --models` plus Scan und getrennte API/Auth/Netz-Fehler;
beschränkter UIA-Worker und sichtbare Spawns; Auto-Compact mit 70/30-Schwellen;
maximal drei Rate-Limit-Retries; Strict/Cargo/tmux/Host-Profile; nur einzelner
validierter tmux-Socket, nie pauschal `/tmp`/Home; nicht durch Tools
überschreibbares Shell-Profil; und fail-closed ProcessPermitLedger, gebunden an
Befehl, Worker, Session und Umgebung. Exakter Wortlaut:
`.harw/goals/default/rev-25.json:acceptance_criteria`.

Zusätzliche Constraints: Provider-Scans nur mit konfigurierten Credential- und
Netz-Policies, niemals Klartextsecrets; token-effiziente, gezielte Reads und
keine erneuten blockierten Spawn-Versuche.

## Memory-/Remember-Befunde und Inkonsistenzen

Die 29 `facts/pitfall-*.md` sind automatische Fehlermeldungen, keine bestätigten
Produktanforderungen. Wiederkehrend: Statusübergänge müssen
`draft → ready → in_progress → completed` durchlaufen; Coding braucht frische
Exploration; Revisionen müssen monoton steigen; ein Modell darf das aktive Ziel
nicht abschließen/aufgeben; Spawn kann fehlen; Sandbox hatte Cargo-/DNS-Grenzen.

`.remember/logs/memory-2026-09-15.log` und `hook-errors.log` belegen mehrere
fehlgeschlagene Summarizerläufe durch Auth-/Connector-Konflikte und fehlende
Claude-Sessiondateien. `.remember` ist daher unvollständig und keine alleinige
Quelle für offene Aufträge.

Inkonsistenzen: Das aktive Ziel heißt weiterhin `models-management`, handelt
aber von Sandbox. Plan 280 ist überwiegend superseded, während das Goal noch
auf Plan `arbeite` rev 0 zeigt. Daraus folgt keine aktuelle OAuth-Spezifikation.

## Codex-Login: laufender nativer Fix

Die frühere OAuth-Nicht-Unterstützung im Evidenzdokument beschreibt den
**Iststand vor diesem Fix**, nicht eine Nutzervorgabe. Der Parent implementiert:

1. Nur kanonische Codex-CLI-Datei plus exakt `/tokens/access_token` qualifizieren.
2. Endpoint-Korrektur nur bei OpenAI-Standardbase; Custom-URLs bleiben unverändert.
3. Responses-SSE und Model-Discovery nutzen dieselbe effektive Route.
4. Token wird vor jeder Anfrage sicher neu gelesen.
5. Keine Refresh-Token-Rotation und kein Schreiben in `~/.codex/auth.json`.
6. API-Key- und andere Credential-Routen behalten ihre Semantik.

Abnahme: Tests für positive Korrektur, API-Key/falschen Pointer/Custom-URL ohne
Korrektur, SSE und Discovery; Fehler dürfen niemals Token oder Dateiinhalte
ausgeben.

## Fremdcheckout `pi-xai-oauth`: nur Inventar

`BlockedPath/pi-xai-oauth`, Commit `b09a8d0`, Paket 1.5.2: TypeScript/Pi-Plugin
mit OAuth-, Responses-, Streaming-, Credential- und Testmodulen. Seine
Constraints verbieten das Ersetzen, Registrieren, Refreshen oder Streamen des
eingebauten `xai`-Providers. Das ist keine Codex-Vorgabe; als Referenz eignen
sich allein sichere Fehler-, Routen- und Testmuster. Der Checkout blieb
unverändert. Quellen: `package.json`, `.scaffold/{constraints,progress}.md`,
`extensions/xai/{auth,oauth,responses,routing}.ts`.
