---
id: HP-GW
title: Vertrag harw ↔ eigener Cloudflare-Worker vor Workers AI
status: draft
date: 2026-09-27
tags: [harness-patterns, gateway, workers-ai, cache, placement]
related:
  - README.md
  - claude-code.md
  - ../70-decisions/DEC-003-provider-limits.md
---

# Vertrag: harw ↔ eigener Cloudflare-Worker vor Workers AI

## Voraussetzung
Im Betrieb setzt harw einen **eigenen Cloudflare-Worker** vor Workers AI
voraus. Er ist die untere Stufe des Placements:
- Er hält eine oder mehrere Workers-AI-Bindings (Lanes).
- Er verteilt die Anfragen darauf.
- Er leitet die Session-Affinität selbst ab.

harw modelliert die einzelnen Bindings **nicht**. Für harw ist der Worker
**ein** Provider mit Kapazität.

Konto-, Binding- und Worker-Namen stehen nicht im Repo.

## Zweistufiges Placement
1. **harw** (Placement-Engine) wählt:
   - den Provider, also diesen Worker;
   - Modell und Rolle;
   - die Präfix-Gruppe.
2. **Der Worker** wählt Binding bzw. Lane und Affinität, die „sticky“ bleibt,
   und ruft Workers AI auf.

## Anfrage (harw → Worker)
- **API:** OpenAI-Chat-kompatibel (`api = "openai-chat"`).
- **Konfiguration:**
  - `base_url` zeigt auf den Worker.
  - `gateway_identity_headers = true`.
- **Header**, gesetzt nur bei gesetzter `RequestIdentity`, auf sichtbares ASCII
  gekürzt, maximal 64 Zeichen:
  - `x-harw-session`: Wurzel-Session des Agentenbaums.
  - `x-harw-agent`: ID des Agenten.
  - `x-harw-role`: Rolle, z. B. `root-orchestrator` oder `worker`.
- **`x-session-affinity`** setzt harw bewusst **nie**. Der Worker leitet ihn
  selbst ab. Quelle: `harw-provider-http/src/lib.rs`, `identity_headers`.
- **Kapazität auf harw-Seite (DEC-003):**
  - `max_concurrency` begrenzt die Parallelität.
  - `[rate_limit]` bildet RPM und TPM ab.
  - Frontier-Modelle bei Workers AI: 20 Anfragen pro Minute pro Modell und
    Konto mit Standard-Abrechnung, 50 mit AI-Gateway-Credits.

## Antwort (Worker → harw)
- **Format:** OpenAI-Chat, mit `usage`.
- **Cache-Tokens:** Gecachte Input-Tokens werden als eigene Zahl gemeldet.
  Nur so kann harw die Kosten richtig rechnen, siehe Kostenmodell.
- **429:** mit `Retry-After`. harw pausiert dann und zählt den Versuch nicht.

## Geplante Erweiterungen (optional, abwärtskompatibel)
- **`x-harw-cache-affinity`** (Anfrage):
  - Benennt die **Präfix-Gruppe**, z. B. `session/rolle/präfix-hash`.
  - Kurzlebige Worker mit gleichem Präfix, also gleichem Systemprompt, gleichen
    Tools und gleichem Repo-Kontext, landen dadurch auf derselben Instanz, und
    der Cache bleibt warm.
  - Ist der Header nicht gesetzt, fällt der Worker auf Session plus Agent zurück.
  - Gesetzt wird er von der Placement-Engine bzw. vom Work-Driver.
- **`x-harw-lane` und `x-harw-affinity`** (Antwort): Sie melden, welche Lane
  und welcher Affinitätsschlüssel benutzt wurden. Das dient der Beobachtung,
  weil Logpush nicht in jedem Konto verfügbar ist.
- **Kapazitätssignale** (Antwort, optional): Restkontingent und Reset-Zeit, in
  denselben Header-Familien, die `ProviderRateLimiter` schon liest.

## Warum so (Muster)
- **Die Cache-Affinität folgt dem Präfix, nicht der Identität.** Bei
  cache-lastigen Workloads (etwa 98 % Cache-Reads) entscheidet der Preis für
  Cache-Read über die Kosten. Einen kalten Start zahlt jeder Worker zum vollen
  Input-Preis.
- **Kosten und Usage kommen aus harw selbst** (Usage pro Antwort, Kosten-Status)
  und nicht aus Logpush.
