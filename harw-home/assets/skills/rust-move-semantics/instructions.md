# Move vs Copy: When Values Are Consumed

**Regel:** Types that own heap memory (String, Vec, custom structs without Copy) are *moved* on assignment or into a function — the original binding becomes invalid. Types that are entirely stack-allocated and cheap to duplicate (integers, bool, char, Uuid, small Copy structs) are *copied* implicitly and both the original and destination stay valid.

**Warum:** Rust enforces single ownership to guarantee memory safety without a GC. Moving is the default; Copy is an opt-in marker that says "duplicating this is always safe and free".

## Falsch

```rust
// Takes String by value just to read it — caller permanently loses the String.
fn log_name(name: String) {
    println!("{name}");
}

fn main() {
    let n = String::from("invoice-42");
    log_name(n);
    log_name(n); // ✗ error[E0382]: use of moved value: `n`
}
```

## Richtig

```rust
// Reading only → borrow (&str).  Ownership transfer → consume self intentionally.

/// Reads a name without taking ownership.
fn log_name(name: &str) {
    println!("{name}");  // borrows; caller keeps the String
}

/// Typestate transition: consumes `self` so the old state cannot be reused.
/// Real pattern: apps/sgh-flow/src/typestate/api.rs:108
///   pub fn authenticate_bearer(self, claims: AuthClaims) -> ApiRequest<BearerAuth>
pub struct Request<S> {
    path: String,
    _state: std::marker::PhantomData<S>,
}

pub struct Unauthenticated;
pub struct Authenticated;

impl Request<Unauthenticated> {
    /// Consume `self` (move) so an unauthenticated request cannot be reused
    /// after authentication.  The compiler enforces this at zero runtime cost.
    pub fn authenticate(self, token: String) -> Request<Authenticated> {
        let _ = token; // validate in real code
        Request { path: self.path, _state: std::marker::PhantomData }
    }
}

fn main() {
    let n = String::from("invoice-42");
    log_name(&n);  // borrow — n still valid
    log_name(&n);  // ✓ fine

    // Copy type (u64): both variables remain valid after "assignment"
    let a: u64 = 42;
    let b = a;
    println!("{a} {b}");  // ✓ both valid; u64 implements Copy
}
```

## Quelle
- https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html
