# Schleifen, Labels und Iterator-Chains

**Regel:** Greife nie per Index auf Collections zu (`collection[i]`). Nutze `for element in collection`, `loop { break wert }` für Retry-Logik, Loop-Labels für verschachtelte Schleifen, und Iterator-Chains (`.map().filter().collect()`) für Transformationen.

**Warum:** Index-Schleifen verursachen Panik bei falscher Länge, verlangen manuelle Grenzen-Prüfung und sind wartungsfeindlich. Iterator-Chains eliminieren Off-by-One-Fehler, sind komposierbar, und kommunizieren die Absicht klar. `loop { break wert }` ist der idiomatische Weg, einen Rückgabewert aus Retry-Logik zu gewinnen, ohne eine Hilfsvariable mit `unwrap()` zu führen.

## Falsch

```rust
// Index-Schleife: Panik bei falscher Länge
let rows = vec![row1, row2, row3];
for i in 0..rows.len() {
    process(rows[i].clone()); // .clone() statt Konvertierung; Panik möglich
}

// Retry mit Hilfsvariable und unwrap
let mut file_opt: Option<File> = None;
for _ in 0..10 {
    if let Ok(f) = File::open(&path) {
        file_opt = Some(f);
        break;
    }
}
let file = file_opt.unwrap(); // Panik wenn alle Versuche fehlschlugen

// Verschachtelte Schleife mit Flag-Variable
let mut found = false;
for outer in &matrix {
    for inner in outer {
        if *inner == target {
            found = true;
            break; // verlässt nur die innere Schleife!
        }
    }
    if found { break; }
}
```

## Richtig

```rust
// Iterator-Chain statt Index-Schleife
// Datei: apps/acme-app/src/db/repositories/invoice_items.rs:404
let items: Vec<InvoiceItemRow> = rows.into_iter().map(ItemRow::into_row).collect();
// into_iter() überträgt Ownership; keine Kopie, kein Index.

// `loop` mit break-Wert für Retry-Logik
// Datei: apps/acme-app/src/services/python_agent.rs:619
let file = loop {
    match File::open(&log_path).await {
        Ok(f) => break f,                          // Wert aus der Schleife heraus
        Err(_) if retries < 10 => {
            retries += 1;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(e) => {
            warn!(path = %log_path.display(), err = %e, "log file not found");
            return;
        }
    }
};
// `file` ist jetzt ein File, kein Option<File> — kein unwrap().

// Loop-Labels für verschachtelte Schleifen (Beispiel aus dem Rust Buch)
// Wenn kein Label: break verlässt nur die innerste Schleife.
'outer: for row in &matrix {
    for cell in row {
        if *cell == target {
            break 'outer; // verlässt BEIDE Schleifen, kein Flag nötig
        }
    }
}

// filter_map: kombiniertes Filter + Transformation ohne Zwischenvec
// Datei: apps/acme-app/src/runtime.rs:473
let resolved: Vec<_> = env_vars
    .filter_map(|(env_key, paths)| {
        std::env::var(env_key).ok().map(|val| (val, paths))
    })
    .collect();
```

## Quelle

- https://doc.rust-lang.org/book/ch03-05-control-flow.html (`loop`, `while`, `for`, Loop-Labels, `break` mit Wert)
- https://doc.rust-lang.org/book/ch13-02-iterators.html (Iterator-Chains: Adapter-Methoden und Consuming Adapters)

Allgemeine Variante: `loops-and-control-flow`
