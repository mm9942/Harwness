# E0505: Cannot Move Out of Value Because It Is Borrowed

**Regel:** Wenn du einen Wert in einen Match-Arm entleihst und ihn danach in eine Funktion *bewegst*, brich die Ausleihe zuerst ab — bilde den geliehenen Wert in einen eigenen Wert um, bevor der Match beginnt.

**Warum:** Der Borrow Checker verbietet es, einen Wert zu *moven*, solange noch eine Referenz darauf lebt. `request.method()` gibt `&Method` zurück — eine Referenz *in* `request`. Wenn der Match-Arm diese Referenz als `method` bindet und du dann `request` in `dispatch_business` bewegst, existieren Borrow und Move gleichzeitig: E0505.

## Falsch

```rust
// handlers.rs:120,170,206 — original pattern
pub async fn dispatch(
    context: ApiContext,
    handler: &str,
    request: Request<Incoming>,          // owned here
) -> Result<Response<ResponseBody>> {
    match (request.method(), handler) {  // borrows request → method: &Method into request
        (&Method::GET, "health") => { /* ... */ }
        (&Method::POST, "mcp") => {
            // consumes request body here...
        }
        (method, business_handler) => {  // method: &Method still borrows request
            // ... build service ...
            dispatch_business(          // ERROR E0505: moves request while method borrows it
                service,
                svc_principal,
                method,                 // &Method points into request
                business_handler,
                request,               // cannot move: request is borrowed
            ).await
        }
    }
}
```

## Richtig

```rust
pub async fn dispatch(
    context: ApiContext,
    handler: &str,
    request: Request<Incoming>,
) -> Result<Response<ResponseBody>> {
    // Own the method up-front so no reference into `request` outlives the
    // point where `request` is moved into dispatch_business.
    // Method implements Clone; `.to_owned()` is the house-style specific
    // conversion (prefer over `.clone()` when a dedicated method exists).
    let method = request.method().to_owned();      // handlers.rs:120 (fix)

    match (&method, handler) {
        (&Method::GET, "health") => {
            json_response(StatusCode::OK, json!({"status": "ok"}))
        }
        (&Method::POST, "mcp") => {
            // request can be consumed freely here — method is already owned
            let body = request
                .into_body()
                .collect()
                .await
                .map_err(|e| Error::Http { message: e.to_string() })?
                .to_bytes();
            // ...
            todo_handle_mcp(body)
        }
        (_, business_handler) => {
            let api_principal =
                extract_principal(request.headers(), &context.config)
                    .map_err(|err| { /* ... */ err })?;
            let svc_principal = ServicePrincipal {
                sub: api_principal.sub,
                org_id: api_principal.organization_id,
            };
            let pool = context.db_pool.ok_or_else(|| Error::Config {
                key: "DATABASE_POOL".to_owned(),
                reason: "pool not initialized".to_owned(),
            })?;
            let master_kek = Arc::new(MasterKek::from_env()?);
            let service =
                SecureHubService::new(pool, master_kek, context.config.default_crypto);
            // `request` moves here; `method` is an independent owned value — no conflict
            dispatch_business(service, svc_principal, &method, business_handler, request).await
            //                                         ^^^^^^^ borrow of owned local, not of request
        }
    }
}
```

**Alternative:** Alles Nötige aus `request` extrahieren (z.B. `let method = …`, `let headers = …`), *bevor* du `request` bewegst. Welcher Weg besser passt, hängt davon ab, ob der Match viele Arme hat, die `request` selbst konsumieren müssen.

## Quelle

- <https://doc.rust-lang.org/book/ch04-02-references-and-borrowing.html>
- `rustc --explain E0505` (Cannot move out of value because it is borrowed)
