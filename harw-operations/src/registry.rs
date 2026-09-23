//! Zentrale Operation-Registry: hybrid aus `inventory`-gesammelten Built-ins
//! und explizit registrierten Operationen (z. B. aus Plugins/Extensions).
//!
//! # Verantwortungsbereich
//! Dieses Modul verwaltet eine geordnete Sammlung von [`Operation`]-Instanzen.
//! Es unterscheidet zwei Registrierungskanäle:
//!
//! - **Built-ins** (statisch): Operationen, die über [`inventory::submit!`]
//!   beim Kompilieren in den Binary eingelinkt werden. [`OperationRegistry::built_in`]
//!   sammelt sie alle via [`inventory::iter`].
//!   *Reserviert für zukünftige dynamische Op-Packs. Aktuell leer — das kanonische
//!   Built-in-Pack wird via `harw_ops::register_all()` registriert.*
//! - **Dynamisch**: Operationen, die zur Laufzeit via [`OperationRegistry::register`]
//!   oder [`OperationRegistry::try_register`] hinzugefügt werden (z. B. aus Plugin-Laden
//!   oder Erweiterungs-Systemen).
//!
//! # Schlüsseltypen
//! - [`InventoryOp`] — Slot für `inventory::submit!`-Deklarationen von Built-ins.
//! - [`OperationRegistry`] — Hauptstruktur; hält `Vec<Arc<dyn Operation>>` in Insert-Reihenfolge.
//! - [`RegistryError`] — Fehlertyp für deterministische Duplikat-Erkennung in [`OperationRegistry::try_register`].
//!
//! # Nebenläufigkeit
//! [`OperationRegistry`] ist `Send + Sync`, da `Arc<dyn Operation>` es voraussetzt
//! und `Vec<Arc<dyn Operation>>` beides implementiert, sobald `Operation: Send + Sync`.
//! Die Registry selbst bietet **keine innere Mutabilität** — für parallele Zugriffe
//! muss die aufrufende Schicht `Arc<RwLock<OperationRegistry>>` o. Ä. verwenden.
//!
//! # Fehlertypen
//! - [`RegistryError`]: Wird von [`OperationRegistry::try_register`] bei Kollisionen erzeugt.
//!   [`OperationRegistry::register`] ist die infallible Variante; sie paniziert bei Kollisionen.
//!
//! # Beispiel
//! ```rust,no_run
//! use std::sync::Arc;
//! use harw_operations::registry::OperationRegistry;
//!
//! // Leere Registry anlegen und dynamisch erweitern:
//! let mut registry = OperationRegistry::new();
//! // registry.register(Arc::new(my_op));
//! assert!(registry.is_empty());
//! ```

use std::sync::Arc;

use crate::operation::{Operation, Surface, WebMethod};

// ── RegistryError ─────────────────────────────────────────────────────────────

/// Error returned by [`OperationRegistry::try_register`] when a collision is detected.
///
/// # Description
/// Covers four distinct failure modes that can arise when registering a new
/// operation against an already-populated registry. The name/alias checks are
/// deterministic and case-insensitive; the web-route check compares the full
/// `(path, method)` pair — `path` as an exact, case-sensitive string match
/// (HTTP paths are case-sensitive), `method` as an exact enum match (F-031).
///
/// # Variants
/// - [`Self::DuplicateName`]: Two ops share the same canonical name.
/// - [`Self::AliasCollision`]: A new op's alias shadows another op's name or alias.
/// - [`Self::SelfCollision`]: An op declares its own name as one of its aliases, or
///   lists the same alias string more than once.
/// - [`Self::WebPathCollision`]: Two ops declare a `Surface::Web` with the identical
///   `(path, method)` pair — see that variant's own doc for why this check lives in
///   the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// An operation with this canonical name is already registered.
    DuplicateName {
        /// The colliding name.
        name: String,
    },
    /// An alias claimed by `second_owner` collides with a name or alias already owned
    /// by `first_owner`.
    AliasCollision {
        /// The alias string that collides.
        alias: String,
        /// Name of the operation that first claimed `alias`.
        first_owner: String,
        /// Name of the operation that tried to claim `alias` again.
        second_owner: String,
    },
    /// An operation contains an internal name/alias inconsistency.
    SelfCollision {
        /// Name of the offending operation.
        owner: String,
        /// Human-readable description of the inconsistency.
        reason: String,
    },
    /// Two operations declare a `Surface::Web` entry with the identical `path`
    /// **and** the identical `method` (F-031: `method` is part of route
    /// identity, exactly like an HTTP router treats `GET /x` and `POST /x` as
    /// two distinct, non-colliding routes).
    ///
    /// # Why `(path, method)`, not `path` alone
    /// Before F-031, `Surface::Web` carried no explicit `method` — a `path`
    /// match was the only possible collision signal. Now that `method` is
    /// mandatory and explicit, two operations may legitimately share a `path`
    /// as long as they answer to different methods (a read route and a write
    /// route under the same URL, e.g. `GET /api/session` vs.
    /// `POST /api/session`). Colliding only on the full pair keeps that
    /// pattern possible while still rejecting two operations that would
    /// otherwise be indistinguishable to `harw-web` (same `path` **and**
    /// same `method`).
    ///
    /// # Why this check lives in the registry, not the macro
    /// `#[operation(...)]` (`harw-macros`) expands one operation at a time — it
    /// has no visibility into any other operation's declared surfaces, so it
    /// cannot detect a cross-operation path collision at compile time. The
    /// registry is the first point that ever sees every operation together, so
    /// it is the only place this check can run. Two operations that answered
    /// to the same `(path, method)` pair would be indistinguishable to
    /// `harw-web` — a silent "first one wins" would hide the collision
    /// instead of failing loudly.
    WebPathCollision {
        /// The colliding HTTP path.
        path: String,
        /// The colliding HTTP method (identical on both sides — see above).
        method: WebMethod,
        /// Name of the operation that first claimed `(path, method)` as a `Surface::Web`.
        first_owner: String,
        /// Name of the operation that tried to claim `(path, method)` again.
        second_owner: String,
    },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateName { name } => {
                write!(f, "duplicate operation name: '{name}'")
            }
            Self::AliasCollision {
                alias,
                first_owner,
                second_owner,
            } => {
                write!(
                    f,
                    "alias '{alias}' is claimed by both '{first_owner}' and '{second_owner}'"
                )
            }
            Self::SelfCollision { owner, reason } => {
                write!(f, "operation '{owner}' has a self-collision: {reason}")
            }
            Self::WebPathCollision {
                path,
                method,
                first_owner,
                second_owner,
            } => {
                write!(
                    f,
                    "web route '{method:?} {path}' is claimed by both '{first_owner}' and '{second_owner}'"
                )
            }
        }
    }
}

impl std::error::Error for RegistryError {}

// ── InventoryOp ───────────────────────────────────────────────────────────────

/// Slot, der via `inventory::submit!` von Built-in-Operationen befüllt wird.
///
/// # Beschreibung
/// Built-in-Operationen (d. h. Implementierungen, die direkt in die Binary
/// gelinkt werden) deklarieren sich durch einen `inventory::submit!`-Aufruf
/// in ihrer jeweiligen Crate:
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use harw_operations::registry::InventoryOp;
///
/// // In der Crate, die eine Built-in-Operation bereitstellt:
/// // inventory::submit!(InventoryOp { factory: || Arc::new(MyOp) });
/// ```
///
/// Die Fabrik-Funktion wird von [`OperationRegistry::built_in`] einmal pro
/// registrierter Built-in aufgerufen, um eine `Arc<dyn Operation>`-Instanz zu erzeugen.
///
/// # Felder
/// - `factory` (`fn() -> Arc<dyn Operation>`): Fabrik-Funktion ohne Argumente,
///   die eine neue Instanz der Operation erzeugt. Sie wird nur einmal während
///   des [`OperationRegistry::built_in`]-Aufrufs aufgerufen.
///
/// # Nebenläufigkeit
/// `InventoryOp` ist `Sync`, da `fn() -> _` immer `Send + Sync` ist und
/// `inventory` den Zugriff auf alle Slots thread-safe kapselt.
pub struct InventoryOp {
    /// Fabrik-Funktion, die eine gemeinsame `Arc<dyn Operation>` erzeugt.
    pub factory: fn() -> Arc<dyn Operation>,
}

// Reserved for future dynamic op-packs. Currently empty; the canonical built-in
// pack is registered via harw-ops::register_all().
inventory::collect!(InventoryOp);

// ── OperationRegistry ─────────────────────────────────────────────────────────

/// Zentrale Sammlung aller im Prozess sichtbaren `Operation`-Instanzen.
///
/// # Beschreibung
/// `OperationRegistry` hält eine geordnete Liste von `Arc<dyn Operation>` in
/// der Reihenfolge ihrer Registrierung (Insert-Order). Sie unterstützt zwei
/// Registrierungskanäle:
///
/// - **Statische Built-ins**: Gesammelt über [`Self::built_in`] aus allen
///   `inventory::submit!`-Deklarationen in der Binary.
/// - **Dynamische Operationen**: Hinzugefügt via [`Self::register`].
///
/// Die Registry ist nicht thread-safe für parallele Schreibzugriffe; wrap sie
/// in `Arc<RwLock<OperationRegistry>>` wenn nötig.
///
/// # Nebenläufigkeit
/// `OperationRegistry` ist `Send + Sync` (alle Felder sind es). Sie enthält
/// keine innere Mutabilität — Schreibzugriffe erfordern `&mut self`.
pub struct OperationRegistry {
    ops: Vec<Arc<dyn Operation>>,
}

impl OperationRegistry {
    /// Erstellt eine leere Registry ohne registrierte Operationen.
    ///
    /// # Beschreibung
    /// Erzeugt eine neue `OperationRegistry` mit einer leeren internen Liste.
    /// Weder Built-ins noch dynamisch registrierte Operationen sind enthalten.
    /// Verwende [`Self::built_in`], um alle `inventory::submit!`-Deklarationen
    /// zu sammeln, oder [`Self::register`], um einzelne Operationen hinzuzufügen.
    ///
    /// # Rückgabe
    /// Eine neue, leere `OperationRegistry`.
    ///
    /// # Nebenläufigkeit
    /// Diese Funktion ist `Send + Sync`-sicher und kann von jedem Thread aufgerufen werden.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::registry::OperationRegistry;
    ///
    /// let registry = OperationRegistry::new();
    /// assert!(registry.is_empty());
    /// assert_eq!(registry.len(), 0);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    /// Baut eine Registry aus allen `inventory::submit!`-Registrierungen.
    ///
    /// # Beschreibung
    /// Iteriert über alle [`InventoryOp`]-Slots, die via `inventory::submit!`
    /// beim Kompilieren in die Binary eingelinkt wurden, ruft für jeden die
    /// `factory`-Funktion auf und legt die erzeugte `Arc<dyn Operation>` in
    /// der Registry ab.
    ///
    /// **Wichtig**: Die resultierende Registry enthält ausschließlich Built-ins.
    /// Dynamisch nachregistrierte Operationen (via [`Self::register`]) sind
    /// nicht enthalten — diese müssen nach dem Aufruf manuell hinzugefügt werden.
    ///
    /// Die Reihenfolge der Built-ins ist definiert durch die Initialisierungsreihenfolge
    /// des `inventory`-Crates (deterministisch innerhalb einer Build, aber nicht
    /// notwendigerweise zwischen unterschiedlichen Builds konfigurierbar).
    ///
    /// # Rückgabe
    /// Eine neue `OperationRegistry`, die alle Built-in-Operationen enthält.
    ///
    /// # Nebenläufigkeit
    /// `inventory::iter` ist thread-safe; diese Funktion kann von jedem Thread
    /// aufgerufen werden, solange keine gleichzeitigen Schreibzugriffe auf die
    /// Registry-Instanz stattfinden.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::registry::OperationRegistry;
    ///
    /// // Läuft ohne Panic; Länge hängt von submit!-Deklarationen in der Binary ab.
    /// let registry = OperationRegistry::built_in();
    /// let _ = registry.len(); // 0 wenn keine Built-ins registriert
    /// ```
    #[must_use]
    pub fn built_in() -> Self {
        let ops = inventory::iter::<InventoryOp>
            .into_iter()
            .map(|slot| (slot.factory)())
            .collect();
        Self { ops }
    }

    /// Fügt eine dynamisch (z. B. via Plugin) erzeugte Operation hinzu.
    ///
    /// # Beschreibung
    /// Infallible convenience wrapper around [`Self::try_register`]. Appends the
    /// operation to the internal list. Performs the same deterministic duplicate
    /// and alias-collision checks as [`Self::try_register`] — **panics** if a
    /// collision is detected. This contract keeps built-in packs (which must be
    /// collision-free by construction) simple to call without error-handling noise,
    /// while making collisions loudly visible during development.
    ///
    /// For plugin-supplied or user-provided ops, prefer [`Self::try_register`] to
    /// handle collisions gracefully.
    ///
    /// # Panics
    /// Panics when the new operation would produce a [`RegistryError::DuplicateName`],
    /// [`RegistryError::AliasCollision`], or [`RegistryError::SelfCollision`].
    ///
    /// # Argumente
    /// - `op` (`Arc<dyn Operation>`): Die zu registrierende Operation.
    ///   Wird via `Arc`-Klon geteilt; keine Ownership-Übertragung des inneren Werts.
    ///
    /// # Nebenläufigkeit
    /// Erfordert exklusiven Zugriff (`&mut self`). Für parallele Schreibzugriffe
    /// wrap die Registry in `Arc<RwLock<OperationRegistry>>`.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use harw_operations::registry::OperationRegistry;
    ///
    /// let mut registry = OperationRegistry::new();
    /// // registry.register(Arc::new(my_op));
    /// assert_eq!(registry.len(), 0); // vorher leer
    /// ```
    pub fn register(&mut self, op: Arc<dyn Operation>) {
        self.try_register(op)
            .expect("built-in pack has no collisions; register() panics on collision — use try_register() for fallible registration")
    }

    /// Versucht, eine dynamisch erzeugte Operation kollisionssicher hinzuzufügen.
    ///
    /// # Beschreibung
    /// Performs four deterministic checks before appending `op` to the internal list:
    ///
    /// 1. **Self-collision**: the op's canonical `name` appears in its own `aliases`, or
    ///    the same alias string appears more than once in its `aliases` list.
    /// 2. **Duplicate name**: an existing op already uses the same canonical name
    ///    (case-insensitive).
    /// 3. **Alias collision**: one of the new op's aliases matches either the canonical
    ///    name or any alias of an already-registered op (case-insensitive).
    /// 4. **Web path collision**: one of the new op's `Surface::Web` `(path, method)`
    ///    pairs matches a `Surface::Web` `(path, method)` pair already claimed by an
    ///    existing op (exact string match on `path` — HTTP paths are case-sensitive —
    ///    plus an exact `method` match; the same `path` under a different `method` is
    ///    not a collision). See [`RegistryError::WebPathCollision`] for why this check
    ///    lives here rather than in `#[operation(...)]`.
    ///
    /// If all checks pass, the op is appended in insert order.
    ///
    /// # Arguments
    /// - `op` (`Arc<dyn Operation>`): The operation to register.
    ///
    /// # Returns
    /// - `Ok(())` if registration succeeded.
    /// - `Err(RegistryError)` if any collision was detected (op is NOT appended).
    ///
    /// # Errors
    /// - [`RegistryError::SelfCollision`]: op's name equals one of its aliases, or an alias is repeated.
    /// - [`RegistryError::DuplicateName`]: a different op with the same canonical name exists.
    /// - [`RegistryError::AliasCollision`]: a new alias collides with an existing op's name or alias.
    /// - [`RegistryError::WebPathCollision`]: a new `Surface::Web` `(path, method)` pair
    ///   collides with an existing op's `Surface::Web` `(path, method)` pair.
    ///
    /// # Nebenläufigkeit
    /// Erfordert exklusiven Zugriff (`&mut self`).
    ///
    /// # Beispiel
    /// ```rust
    /// use std::sync::Arc;
    /// use harw_operations::registry::{OperationRegistry, RegistryError};
    ///
    /// let mut registry = OperationRegistry::new();
    /// // registry.try_register(Arc::new(my_op)).expect("no collision");
    /// assert!(registry.is_empty());
    /// ```
    pub fn try_register(&mut self, op: Arc<dyn Operation>) -> Result<(), RegistryError> {
        let meta = op.meta();
        let new_name = meta.name.to_ascii_lowercase();
        let new_aliases: Vec<String> = meta
            .aliases
            .iter()
            .map(|a| a.to_ascii_lowercase())
            .collect();

        // 1. Self-collision: name appears in own aliases, or aliases has duplicates.
        if new_aliases.contains(&new_name) {
            return Err(RegistryError::SelfCollision {
                owner: meta.name.to_owned(),
                reason: format!(
                    "canonical name '{}' also appears in its aliases list",
                    meta.name
                ),
            });
        }
        for (i, a) in new_aliases.iter().enumerate() {
            if new_aliases[..i].contains(a) {
                return Err(RegistryError::SelfCollision {
                    owner: meta.name.to_owned(),
                    reason: format!("alias '{}' is listed more than once", a),
                });
            }
        }

        // Build a flat lookup of (string → owner_name) for all already-registered ops.
        for existing in &self.ops {
            let ex_meta = existing.meta();
            let ex_name = ex_meta.name.to_ascii_lowercase();
            let ex_aliases: Vec<String> = ex_meta
                .aliases
                .iter()
                .map(|a| a.to_ascii_lowercase())
                .collect();

            // 2. Duplicate canonical name.
            if new_name == ex_name {
                return Err(RegistryError::DuplicateName {
                    name: meta.name.to_owned(),
                });
            }

            // 3. Alias collision — new alias vs existing canonical name.
            for alias in &new_aliases {
                if *alias == ex_name {
                    return Err(RegistryError::AliasCollision {
                        alias: alias.clone(),
                        first_owner: ex_meta.name.to_owned(),
                        second_owner: meta.name.to_owned(),
                    });
                }
                // New alias vs existing aliases.
                for ex_alias in &ex_aliases {
                    if alias == ex_alias {
                        return Err(RegistryError::AliasCollision {
                            alias: alias.clone(),
                            first_owner: ex_meta.name.to_owned(),
                            second_owner: meta.name.to_owned(),
                        });
                    }
                }
            }

            // New canonical name vs existing aliases.
            if ex_aliases.contains(&new_name) {
                return Err(RegistryError::AliasCollision {
                    alias: new_name.clone(),
                    first_owner: ex_meta.name.to_owned(),
                    second_owner: meta.name.to_owned(),
                });
            }

            // 4. Web path collision — two operations answering the same
            // `(path, method)` pair are indistinguishable to `harw-web`. The
            // same `path` under a different `method` is a legitimate,
            // distinct route (e.g. `GET /api/x` and `POST /api/x`) and must
            // not collide (F-031). Checked against every already-registered
            // operation's `Surface::Web` entries.
            for new_surface in &meta.surfaces {
                let Surface::Web {
                    path: new_path,
                    method: new_method,
                    ..
                } = new_surface
                else {
                    continue;
                };
                for ex_surface in &ex_meta.surfaces {
                    let Surface::Web {
                        path: ex_path,
                        method: ex_method,
                        ..
                    } = ex_surface
                    else {
                        continue;
                    };
                    if new_path == ex_path && new_method == ex_method {
                        return Err(RegistryError::WebPathCollision {
                            path: (*new_path).to_owned(),
                            method: *new_method,
                            first_owner: ex_meta.name.to_owned(),
                            second_owner: meta.name.to_owned(),
                        });
                    }
                }
            }
        }

        self.ops.push(op);
        Ok(())
    }

    /// Sucht eine Op über den maschinellen `meta().name`.
    ///
    /// # Beschreibung
    /// Lineare Suche in Insert-Reihenfolge. Gibt die erste Operation zurück,
    /// deren [`crate::operation::OperationMeta::name`] exakt mit `name` übereinstimmt.
    ///
    /// # Argumente
    /// - `name` (`&str`): Maschinenlesbarer Name der gesuchten Operation (z. B. `"status"`).
    ///   Groß-/Kleinschreibung wird berücksichtigt (exakter Vergleich via `==`).
    ///
    /// # Rückgabe
    /// - `Some(&Arc<dyn Operation>)`: Erste gefundene Operation mit diesem Namen.
    /// - `None`: Keine Operation mit diesem Namen registriert.
    ///
    /// # Nebenläufigkeit
    /// Erfordert nur einen shared borrow (`&self`); kann nebenläufig aufgerufen werden.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_operations::registry::OperationRegistry;
    ///
    /// let registry = OperationRegistry::new();
    /// assert!(registry.find_by_name("nonexistent").is_none());
    /// ```
    #[must_use]
    pub fn find_by_name(&self, name: &str) -> Option<&Arc<dyn Operation>> {
        self.ops.iter().find(|op| op.meta().name == name)
    }

    /// Sucht eine Op über einen Command-Pfad (z. B. `"/status"`).
    ///
    /// # Beschreibung
    /// Lineare Suche über alle registrierten Operationen. Gibt die erste Operation
    /// zurück, die in ihren [`crate::operation::OperationMeta::surfaces`] eine
    /// [`Surface::Command`]-Variante mit `path == path` deklariert.
    ///
    /// Andere Surface-Varianten (z. B. `Surface::ModelTool`) werden ignoriert.
    /// Falls eine Operation mehrere `Surface::Command`-Einträge hat, genügt
    /// einer mit passendem Pfad.
    ///
    /// # Argumente
    /// - `path` (`&str`): Befehlspfad, z. B. `"/status"` oder `"/session/list"`.
    ///   Groß-/Kleinschreibung wird berücksichtigt (exakter Vergleich via `==`).
    ///
    /// # Rückgabe
    /// - `Some(&Arc<dyn Operation>)`: Erste Operation, die diesen Pfad als `Surface::Command` deklariert.
    /// - `None`: Keine Operation hat diesen Pfad in ihren Surfaces.
    ///
    /// # Nebenläufigkeit
    /// Erfordert nur einen shared borrow (`&self`); kann nebenläufig aufgerufen werden.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_operations::registry::OperationRegistry;
    ///
    /// let registry = OperationRegistry::new();
    /// assert!(registry.find_by_command("/nonexistent").is_none());
    /// ```
    #[must_use]
    pub fn find_by_command(&self, path: &str) -> Option<&Arc<dyn Operation>> {
        self.ops.iter().find(|op| {
            op.meta()
                .surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::Command { path: p, .. } if *p == path))
        })
    }

    /// Iteriert alle registrierten Ops in Insert-Reihenfolge.
    ///
    /// # Beschreibung
    /// Gibt einen Iterator über alle `Arc<dyn Operation>`-Referenzen zurück,
    /// in der Reihenfolge, in der sie via [`Self::register`] oder
    /// [`Self::built_in`] hinzugefügt wurden.
    ///
    /// # Rückgabe
    /// Ein Iterator über `&Arc<dyn Operation>`.
    ///
    /// # Nebenläufigkeit
    /// Erfordert nur einen shared borrow (`&self`); der Iterator ist `Send` wenn
    /// `self` `Send` ist.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::registry::OperationRegistry;
    ///
    /// let registry = OperationRegistry::new();
    /// let count = registry.iter().count();
    /// assert_eq!(count, 0);
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Operation>> {
        self.ops.iter()
    }

    /// Alle Ops, die mind. eine Surface haben, die den Filter `pred` erfüllt.
    ///
    /// # Beschreibung
    /// Filtert alle registrierten Operationen und gibt diejenigen zurück, für
    /// die mindestens eine ihrer deklarierten [`Surface`]-Varianten das Prädikat
    /// `pred` erfüllt. Die Reihenfolge der zurückgegebenen Operationen entspricht
    /// der Insert-Reihenfolge in der Registry.
    ///
    /// Es wird immer die **Op** geliefert, nicht die einzelne Surface.
    ///
    /// # Argumente
    /// - `pred` (`impl Fn(&Surface) -> bool`): Prädikat, das auf jede einzelne
    ///   Surface einer Operation angewendet wird. Eine Op wird eingeschlossen,
    ///   sobald eine ihrer Surfaces `pred` als `true` zurückgibt.
    ///
    /// # Rückgabe
    /// Ein `Vec<&Arc<dyn Operation>>` mit allen Ops, die mindestens eine Surface
    /// haben, für die `pred` gilt. Leer, wenn keine passende Op gefunden wird.
    ///
    /// # Nebenläufigkeit
    /// Erfordert nur einen shared borrow (`&self`).
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_operations::operation::Surface;
    /// use harw_operations::registry::OperationRegistry;
    ///
    /// let registry = OperationRegistry::new();
    /// let readonly_tools = registry.by_surface(|s| {
    ///     matches!(s, Surface::ModelTool { readonly: true, .. })
    /// });
    /// assert!(readonly_tools.is_empty());
    /// ```
    #[must_use]
    pub fn by_surface(&self, pred: impl Fn(&Surface) -> bool) -> Vec<&Arc<dyn Operation>> {
        self.ops
            .iter()
            .filter(|op| op.meta().surfaces.iter().any(&pred))
            .collect()
    }

    /// Anzahl der Ops in der Registry (für Tests / Diagnose).
    ///
    /// # Rückgabe
    /// Die Gesamtanzahl der registrierten Operationen (Built-ins + dynamische).
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::registry::OperationRegistry;
    ///
    /// let registry = OperationRegistry::new();
    /// assert_eq!(registry.len(), 0);
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Gibt `true` zurück, wenn keine Ops registriert sind.
    ///
    /// # Rückgabe
    /// `true` wenn die Registry leer ist, `false` wenn mindestens eine Op registriert ist.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::registry::OperationRegistry;
    ///
    /// let registry = OperationRegistry::new();
    /// assert!(registry.is_empty());
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

impl Default for OperationRegistry {
    /// Erstellt eine leere Registry; entspricht [`OperationRegistry::new`].
    ///
    /// # Beschreibung
    /// Delegiert an [`OperationRegistry::new`]. Enthält weder Built-ins noch
    /// dynamisch registrierte Operationen. Für eine Registry mit Built-ins
    /// verwende explizit [`OperationRegistry::built_in`].
    ///
    /// # Rückgabe
    /// Eine neue, leere `OperationRegistry`.
    ///
    /// # Beispiel
    /// ```rust
    /// use harw_operations::registry::OperationRegistry;
    ///
    /// let r: OperationRegistry = Default::default();
    /// assert!(r.is_empty());
    /// ```
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::{Arc, OnceLock};

    use super::OperationRegistry;
    use crate::error::OpError;
    use crate::operation::{
        ApprovalPolicy, BusyAvailability, CommandVisibility, OpFuture, OpInput, Operation,
        OperationCategory, OperationDomain, OperationMeta, PermissionTier, Surface, WebMethod,
    };
    use crate::test_support::{TestError, TestResult};

    // ── Fixtures ─────────────────────────────────────────────────────────────

    /// Minimale Test-Operation mit konfigurierbarem Namen und Surfaces.
    ///
    /// Für `run` reicht `unimplemented!()` — die Registry ruft `run` nicht auf.
    struct TestOp {
        name: &'static str,
        surfaces: Vec<Surface>,
    }

    impl Operation for TestOp {
        fn meta(&self) -> &OperationMeta {
            // Wir können kein `static OnceLock` pro Instanz anlegen, da der Name
            // zur Laufzeit konfiguriert wird. Deshalb bauen wir die Meta inline
            // und speichern sie in einem Box::leak — für Tests akzeptabel.
            // (In Produktion würden Built-ins OnceLock<OperationMeta> verwenden.)
            Box::leak(Box::new(OperationMeta {
                name: self.name,
                summary: "Test-Operation.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: self.surfaces.clone(),
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            }))
        }

        fn run<'a>(&'a self, _ctx: &'a crate::context::OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async {
                Err(OpError::Execution(
                    "TestOp::run wird in Registry-Tests nicht aufgerufen".to_owned(),
                ))
            })
        }
    }

    /// Erzeugt eine `Arc<dyn Operation>` für eine Op mit gegebenem Namen und
    /// optionalen Surfaces.
    fn make_op(name: &'static str, surfaces: Vec<Surface>) -> Arc<dyn Operation> {
        Arc::new(TestOp { name, surfaces })
    }

    /// Erzeugt eine `Arc<dyn Operation>` ohne Surfaces.
    fn make_plain_op(name: &'static str) -> Arc<dyn Operation> {
        make_op(name, vec![])
    }

    /// Erzeugt eine Op mit einem `Surface::Command`-Pfad.
    fn make_command_op(name: &'static str, path: &'static str) -> Arc<dyn Operation> {
        make_op(
            name,
            vec![Surface::Command {
                path,
                visibility: CommandVisibility::TuiOnly,
            }],
        )
    }

    /// Erzeugt eine Op mit einem readonly `Surface::ModelTool`.
    fn make_readonly_tool_op(name: &'static str) -> Arc<dyn Operation> {
        make_op(
            name,
            vec![Surface::ModelTool {
                readonly: true,
                approval: ApprovalPolicy::None,
            }],
        )
    }

    /// Erzeugt eine Op mit einem nicht-readonly `Surface::ModelTool`.
    fn make_write_tool_op(name: &'static str) -> Arc<dyn Operation> {
        make_op(
            name,
            vec![Surface::ModelTool {
                readonly: false,
                approval: ApprovalPolicy::Always,
            }],
        )
    }

    // ── 1. new() liefert leere Registry ──────────────────────────────────────

    #[test]
    fn test_new_is_empty() {
        let registry = OperationRegistry::new();
        assert!(registry.is_empty(), "neue Registry muss leer sein");
        assert_eq!(registry.len(), 0, "neue Registry muss len() == 0 haben");
    }

    // ── 2. register() fügt Op hinzu; find_by_name findet sie ─────────────────

    #[test]
    fn test_register_and_find_by_name() -> TestResult {
        let mut registry = OperationRegistry::new();
        let op = make_plain_op("status");
        registry.register(Arc::clone(&op));

        assert_eq!(registry.len(), 1);
        let found = registry.find_by_name("status");
        assert!(
            found.is_some(),
            "find_by_name('status') sollte Some zurückgeben"
        );
        assert_eq!(
            found
                .ok_or(TestError::Missing("find_by_name('status')"))?
                .meta()
                .name,
            "status"
        );
        Ok(())
    }

    // ── 3. find_by_name mit unbekanntem Namen → None ──────────────────────────

    #[test]
    fn test_find_by_name_unknown_returns_none() {
        let mut registry = OperationRegistry::new();
        registry.register(make_plain_op("known"));

        let result = registry.find_by_name("unknown");
        assert!(result.is_none(), "unbekannter Name sollte None liefern");
    }

    // ── 4. find_by_command findet Op mit Surface::Command ────────────────────

    #[test]
    fn test_find_by_command_matches_surface_path() -> TestResult {
        let mut registry = OperationRegistry::new();
        registry.register(make_command_op("foo-op", "/foo"));

        let found = registry.find_by_command("/foo");
        assert!(
            found.is_some(),
            "find_by_command('/foo') sollte Some zurückgeben"
        );
        assert_eq!(
            found
                .ok_or(TestError::Missing("find_by_command('/foo')"))?
                .meta()
                .name,
            "foo-op"
        );
        Ok(())
    }

    // ── 5. find_by_command mit unbekanntem Pfad → None ───────────────────────

    #[test]
    fn test_find_by_command_unknown_path_returns_none() {
        let mut registry = OperationRegistry::new();
        registry.register(make_command_op("foo-op", "/foo"));

        let result = registry.find_by_command("/bar");
        assert!(
            result.is_none(),
            "unbekannter Command-Pfad sollte None liefern"
        );
    }

    #[test]
    fn test_find_by_command_empty_registry_returns_none() {
        let registry = OperationRegistry::new();
        assert!(registry.find_by_command("/anything").is_none());
    }

    // ── 6. by_surface filtert korrekt ─────────────────────────────────────────

    #[test]
    fn test_by_surface_filters_readonly_model_tools() {
        let mut registry = OperationRegistry::new();
        registry.register(make_readonly_tool_op("readonly-tool"));
        registry.register(make_write_tool_op("write-tool"));
        registry.register(make_plain_op("no-surface"));

        let results =
            registry.by_surface(|s| matches!(s, Surface::ModelTool { readonly: true, .. }));

        assert_eq!(results.len(), 1, "nur eine Op hat readonly:true ModelTool");
        assert_eq!(results[0].meta().name, "readonly-tool");
    }

    #[test]
    fn test_by_surface_returns_empty_when_no_match() {
        let mut registry = OperationRegistry::new();
        registry.register(make_plain_op("no-surface-op"));

        let results = registry.by_surface(|s| matches!(s, Surface::Command { .. }));
        assert!(
            results.is_empty(),
            "keine Command-Op registriert — Ergebnis muss leer sein"
        );
    }

    #[test]
    fn test_by_surface_op_with_multiple_surfaces_included_once() {
        let op = make_op(
            "multi",
            vec![
                Surface::Command {
                    path: "/multi",
                    visibility: CommandVisibility::ChannelParity,
                },
                Surface::ModelTool {
                    readonly: true,
                    approval: ApprovalPolicy::None,
                },
            ],
        );
        let mut registry = OperationRegistry::new();
        registry.register(op);

        // Suche nach Command-Surface
        let by_cmd = registry.by_surface(|s| matches!(s, Surface::Command { .. }));
        assert_eq!(by_cmd.len(), 1);

        // Suche nach ModelTool-Surface — gleiche Op, trotzdem nur einmal
        let by_tool = registry.by_surface(|s| matches!(s, Surface::ModelTool { .. }));
        assert_eq!(by_tool.len(), 1);
    }

    // ── 7. iter liefert alle Ops in Insert-Reihenfolge ───────────────────────

    #[test]
    fn test_iter_insert_order() {
        let mut registry = OperationRegistry::new();
        let names: Vec<&'static str> = vec!["alpha", "beta", "gamma"];
        for &name in &names {
            registry.register(make_plain_op(name));
        }

        let iterated: Vec<&str> = registry.iter().map(|op| op.meta().name).collect();
        assert_eq!(iterated, names, "iter() muss Insert-Reihenfolge bewahren");
    }

    #[test]
    fn test_iter_empty_registry_gives_empty_iterator() {
        let registry = OperationRegistry::new();
        assert_eq!(registry.iter().count(), 0);
    }

    // ── 8. built_in() läuft ohne Panic ───────────────────────────────────────

    #[test]
    fn test_built_in_does_not_panic() {
        // Anzahl nicht assert'd — hängt von submit!-Deklarationen in der Binary ab.
        let registry = OperationRegistry::built_in();
        let _ = registry.len();
    }

    // ── 9. Default::default() == new() ───────────────────────────────────────

    #[test]
    fn test_default_is_equivalent_to_new() {
        let via_new = OperationRegistry::new();
        let via_default: OperationRegistry = Default::default();

        // Beide müssen leer sein.
        assert!(via_new.is_empty());
        assert!(via_default.is_empty());
        assert_eq!(via_new.len(), via_default.len());
    }

    // ── Weitere Randfälle ─────────────────────────────────────────────────────

    #[test]
    fn test_register_multiple_ops_len_grows() {
        let mut registry = OperationRegistry::new();
        assert_eq!(registry.len(), 0);

        registry.register(make_plain_op("op-1"));
        assert_eq!(registry.len(), 1);

        registry.register(make_plain_op("op-2"));
        assert_eq!(registry.len(), 2);

        registry.register(make_plain_op("op-3"));
        assert_eq!(registry.len(), 3);
    }

    #[test]
    fn test_try_register_returns_err_on_duplicate_names() -> TestResult {
        use super::RegistryError;
        let mut registry = OperationRegistry::new();
        // First registration succeeds.
        registry.register(make_op(
            "dup",
            vec![Surface::Command {
                path: "/first",
                visibility: CommandVisibility::TuiOnly,
            }],
        ));
        // Second registration with same canonical name must fail.
        let result = registry.try_register(make_op(
            "dup",
            vec![Surface::Command {
                path: "/second",
                visibility: CommandVisibility::TuiOnly,
            }],
        ));
        assert!(
            matches!(result, Err(RegistryError::DuplicateName { ref name }) if name == "dup"),
            "expected DuplicateName error for second registration of 'dup', got: {result:?}"
        );
        // Registry must still contain only the original op.
        assert_eq!(registry.len(), 1);
        let found = registry
            .find_by_name("dup")
            .ok_or(TestError::Missing("find_by_name('dup')"))?;
        let has_first = found
            .meta()
            .surfaces
            .iter()
            .any(|s| matches!(s, Surface::Command { path: "/first", .. }));
        assert!(
            has_first,
            "find_by_name should return the first (oldest) op"
        );
        Ok(())
    }

    #[test]
    fn test_try_register_returns_err_on_web_path_collision() {
        use super::RegistryError;
        let mut registry = OperationRegistry::new();
        registry.register(make_op(
            "status",
            vec![Surface::Web {
                path: "/api/status",
                method: WebMethod::Get,
                approval: ApprovalPolicy::None,
            }],
        ));
        let result = registry.try_register(make_op(
            "other",
            vec![Surface::Web {
                path: "/api/status",
                method: WebMethod::Get,
                approval: ApprovalPolicy::None,
            }],
        ));
        assert!(
            matches!(
                result,
                Err(RegistryError::WebPathCollision { ref path, method, ref first_owner, ref second_owner })
                    if path == "/api/status" && method == WebMethod::Get
                        && first_owner == "status" && second_owner == "other"
            ),
            "expected WebPathCollision for two ops on '/api/status', got: {result:?}"
        );
        // The colliding op must not have been inserted.
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_try_register_allows_distinct_web_paths() {
        let mut registry = OperationRegistry::new();
        registry.register(make_op(
            "status",
            vec![Surface::Web {
                path: "/api/status",
                method: WebMethod::Get,
                approval: ApprovalPolicy::None,
            }],
        ));
        let result = registry.try_register(make_op(
            "ps",
            vec![Surface::Web {
                path: "/api/ps",
                method: WebMethod::Get,
                approval: ApprovalPolicy::None,
            }],
        ));
        assert!(result.is_ok(), "distinct web paths must not collide");
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn test_try_register_allows_same_path_with_different_methods() {
        // F-031: `GET /api/session` and `POST /api/session` are two distinct,
        // legitimate routes — the same `path` under a different `method` must
        // not be treated as a collision.
        let mut registry = OperationRegistry::new();
        registry.register(make_op(
            "session-read",
            vec![Surface::Web {
                path: "/api/session",
                method: WebMethod::Get,
                approval: ApprovalPolicy::None,
            }],
        ));
        let result = registry.try_register(make_op(
            "session-write",
            vec![Surface::Web {
                path: "/api/session",
                method: WebMethod::Post,
                approval: ApprovalPolicy::Always,
            }],
        ));
        assert!(
            result.is_ok(),
            "same path with distinct methods must not collide, got: {result:?}"
        );
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn test_web_path_collision_display_mentions_both_owners_and_path() {
        use super::RegistryError;
        let err = RegistryError::WebPathCollision {
            path: "/api/status".to_owned(),
            method: WebMethod::Get,
            first_owner: "status".to_owned(),
            second_owner: "other".to_owned(),
        };
        let rendered = err.to_string();
        assert!(rendered.contains("/api/status"));
        assert!(rendered.contains("status"));
        assert!(rendered.contains("other"));
    }

    #[test]
    fn test_try_register_returns_err_on_alias_collision() {
        use super::RegistryError;
        let mut registry = OperationRegistry::new();

        // Op with name "model" that declares alias "m".
        struct ModelOp;
        impl Operation for ModelOp {
            fn meta(&self) -> &OperationMeta {
                static META: OnceLock<OperationMeta> = OnceLock::new();
                META.get_or_init(|| OperationMeta {
                    name: "model",
                    summary: "test",
                    domain: OperationDomain::Misc,
                    permission: PermissionTier::Observer,
                    surfaces: vec![],
                    aliases: &["m"],
                    category: OperationCategory::Misc,
                    args_schema: None,
                    output_schema: None,
                    busy: BusyAvailability::DeferredUntilTurnEnd,
                })
            }
            fn run<'a>(
                &'a self,
                _ctx: &'a crate::context::OpContext,
                _input: OpInput,
            ) -> OpFuture<'a> {
                Box::pin(async {
                    Err(OpError::Execution(
                        "run() wird in Registry-Tests nicht aufgerufen".to_owned(),
                    ))
                })
            }
        }

        registry.register(Arc::new(ModelOp));

        // A second op claiming the same alias "m" must fail.
        struct MiniOp;
        impl Operation for MiniOp {
            fn meta(&self) -> &OperationMeta {
                static META: OnceLock<OperationMeta> = OnceLock::new();
                META.get_or_init(|| OperationMeta {
                    name: "mini",
                    summary: "test",
                    domain: OperationDomain::Misc,
                    permission: PermissionTier::Observer,
                    surfaces: vec![],
                    aliases: &["m"],
                    category: OperationCategory::Misc,
                    args_schema: None,
                    output_schema: None,
                    busy: BusyAvailability::DeferredUntilTurnEnd,
                })
            }
            fn run<'a>(
                &'a self,
                _ctx: &'a crate::context::OpContext,
                _input: OpInput,
            ) -> OpFuture<'a> {
                Box::pin(async {
                    Err(OpError::Execution(
                        "run() wird in Registry-Tests nicht aufgerufen".to_owned(),
                    ))
                })
            }
        }

        let result = registry.try_register(Arc::new(MiniOp));
        assert!(
            matches!(result, Err(RegistryError::AliasCollision { ref alias, .. }) if alias == "m"),
            "expected AliasCollision on alias 'm', got: {result:?}"
        );
    }

    #[test]
    fn test_try_register_returns_err_on_self_collision() {
        use super::RegistryError;
        let mut registry = OperationRegistry::new();

        // Op whose canonical name also appears in its own aliases list.
        struct BadOp;
        impl Operation for BadOp {
            fn meta(&self) -> &OperationMeta {
                static META: OnceLock<OperationMeta> = OnceLock::new();
                META.get_or_init(|| OperationMeta {
                    name: "bad",
                    summary: "test",
                    domain: OperationDomain::Misc,
                    permission: PermissionTier::Observer,
                    surfaces: vec![],
                    aliases: &["bad"],
                    category: OperationCategory::Misc,
                    args_schema: None,
                    output_schema: None,
                    busy: BusyAvailability::DeferredUntilTurnEnd,
                })
            }
            fn run<'a>(
                &'a self,
                _ctx: &'a crate::context::OpContext,
                _input: OpInput,
            ) -> OpFuture<'a> {
                Box::pin(async {
                    Err(OpError::Execution(
                        "run() wird in Registry-Tests nicht aufgerufen".to_owned(),
                    ))
                })
            }
        }

        let result = registry.try_register(Arc::new(BadOp));
        assert!(
            matches!(result, Err(RegistryError::SelfCollision { ref owner, .. }) if owner == "bad"),
            "expected SelfCollision for op 'bad', got: {result:?}"
        );
    }

    #[test]
    fn test_find_by_command_only_matches_exact_path() {
        let mut registry = OperationRegistry::new();
        registry.register(make_command_op("foo", "/foo"));

        assert!(registry.find_by_command("/foo").is_some());
        assert!(registry.find_by_command("/fo").is_none());
        assert!(registry.find_by_command("/foo/bar").is_none());
        assert!(registry.find_by_command("foo").is_none()); // kein führender Slash
    }

    #[test]
    fn test_is_empty_false_after_register() {
        let mut registry = OperationRegistry::new();
        assert!(registry.is_empty());
        registry.register(make_plain_op("x"));
        assert!(!registry.is_empty());
    }

    // Statischer OnceLock-Test: Prüft, dass eine Op mit OnceLock<OperationMeta>
    // stabil dieselbe Referenz liefert (wie Built-in-Ops es tun würden).
    #[test]
    fn test_once_lock_op_meta_stable_reference() -> TestResult {
        struct StableOp;

        impl Operation for StableOp {
            fn meta(&self) -> &OperationMeta {
                static META: OnceLock<OperationMeta> = OnceLock::new();
                META.get_or_init(|| OperationMeta {
                    name: "stable",
                    summary: "Stabile Referenz.",
                    domain: OperationDomain::Misc,
                    permission: PermissionTier::Observer,
                    surfaces: vec![],
                    aliases: &[],
                    category: OperationCategory::Misc,
                    args_schema: None,
                    output_schema: None,
                    busy: BusyAvailability::DeferredUntilTurnEnd,
                })
            }

            fn run<'a>(
                &'a self,
                _ctx: &'a crate::context::OpContext,
                _input: OpInput,
            ) -> OpFuture<'a> {
                Box::pin(async {
                    Err(OpError::Execution(
                        "run() wird in Registry-Tests nicht aufgerufen".to_owned(),
                    ))
                })
            }
        }

        let mut registry = OperationRegistry::new();
        registry.register(Arc::new(StableOp));

        let found = registry
            .find_by_name("stable")
            .ok_or(TestError::Missing("find_by_name('stable')"))?;
        let ptr1 = found.meta() as *const OperationMeta;
        let ptr2 = found.meta() as *const OperationMeta;
        assert_eq!(ptr1, ptr2, "OnceLock-Meta muss dieselbe Adresse liefern");
        Ok(())
    }
}
