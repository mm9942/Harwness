# Secrets im Code: Redaction, Log-Hygiene, Vergleich, Speicher — sprachunabhängig

**Regel:**
1. **Secrets leben in einem eigenen Wrapper-Typ**, dessen `toString`/`repr`/`Debug`/`Display`/JSON-Serialisierung **redacted** ausgibt (`***`). Der Klartext wird nur über eine explizit benannte Methode geholt (`expose_secret()`, `reveal()`).
2. **Nie in Logs, Fehlermeldungen, Exceptions, URLs, CLI-Argumenten oder Config-Dumps.**
3. **Vergleiche in konstanter Zeit** (Tokens, HMACs, API-Keys).
4. **Kurz im Speicher halten und überschreiben**, wo die Sprache es erlaubt (Rust `zeroize`, Go `clear([]byte)`, Java `Arrays.fill(char[])`, Node `Buffer.fill(0)`, Python `bytearray`) — mit ehrlichem Wissen um die Grenzen.

**Warum:** Die häufigsten Secret-Leaks sind banal: ein Debug-Log des Config-Objekts, eine Exception-Message mit Connection-String, ein Token im Query-String im Access-Log, ein Passwort als CLI-Argument in `ps`. Redaction-Typen verhindern das **per Default**. Nicht-konstante Vergleiche erlauben Timing-Angriffe. Klartext, der nach Gebrauch im Heap liegen bleibt, landet in Core-Dumps und Swap.

## Wann anwenden

- Code liest/speichert/übergibt Passwörter, API-Keys, Tokens, private Schlüssel, Session-IDs, Connection-Strings.
- Ein Config-/Settings-Objekt enthält Secrets und wird geloggt oder serialisiert.
- Tokens/Signaturen werden verglichen.
- Kryptografisches Schlüsselmaterial wird entschlüsselt und kurz benutzt.

## Falsch

```python
@dataclass
class Settings:
    db_url: str          # postgres://user:PASSWORT@host/db
    api_key: str
log.info("starting with %s", settings)          # dataclass-repr loggt alles im Klartext
if request_token == settings.api_key: ...       # früher Abbruch beim ersten falschen Zeichen → Timing
```

```typescript
throw new Error(`login failed for ${user} with password ${password}`);
fetch(`https://api.example.com/data?api_key=${key}`); // landet in Proxy-/Access-Logs
```

```go
cmd := exec.Command("tool", "--password", pw) // für jeden Nutzer via `ps` sichtbar
```

```rust
fn unwrap_secret_key(&self, enc: &[u8]) -> Result<Vec<u8>> { /* … */ } // Klartext bleibt nach Drop im Heap
```

## Richtig

### Rust — `zeroize` + redacted `Debug`

```rust
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct MasterKek { secret_key: Vec<u8> }

impl std::fmt::Debug for MasterKek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKek(***)")
    }
}

fn unwrap_secret_key(&self, enc: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    Ok(Zeroizing::new(open_for_recipient(self.kek.secret_key(), enc)?)) // überschrieben beim Drop
}
// Konstante Zeit: subtle::ConstantTimeEq — a.ct_eq(b).into()
```

### Python — Redaction-Typ, `hmac.compare_digest`

```python
import hmac

class Secret:
    __slots__ = ("_value",)
    def __init__(self, value: str) -> None:
        self._value = value
    def reveal(self) -> str:
        return self._value
    def __repr__(self) -> str:
        return "Secret('***')"
    __str__ = __repr__

@dataclass
class Settings:
    db_url: Secret
    api_key: Secret

def check_token(given: str, settings: Settings) -> bool:
    return hmac.compare_digest(given.encode(), settings.api_key.reveal().encode())
```

(Pydantic: `SecretStr`. Python-`str` ist immutable und kann nicht überschrieben werden — für Schlüsselbytes `bytearray` nutzen und danach `buf[:] = bytes(len(buf))`; garantiert ist das trotzdem nicht, weil Kopien entstehen können.)

### TypeScript/Node — Redaction via `toJSON`/`inspect`, `timingSafeEqual`

```typescript
import { timingSafeEqual } from "node:crypto";
import { inspect } from "node:util";

export class Secret {
  #value: string;                                  // echtes privates Feld
  constructor(value: string) { this.#value = value; }
  reveal(): string { return this.#value; }
  toString() { return "***"; }
  toJSON() { return "***"; }
  [inspect.custom]() { return "Secret(***)"; }
}

function tokensEqual(a: Buffer, b: Buffer): boolean {
  return a.length === b.length && timingSafeEqual(a, b); // timingSafeEqual wirft bei ungleicher Länge
}

// Header statt Query-String
await fetch(url, { headers: { Authorization: `Bearer ${token.reveal()}` } });
```

### Go — `[]byte` statt `string`, `clear`, `subtle`, Stdin/Env statt Argument

```go
type Secret []byte

func (Secret) String() string   { return "***" }         // fmt.Print, %v, %s
func (Secret) GoString() string { return "Secret(***)" } // %#v

func use(key Secret) {
    defer clear(key) // Go 1.21+: überschreibt den Inhalt mit Nullen
    // …
}

ok := subtle.ConstantTimeCompare(given, expected) == 1

cmd := exec.Command("tool", "--password-stdin")
cmd.Stdin = bytes.NewReader(pw)
```

### Java — `char[]` + `Arrays.fill`, `MessageDigest.isEqual`

```java
char[] password = console.readPassword();
try {
    authenticate(password);
} finally {
    Arrays.fill(password, '\0');
}
boolean ok = MessageDigest.isEqual(givenMac, expectedMac);
```

(C#: `SecureString` wird von Microsoft für neuen Code **nicht** empfohlen — lieber Secrets kurz halten, nicht loggen, und OS-/Vault-Mechanismen nutzen.)

## Grenzen des Überschreibens (ehrlich bleiben)

- GC-Sprachen können Objekte beim Kompaktieren **kopieren**; alte Kopien werden nicht überschrieben.
- Immutable Strings (Python, Java, JS, C#, Go-`string`) sind nicht löschbar → Secrets möglichst als Bytes/`char[]` halten.
- Rust `Vec` kann bei Reallokation alte Kopien hinterlassen → Kapazität vorab festlegen oder feste Arrays.
- Kein Schutz vor Swap/Core-Dumps ohne OS-Maßnahmen (`mlock`, Core-Dumps deaktivieren).
- Überschreiben ist **Defense in Depth**; Log- und Fehler-Hygiene verhindern die weitaus häufigeren Leaks.

## Fallstricke

- **Auto-generierte `repr`/`toString`/`Debug`** (dataclass, record, `#[derive(Debug)]`, Lombok `@Data`) geben Secrets aus.
- **Structured Logging mit ganzem Objekt** (`log.info("cfg", cfg)`) — Felder einzeln und bewusst loggen.
- **Exceptions aus HTTP-Clients** enthalten manchmal URL inkl. Query-String oder Header — vor dem Loggen prüfen.
- **Secrets in Env-Dumps/Crash-Reports/Telemetry** (Sentry-Breadcrumbs, `process.env` dumpen).
- **Längenvergleich vor `timingSafeEqual`** leakt nur die Länge — bei HMACs mit fixer Länge unkritisch; bei Tokens ggf. vorher hashen.
- **Test-Fixtures mit echten Secrets** im Repo.

## Checkliste

1. Liegt jedes Secret in einem Redaction-Typ (oder mindestens nicht in automatisch geloggten Strukturen)?
2. Taucht ein Secret in Log, Exception-Message, URL, CLI-Argument oder Serialisierung auf?
3. Werden Tokens/MACs in konstanter Zeit verglichen?
4. Wird Schlüsselmaterial nach Gebrauch überschrieben, wo die Sprache es erlaubt?
5. Sind die Grenzen dokumentiert (Kopien, immutable Strings)?
6. Rust zusätzlich: `rust-zeroize-secrets`.

## Quelle

- https://docs.rs/zeroize/latest/zeroize/
- https://docs.python.org/3/library/hmac.html#hmac.compare_digest
- https://nodejs.org/api/crypto.html#cryptotimingsafeequala-b
- https://pkg.go.dev/crypto/subtle#ConstantTimeCompare
- https://learn.microsoft.com/en-us/dotnet/fundamentals/runtime-libraries/system-security-securestring
- https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html
