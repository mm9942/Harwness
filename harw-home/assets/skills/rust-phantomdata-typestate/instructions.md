# PhantomData + Zero-Sized Marker Structs für Typestate-Maschinen

**Regel:** Kapsle Zustand als Typ-Parameter mit Zero-Sized Marker Structs und `PhantomData<State>`. Transitionen nehmen `self` by value (konsumieren den alten Zustand) und geben einen neuen Typen zurück. Ungültige Transitionen existieren als Methoden gar nicht — sie sind zur Compile-Zeit nicht aufrufbar.

**Warum:** Runtime-Enums prüfen Zustandsübergänge erst zur Laufzeit und erzeugen `match`/`if`-Äste und potenzielle `panic!`-Pfade. Typestate verlagert dieselbe Invariante in das Typensystem: der Compiler beweist die Korrektheit.

> „Zero-sized type used to mark things that 'act like' they own a `T`, even though they don't really. Adding a `PhantomData<T>` field to your type tells the compiler that your type acts as though it stores a value of type `T`, even though it doesn't really."
> — std::marker::PhantomData, Rust Standard Library docs

## Falsch

```rust
// Laufzeit-Enum für Zustand — Fehler erst zur Laufzeit erkennbar
#[derive(Debug)]
enum S3State { Pending, Stored, Tombstoned }

struct S3Object {
    state: S3State,
    key: String,
}

impl S3Object {
    // Nichts hindert den Aufrufer daran, cancel() nach mark_stored() zu rufen
    fn mark_stored(&mut self) { self.state = S3State::Stored; }
    fn cancel(&mut self)      { self.state = S3State::Tombstoned; }

    fn retrieve(&self) -> Result<String, String> {
        // Schutz erst zur Laufzeit
        match self.state {
            S3State::Stored => Ok(self.key.clone()),
            _ => Err("wrong state".into()),
        }
    }
}
```

## Richtig

```rust
// apps/sgh-flow/src/typestate/s3.rs:49-92
use std::marker::PhantomData;

// Zero-Sized Marker Structs — keine Laufzeit-Kosten
#[derive(Debug, Clone, Copy)] pub struct Pending;
#[derive(Debug, Clone, Copy)] pub struct Stored;
#[derive(Debug, Clone, Copy)] pub struct Tombstoned;

#[derive(Debug, Clone)]
pub struct S3Object<State = Pending> {
    pub key: String,
    pub bucket: String,
    _state: PhantomData<State>,   // apps/sgh-flow/src/typestate/s3.rs:92
}

// Nur S3Object<Pending> hat mark_stored() und cancel()
impl S3Object<Pending> {
    pub fn new(bucket: &str, key: &str) -> Self {
        Self { bucket: bucket.to_owned(), key: key.to_owned(), _state: PhantomData }
    }

    // Transition konsumiert self → Pending-Objekt existiert danach nicht mehr
    // apps/sgh-flow/src/typestate/s3.rs:127
    pub fn mark_stored(self) -> S3Object<Stored> {
        S3Object { bucket: self.bucket, key: self.key, _state: PhantomData }
    }

    pub fn cancel(self) -> S3Object<Tombstoned> {
        S3Object { bucket: self.bucket, key: self.key, _state: PhantomData }
    }
}

// Nur S3Object<Stored> hat retrieve()
impl S3Object<Stored> {
    pub fn retrieve(&self) -> &str {
        &self.key
    }
}

// S3Object<Tombstoned> hat absichtlich keine Methoden außer Debug
```

Aufruf (kompiliert problemlos):

```rust
let obj = S3Object::new("invoices", "2024/invoice.pdf");
let stored = obj.mark_stored();       // obj ist jetzt bewegt
let url = stored.retrieve();          // nur auf Stored erlaubt
// obj.cancel();                      // COMPILE-FEHLER: obj wurde bewegt
// stored.cancel();                   // COMPILE-FEHLER: Stored hat keine cancel()-Methode
```

### Session-Rollen: gleiches Muster

```rust
// apps/sgh-flow/src/typestate/session.rs:46 + :80-129
use std::marker::PhantomData;

pub struct Session<Role> { user_id: Option<Uuid>, _role: PhantomData<Role> }

pub struct Anonymous;
pub struct Admin;

// create_user() existiert nur auf Session<Admin> — kein Runtime-Check nötig
impl Session<Admin> {
    pub async fn create_user(&self, /* … */) -> AppResult<Uuid> { /* … */ }
}
// Session<Anonymous>::create_user() gibt es nicht → Compile-Fehler beim Aufruf
```

### Hausregeln

- Marker-Structs leben unter `src/typestate/` (Projekt-Konvention).
- `_state: PhantomData<State>` — `_`-Prefix, da das Feld nie direkt gelesen wird.
- Transitionen immer `fn foo(self) -> Self<NewState>` — niemals `&mut self`.
- Kein `unsafe`-Block ohne Safety-Kommentar.
- Kein `unwrap()`/`expect()` in Produktionspfaden — `?` und `Err(…)` stattdessen.
- Kein `println!` / `log` — ausschließlich `tracing`.

## Quelle

- https://doc.rust-lang.org/std/marker/struct.PhantomData.html

Allgemeine Variante: `typestate-and-illegal-states`
