# `while let`: Schleifen über optionale Werte

**Regel:** Ersetze jedes `loop { match x.next() { Some(v) => …, None => break } }` durch `while let Some(v) = x.next() { … }`.

**Warum:** `while let` drückt die Intention direkt aus — "solange das Muster passt, iteriere weiter". Der Compiler prüft Exhaustivität; ein übersehenes `None` kann nicht still durchfallen. Kein manuelles `break` am Schleifenende nötig.

## Falsch

```rust
// Verboser loop + match, der None explizit bricht
let mut chars = s.chars().peekable();
loop {
    let ch = match chars.next() {
        Some(c) => c,
        None => break,          // rein mechanischer Kontrollfluss
    };
    out.push(ch);
}

// Noch schlimmer: unwrap() statt Match
loop {
    let envelope = rx.recv().await.unwrap(); // Panik wenn Sender geschlossen
    process(envelope);
}
```

## Richtig

```rust
// Datei: apps/acme-app/src/services/python_agent.rs:49
// ANSI-Escape-Sequenzen aus einem String herausfiltern
let mut chars = s.chars().peekable();
while let Some(ch) = chars.next() {
    if ch == '\x1b' && chars.peek() == Some(&'[') {
        chars.next(); // '[' konsumieren
        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() { break; }
        }
    } else {
        out.push(ch);
    }
}

// Datei: apps/acme-app/src/services/child_supervisor.rs:245
// Veraltete Restart-Events aus dem Ringpuffer entfernen
while let Some(&front) = self.events.front() {
    if now.duration_since(front) > self.window {
        self.events.pop_front();
    } else {
        break; // Frühzeitiger Abbruch ist erlaubt
    }
}

// Datei: apps/acme-app/src/websocket/event_bridge.rs:539
// Async-Channel leeren bis Sender geschlossen wird
while let Some(envelope) = rx.recv().await {
    // rx.recv() gibt None zurück wenn alle Sender weg sind —
    // kein unwrap(), kein Panik-Risiko
    route_envelope(envelope).await;
}
```

## Quelle

- https://doc.rust-lang.org/book/ch19-01-all-the-places-for-patterns.html (Listing 19-4: `while let` Conditional Loops)
- https://doc.rust-lang.org/book/ch17-02-concurrency-with-async.html (Listing 17-10: `while let` mit async Channels)

Allgemeine Variante: `loops-and-control-flow` (siehe auch `null-and-optional-handling`)
