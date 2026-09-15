# Emily — persönliche Harw-Konfiguration

Diese Vorlage richtet unter `~/.harw` **Emily** als warmherzige, sehr sorgfältige UI- und Server-Administrations-Agentin ein. Sie trennt Koordination und Ausführung, damit Aufgaben nachvollziehbar und mit klaren Sicherheitsgrenzen bearbeitet werden.

## Installation

> Sichere zuerst eine bestehende persönliche Konfiguration.

```sh
if [ -d "$HOME/.harw" ]; then
  mv "$HOME/.harw" "$HOME/.harw.backup-$(date +%Y%m%d-%H%M%S)"
fi
cp -a .harw "$HOME/.harw"
```

Die Konfiguration enthält absichtlich **keine Provider, Modelle oder Secrets**. Ergänze diese separat in `~/.harw/providers/`, `~/.harw/models/` und ggf. `~/.harw/auth.toml`. Geheimnisse werden nur als Referenzen wie `env:OPENAI_API_KEY` eingetragen, nie als Klartext.

## Agenten-Hierarchie

```text
Emily (UI-Agentin; warm, sorgfältig, transparent)
└── Coding Orchestrator (Root-Orchestrator)
    ├── Dependency Research Orchestrator
    │   ├── Source Researcher
    │   └── Dependency Analyst
    ├── Debug Orchestrator
    │   ├── Incident Triager
    │   ├── Log Analyst
    │   └── Debugger
    ├── Implementation Orchestrator
    │   ├── Rust Implementer
    │   ├── Test Engineer
    │   └── Refactorer
    └── Security Inspection Orchestrator
        ├── Security Auditor
        ├── Secret Scanner
        └── Dependency Security Reviewer

System Observer ist ein zusätzlicher read-only Worker für Serverzustände.
```

Die TOML-Rollen entsprechen der geschlossenen Harw-Hierarchie: ein UI-Agent, ein Root-Orchestrator, Child-Orchestratoren und Worker. Orchestratoren planen und delegieren; Worker bleiben auf begrenzte Aufgaben beschränkt.

## Sicherheitsprinzipien

- Beobachten und Evidenz sichern, bevor Änderungen vorgeschlagen werden.
- Privilegierte, destruktive, externe oder sicherheitsrelevante Aktionen benötigen Freigabe.
- Security-Worker führen keine offensiven Aktionen aus; sie prüfen autorisierte Scopes nicht-destruktiv.
- Jeder Abschluss berichtet Ergebnis, Evidenz/Verifikation, Restrisiken und Blocker.
- Keine Zugangsdaten in TOML eintragen.

## Hinweis zur Laufzeit

Die Verzeichnisform `agents/<name>/agent.toml` und `skills/<name>/skill.toml` entspricht der aktuellen `.harw`-Discovery. Die hier beschriebenen Eltern-Kind-Beziehungen und Delegationsgrenzen sind in den Prompts dokumentiert. Eine vollständig erzwingbare Organisationsvorlage benötigt zusätzlich die in `agent-definition-dsl.md` beschriebene, noch separat kompilierte Agent-DSL.
