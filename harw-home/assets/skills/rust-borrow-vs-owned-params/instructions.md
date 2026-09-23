# &str / &[T] / &Path in Parameters — Owned Only When Ownership Transfers

**Regel:** Accept `&str`, `&[T]`, and `&Path` in function parameters whenever the function only reads the data. Return an owned `String`, `Vec<T>`, or `PathBuf` only when the caller must own the result. Never take `&String`, `&Vec<T>`, or `&PathBuf` — these force the caller to already hold an owned type and gain nothing over the slice form.

**Warum:** `&str` accepts both `&String` and string literals without allocation. `&[u8]` accepts both `&Vec<u8>` and array slices. `&Path` accepts both `&PathBuf` and `Path::new("…")`. Using owned types in parameters forces unnecessary allocations and restricts callers.

## Falsch

```rust
// ✗ Takes &String, &Vec<u8>, &PathBuf — over-constrained and misleading.
// Caller must hold an owned String/Vec/PathBuf even to pass a literal.
fn store_secret(
    name: &String,       // ✗ use &str
    plaintext: &Vec<u8>, // ✗ use &[u8]
    config_path: &PathBuf, // ✗ use &Path
) {
    let _ = (name, plaintext, config_path);
}

// ✗ Takes owned String just to display it — caller permanently loses the String.
fn log_label(label: String) {
    println!("{label}");
}
```

## Richtig

```rust
use std::path::Path;

// ✓ Borrow slices; accept any string/slice/path source without allocation.
// Real code: crates/securehub-service/src/service.rs:710-716
//   pub async fn store_secret(
//       &self,
//       principal: &Principal,
//       vault_id: Uuid,
//       name: &str,          ← &str, not &String
//       plaintext: &[u8],    ← &[u8], not &Vec<u8>
//       last_four: Option<&str>,
//   ) -> Result<Uuid>
fn store_secret(
    name: &str,
    plaintext: &[u8],
    config_path: &Path,
) {
    let _ = (name, plaintext, config_path);
}

fn log_label(label: &str) {   // ✓ borrow — caller keeps their String
    println!("{label}");
}

// ✓ Return owned when the caller MUST own the result (new allocation).
fn build_key_name(prefix: &str, suffix: &str) -> String {
    format!("{prefix}-{suffix}")  // caller needs this owned String
}

fn main() {
    // &str works for literals AND String — no conversion needed.
    store_secret("my-key", b"raw-bytes", Path::new("/etc/cfg"));

    let owned = String::from("invoice");
    store_secret(&owned, b"bytes", Path::new("/tmp/x"));
    log_label(&owned);  // ✓ owned still valid after the call
}
```

## Quelle
- https://doc.rust-lang.org/book/ch04-02-references-and-borrowing.html
- https://doc.rust-lang.org/book/ch04-03-slices.html
