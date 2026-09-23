//! `warden_actions!`-Expansion — die Aktionstaxonomie des Warden-Durchsetzers.
//!
//! Spec: AW5-01-Brief (`warden_actions!`). Vierter und letzter Makro-Knoten
//! nach AW0-02 (`field!`, `metrics!`, `Redact`), AW1-02 (`#[traced]`) und
//! AW2-06 (`#[derive(SensorSource)]`).
//!
//! # Zweck
//! Eine Warden-Aktion ist heute an fünf Stellen von Hand zu pflegen: das
//! Wire-Format zum Durchsetzer, die Vorschlagsform für einen Agenten, das
//! Tool-Schema, mit dem ein Modell die Aktion ausdrücken darf, der
//! Audit-Eintrag, und die Zulässigkeitsmatrix, die festlegt, ab welcher
//! Eskalationsstufe die Aktion überhaupt greifen darf. Diese fünf Stellen
//! driften auseinander, sobald eine sechste Aktion hinzukommt und nur vier
//! der fünf Stellen aktualisiert werden. `warden_actions!` erzeugt alle fünf
//! aus einer einzigen Deklaration.
//!
//! # Die zwei erzwungenen Compile-Fehler
//! Eine Aktion ohne `admissible_from: [...]` ist eine Lücke in der
//! Eskalationsleiter: sie lässt sich vorschlagen, aber niemand hat
//! festgelegt, ab welcher Stufe sie zulässig ist — das fällt sonst erst im
//! Betrieb auf, wenn der Durchsetzer sie entweder immer oder nie durchlässt.
//! Eine Aktion ohne `audit = "..."` ist ein Eingriff ohne Spur. Beide Felder
//! sind deshalb in [`ActionEntry::parse`] Pflichtfelder ohne Fallback; ihr
//! Fehlen ist ein `syn::Error`, nicht eine Standardannahme.
//!
//! # Warum keine erzeugte Variante freien Text trägt
//! Jedes Feld einer Aktion ist ein getypter Wert aus einer geschlossenen
//! Positivliste ([`ALLOWED_ID_TYPES`], [`ALLOWED_INTEGER_TYPES`], `bool`,
//! sowie `Vec<T>` über einem dieser Typen — siehe Abschnitt „Warum `Vec<T>`"
//! unten). `String` ist explizit **kein** Mitglied dieser Liste, obwohl es
//! syntaktisch ein Feldtyp wie jeder andere wäre. Der Grund: diese Aktionen
//! werden von einem Modell vorgeschlagen ([`ProposedAction`]). Ein Feld, in
//! das ein Modell freien Text schreiben kann, ist genau der Weg, auf dem
//! eine Modellausgabe zu einem Befehl wird — `RunCommand { cmd: String }`
//! ließe sich nicht mehr gegen eine Zulässigkeitsregel prüfen, weil der
//! eigentliche Inhalt des Eingriffs erst zur Laufzeit im Textfeld steckt.
//! `KillProcessTree { cgroup: CgroupId }` bleibt dagegen vollständig
//! prüfbar: jeder mögliche Wert ist vorher bekannt und typgebunden.
//! [`classify_field_type`] erzwingt das zur Compile-Zeit, nicht erst in
//! einer Laufzeitvalidierung — ein Feldtyp außerhalb der Liste (`String`
//! eingeschlossen) ist ein `syn::Error`, der die konkrete Positivliste
//! nennt.
//!
//! # Warum `Vec<T>` zusätzlich zum Skalar
//! Der Erbauer von `#[derive(SensorSource)]` (AW2-06) meldete, dass sein
//! Attributmodell — ein Glob, ein Skalar — den Fall einer zweiten Datenspalte
//! nicht trug, weil die Grammatik keine zweite Quelle benennen konnte. Die
//! Lehre daraus für dieses Makro: der Durchsetzer braucht absehbar Aktionen,
//! die mehrere gleichartige Ziele auf einmal betreffen (mehrere cgroups in
//! einem Zug einfrieren, mehrere Prozessbäume in einem Zug beenden) — ein
//! einzelner Skalar pro Feld trägt diesen Fall nicht. [`classify_field_type`]
//! akzeptiert deshalb zusätzlich `Vec<T>`, sofern `T` selbst in der
//! Positivliste steht (kein `Vec<Vec<T>>`, kein `Vec<String>`): eine Liste
//! typisierter Werte ist weiterhin vollständig geschlossen — jedes Element
//! ist einzeln typgebunden, es gibt keine Stelle, an der freier Text
//! hineinrutschen könnte. `Option<T>` ist bewusst **nicht** unterstützt:
//! ein fehlender Wert lässt sich als eigene Aktionsvariante ausdrücken, ohne
//! die Zweideutigkeit „absichtlich weggelassen oder vergessen" in den
//! Audit-Eintrag zu tragen, in dem wegen `#[serde(deny_unknown_fields)]`
//! ohnehin jedes Feld explizit vorhanden sein muss.
//!
//! # `AuthorizationProof` — Entscheidung
//! Jede Wire-Nachricht muss einen Autorisierungsbeleg tragen; der Typ dafür
//! gehört nach `harw-dod-warden-proto` (AW5-02, zum Zeitpunkt dieses Knotens
//! noch nicht gebaut). Dieses Makro erzeugt ihn deshalb nicht selbst, sondern
//! nimmt seinen Pfad als Pflichtparameter der Deklaration entgegen
//! (`authorization_proof = <Pfad>;`, siehe [`WardenActionsInput::parse`]) —
//! dieselbe Form, die `#[derive(SensorSource)]` bereits für
//! `#[sensor(error = "pfad::zu::LocalError")]` verwendet (siehe
//! `sensor_source.rs`). Die Alternative — die Wire-Nachricht additiv um ein
//! später ergänztes Beleg-Feld zu erweitern — wurde verworfen: sie würde
//! bedeuten, dass die heute erzeugte [`WardenActionRequest`] zunächst *ohne*
//! Beleg auskommt und damit genau die Eigenschaft nicht hat, die der Auftrag
//! ausdrücklich verlangt („jede Wire-Nachricht trägt einen Beleg"). Ein
//! Pfadparameter erzwingt die Eigenschaft sofort, ohne dass dieses Crate auf
//! `harw-dod-warden-proto` angewiesen wäre — der erzeugte Code referenziert
//! den Pfad rein textuell, wie überall in diesem Crate (siehe Moduldoku von
//! `lib.rs`).
//!
//! # `tool_schema` — abwählbar durch Weglassen, nicht durch Einschalten
//! Der AW5-02-Knoten (`harw-dod-warden-proto`) hat gemeldet: `warden_actions!`
//! erzeugte bislang bedingungslos `ProposedAction::tool_schema() ->
//! harw_tools::ToolSpec`, ohne `#[cfg]`-Fluchtweg. Jede Crate, die das Makro
//! aufrief, musste deshalb zwingend gegen `harw-tools` übersetzen —
//! unabhängig davon, ob `tool_schema()` je aufgerufen wird. Für die Kette
//! `harw-warden` → `harw-dod-warden` → `harw-dod-warden-proto` bedeutet das
//! acht zusätzliche Crates, die ohne `tool_schema()` gar nicht nötig wären:
//! `harw-sandbox`, `async-trait`, `ipnet`, `tracing`, `tracing-attributes`,
//! `tracing-core`, `valuable`, sowie `harw-tools` selbst. Das Gate
//! „Warden-Abhängigkeitszahl" erlaubt für diese Kette höchstens zwölf Crates
//! transitiv — acht davon allein für ein Tool-Schema, das der Durchsetzer
//! selbst nie anbietet (`tool_schema()` beschreibt, was ein *Modell*
//! vorschlagen darf; der Warden ist die Durchsetzungsseite, nicht die
//! Modellseite, dieselbe Trennung wie in K11 „Typen ins Typ-Crate, Funktionen
//! ins Funktions-Crate"), sprengt dieses Budget ohne jeden Nutzen für diesen
//! Konsumenten. Der Knoten hat die Typen deshalb von Hand geschrieben statt
//! das Makro zu benutzen — das Makro hatte damit **null Konsumenten**, genau
//! das Muster, gegen das dieses Programm angetreten ist.
//!
//! Die Abhilfe ist ein zusätzlicher, optionaler Deklarationsschlüssel,
//! `tool_schema = <Pfad>;`, in derselben Form wie das bereits vorhandene
//! `authorization_proof = <Pfad>;` (siehe [`WardenActionsInput::parse`]).
//! **Fehlt die Zeile, erzeugt das Makro kein `tool_schema()`-Impl, und der
//! erzeugte Code nennt `harw_tools` an keiner Stelle** — weder als Rückgabetyp
//! noch in der internen Schema-Konstruktion; die übrigen vier Erzeugnisse
//! (Wire-Enum, Vorschlagsform, Wire-Nachricht, Audit-Typ,
//! Zulässigkeitsmatrix — s.u.) bleiben unverändert. Steht die Zeile da, ist
//! die Ausgabe unverändert zu der vor diesem Knoten.
//!
//! **Warum Weglassen und nicht Einschalten:** ein Schlüssel, den man setzen
//! müsste, um eine Abhängigkeit *loszuwerden*, wird vergessen — und das
//! Vergessen ist hier die teure Richtung: eine vergessene Abwahl bedeutet acht
//! überflüssige Crates in einer Kette mit einem harten Zwölfer-Gate, die erst
//! beim nächsten Gate-Lauf auffällt, nicht beim Schreiben der Deklaration.
//! Wer das Schema will, weiß das beim Schreiben der Deklaration und schreibt
//! eine Zeile mehr; wer es nicht braucht, muss nichts tun, um nichts zu
//! bekommen.
//!
//! **Warum kein Cargo-Feature:** Features sind additiv und werden über den
//! gesamten Abhängigkeitsgraphen einer Kompilierung vereinigt — aktiviert
//! irgendeine andere Crate im selben Baum (auch eine, die mit
//! `harw-dod-warden-proto` nichts zu tun hat) dasselbe Feature an
//! `harw-macros`, bekäme `harw-dod-warden-proto` `harw-tools` über diesen
//! Umweg doch wieder, ohne dass in seiner eigenen `Cargo.toml` oder seinem
//! eigenen `warden_actions!`-Aufruf irgendetwas darauf hindeutet. Das
//! Zwölfer-Gate soll aber verlässlich lokal ablesbar sein: eine Abhängigkeit,
//! die je nach *fremder* Feature-Aktivierung woanders im Baum erscheint oder
//! verschwindet, lässt sich nicht mehr an der eigenen Deklaration allein
//! prüfen. Ein Deklarationsschlüssel wirkt dagegen ausschließlich innerhalb
//! der eigenen `warden_actions!`-Aufrufstelle und ist deshalb von jeder
//! anderen Crate im Baum unabhängig.
//!
//! # Was pro Deklaration erzeugt wird
//! Siehe [`expand_warden_actions`] für die vollständige Liste; die
//! Kurzfassung: [`WardenAction`] (Wire-Enum, intern getaggt, kebab-case,
//! `deny_unknown_fields`), `ProposedAction` (identische Form ohne Beleg —
//! das, was ein Agent vorschlagen darf), `WardenActionRequest` (Wire-Enum
//! plus Beleg — das, was tatsächlich über den Socket geht),
//! `WardenActionAudit` (Aktion plus deklarierter Audit-Name) und
//! `EscalationStage` plus `WardenAction::is_admissible_from` (die
//! Zulässigkeitsmatrix). Diese fünf Erzeugnisse — genauer: die ersten vier
//! plus die Zulässigkeitsmatrix, siehe Abschnitt „`tool_schema`" oben für das
//! sechste, optionale Erzeugnis — entstehen unabhängig von `tool_schema`.
//! Alle Namen sind fest vergeben — eine Crate deklariert `warden_actions!`
//! deshalb genau einmal je Modul, ebenso wie `metrics!` genau einmal je Modul
//! ein `ALL` erzeugt (siehe `metrics.rs`-Moduldoku).
//!
//! # Kebab-case-Umrechnung — bekannte Grenze
//! [`to_kebab_case`] bildet `FreezeCgroup` auf `freeze-cgroup` ab, indem es
//! vor jedem Großbuchstaben außer dem ersten einen Bindestrich einfügt und
//! den Buchstaben klein schreibt. Das deckt sich mit `serde`s
//! `rename_all = "kebab-case"` für gewöhnliche `PascalCase`-Bezeichner ohne
//! Akronyme. Ein Aktionsname mit Akronym (`HTTPProxyBlock`) würde anders
//! aufgeteilt als `serde`s eigene Regel — dieses Makro erwartet deshalb
//! Aktionsnamen ohne Akronyme; ein Verstoß fiele als abweichender
//! `"action"`-Tag-Wert im generierten Tool-Schema auf, nicht als
//! Compile-Fehler.

use std::collections::HashSet;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, Ident, LitStr, Path, Token, Type};

/// Die `harw-types`-ID-Newtypes, die als Feldtyp einer Warden-Aktion erlaubt
/// sind (siehe Moduldoku, Abschnitt „Warum keine erzeugte Variante freien
/// Text trägt"). Rein textueller Namensabgleich — `harw-macros` hängt nicht
/// produktiv von `harw-types` ab (siehe Moduldoku von `lib.rs`), daher ist
/// dies eine bewusst gepflegte Kopie der Namen aus `harw-types/src/ids.rs`.
/// Eine neue ID in `harw-types` erfordert eine bewusste, sichtbare Ergänzung
/// hier.
const ALLOWED_ID_TYPES: &[&str] = &[
    "ActionId",
    "BaselineId",
    "CgroupId",
    "ChannelId",
    "FindingId",
    "HostId",
    "ItemId",
    "PeerId",
    "SensorId",
    "SessionId",
    "TenantId",
    "ThreadId",
    "ThreadRef",
    "ToolCallId",
    "TurnId",
    "WorkId",
    "WorkspaceId",
];

/// Die Ganzzahltypen, die als Feldtyp einer Warden-Aktion erlaubt sind.
const ALLOWED_INTEGER_TYPES: &[&str] = &[
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
];

/// Der JSON-Schema-Tag-Feldname des intern getaggten Wire-Enums.
const TAG_FIELD: &str = "kind";

/// Die Art eines einzelnen Skalarwerts aus der Positivliste.
///
/// Trägt den ursprünglichen Typnamen nur zu Diagnosezwecken; die Erzeugung
/// selbst verwendet stets den vom Aufrufer geschriebenen `syn::Type`
/// unverändert (siehe [`ActionField`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScalarKind {
    /// Eine `harw-types`-ID, z. B. `CgroupId`.
    Id,
    /// Ein Ganzzahltyp, z. B. `u64`.
    Integer,
    /// `bool`.
    Bool,
}

/// Die Art eines Feldtyps: entweder ein einzelner erlaubter Skalar oder eine
/// `Vec<T>` darüber (siehe Moduldoku, Abschnitt „Warum `Vec<T>`").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Scalar(ScalarKind),
    List(ScalarKind),
}

/// Ordnet einen einzelnen (nicht-generischen) Pfadtyp einer [`ScalarKind`]
/// zu, sofern er in einer der beiden Positivlisten steht.
///
/// # Returns
/// `None`, wenn `ty` kein einfacher Pfadtyp ohne generische Argumente ist,
/// oder wenn sein letztes Pfadsegment in keiner Positivliste vorkommt.
fn scalar_kind_of(ty: &Type) -> Option<ScalarKind> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if !matches!(segment.arguments, syn::PathArguments::None) {
        return None;
    }
    let name = segment.ident.to_string();
    if ALLOWED_ID_TYPES.contains(&name.as_str()) {
        return Some(ScalarKind::Id);
    }
    if ALLOWED_INTEGER_TYPES.contains(&name.as_str()) {
        return Some(ScalarKind::Integer);
    }
    if name == "bool" {
        return Some(ScalarKind::Bool);
    }
    None
}

/// Baut die Fehlermeldung für einen abgelehnten Feldtyp, mit einem
/// zusätzlichen Hinweis, wenn der abgelehnte Typ `String`/`str` ist (siehe
/// Moduldoku, Abschnitt „Warum keine erzeugte Variante freien Text trägt").
fn field_type_error(ty: &Type) -> syn::Error {
    let rendered = quote!(#ty).to_string();
    let is_stringy = matches!(
        crate::schema::type_last_ident(ty).as_deref(),
        Some("String" | "str")
    );
    let hint = if is_stringy {
        " – insbesondere `String` ist nicht erlaubt: ein Feld, in das ein Modell freien Text \
         schreiben kann, ist der Weg, auf dem eine Modellausgabe zu einem Befehl wird;"
    } else {
        ";"
    };
    syn::Error::new_spanned(
        ty,
        format!(
            "Feldtyp `{rendered}` ist nicht in der Positivliste zulässiger Werttypen{hint} \
             erlaubt sind: {ids} (aus harw-types), die Ganzzahltypen {ints}, `bool`, sowie \
             `Vec<T>` für jeden dieser Typen",
            ids = ALLOWED_ID_TYPES.join(", "),
            ints = ALLOWED_INTEGER_TYPES.join(", "),
        ),
    )
}

/// Klassifiziert einen Feldtyp gegen die Positivliste (siehe Moduldoku).
///
/// # Errors
/// `syn::Error`, wenn `ty` weder ein erlaubter Skalar noch eine `Vec<T>`
/// über einem erlaubten Skalar ist. Die Meldung nennt die vollständige
/// Positivliste und, bei `String`/`str`, den Grund für den Ausschluss.
///
/// # Design-doc reference
/// AW5-01-Brief, Abschnitt „Die harte Auflage: keine freien Argumente".
pub(crate) fn classify_field_type(ty: &Type) -> syn::Result<FieldKind> {
    if let Some(inner) = crate::schema::vec_inner(ty) {
        let scalar = scalar_kind_of(inner).ok_or_else(|| field_type_error(inner))?;
        return Ok(FieldKind::List(scalar));
    }
    let scalar = scalar_kind_of(ty).ok_or_else(|| field_type_error(ty))?;
    Ok(FieldKind::Scalar(scalar))
}

/// Ein einzelnes Datenfeld einer Aktion (`name: Type,`), bereits gegen die
/// Positivliste geprüft.
struct ActionField {
    ident: Ident,
    ty: Type,
    kind: FieldKind,
}

/// Eine vollständig geparste Aktionsdeklaration.
///
/// Siehe Moduldoku, Abschnitt „Die zwei erzwungenen Compile-Fehler": sowohl
/// `admissible_from` als auch `audit` sind Pflichtfelder ohne Fallback.
struct ActionEntry {
    /// `///`-Doc-Kommentare vor dem Aktionsnamen; werden auf die erzeugten
    /// Enum-Varianten übertragen und als Tool-Schema-Beschreibung verwendet.
    docs: Vec<Attribute>,
    ident: Ident,
    fields: Vec<ActionField>,
    admissible_from: Vec<Ident>,
    audit: LitStr,
}

impl Parse for ActionEntry {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let docs = input.call(Attribute::parse_outer)?;
        let ident: Ident = input.parse()?;

        let content;
        syn::braced!(content in input);

        let mut fields: Vec<ActionField> = Vec::new();
        let mut admissible_from: Option<Vec<Ident>> = None;
        let mut audit: Option<LitStr> = None;

        loop {
            if content.is_empty() {
                break;
            }
            let key: Ident = content.parse()?;

            if key == "admissible_from" {
                if admissible_from.is_some() {
                    return Err(syn::Error::new_spanned(
                        &key,
                        "`admissible_from` doppelt angegeben",
                    ));
                }
                content.parse::<Token![:]>()?;
                let list_content;
                syn::bracketed!(list_content in content);
                let list = list_content.parse_terminated(Ident::parse, Token![,])?;
                admissible_from = Some(list.into_iter().collect());
            } else if key == "audit" {
                if audit.is_some() {
                    return Err(syn::Error::new_spanned(&key, "`audit` doppelt angegeben"));
                }
                content.parse::<Token![=]>()?;
                let lit: LitStr = content.parse()?;
                if lit.value().trim().is_empty() {
                    return Err(syn::Error::new_spanned(
                        &lit,
                        "`audit`-Name darf nicht leer sein",
                    ));
                }
                audit = Some(lit);
            } else {
                content.parse::<Token![:]>()?;
                let ty: Type = content.parse()?;
                let kind = classify_field_type(&ty)?;
                fields.push(ActionField {
                    ident: key,
                    ty,
                    kind,
                });
            }

            if content.is_empty() {
                break;
            }
            content.parse::<Token![,]>()?;
        }

        let admissible_from = admissible_from.ok_or_else(|| {
            syn::Error::new_spanned(
                &ident,
                format!(
                    "Aktion `{ident}` hat kein `admissible_from: [...]`; eine Aktion ohne \
                     Zulässigkeitsregel ist eine Lücke in der Eskalationsleiter"
                ),
            )
        })?;
        let audit = audit.ok_or_else(|| {
            syn::Error::new_spanned(
                &ident,
                format!(
                    "Aktion `{ident}` hat kein `audit = \"...\"`; eine Aktion ohne Audit-Namen \
                     ist ein Eingriff ohne Spur"
                ),
            )
        })?;

        Ok(ActionEntry {
            docs,
            ident,
            fields,
            admissible_from,
            audit,
        })
    }
}

/// Die vollständige `warden_actions! { ... }`-Deklaration.
///
/// # Grammatik
/// ```ignore
/// authorization_proof = <Pfad>;
/// tool_schema = <Pfad>;   // optional, siehe Moduldoku Abschnitt „tool_schema"
///
/// $(#[doc = "..."])*
/// <ActionIdent> {
///     <feld>: <Type>,
///     admissible_from: [<Ident>, ...],
///     audit = "<literal>",
/// }
/// ...
/// ```
///
/// `authorization_proof = <Pfad>;` steht genau einmal am Anfang (siehe
/// Moduldoku, Abschnitt „`AuthorizationProof` — Entscheidung"). Direkt danach
/// darf höchstens einmal `tool_schema = <Pfad>;` folgen (siehe Moduldoku,
/// Abschnitt „`tool_schema` — abwählbar durch Weglassen, nicht durch
/// Einschalten"); fehlt sie, entsteht kein `tool_schema()`-Impl und der
/// erzeugte Code nennt `harw_tools` an keiner Stelle. Danach folgt
/// mindestens eine Aktion.
struct WardenActionsInput {
    authorization_proof: Path,
    /// Der über `tool_schema = <Pfad>;` angegebene Rückgabetyp von
    /// `ProposedAction::tool_schema()`, oder `None`, wenn die Zeile fehlt —
    /// dann wird weder das Impl noch irgendein Verweis auf `harw_tools`
    /// erzeugt (siehe Moduldoku).
    tool_schema: Option<Path>,
    actions: Vec<ActionEntry>,
}

impl Parse for WardenActionsInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        if input.is_empty() {
            return Err(syn::Error::new(
                Span::call_site(),
                "warden_actions! darf keine leere Deklaration sein – erwartet wird mindestens \
                 `authorization_proof = <Pfad>;` und eine Aktion",
            ));
        }

        let keyword: Ident = input.parse()?;
        if keyword != "authorization_proof" {
            return Err(syn::Error::new_spanned(
                &keyword,
                "erstes Element einer warden_actions!-Deklaration muss \
                 `authorization_proof = <Pfad>;` sein",
            ));
        }
        input.parse::<Token![=]>()?;
        let authorization_proof: Path = input.parse()?;
        input.parse::<Token![;]>()?;

        // Optionaler `tool_schema = <Pfad>;`-Schlüssel, siehe Moduldoku,
        // Abschnitt „tool_schema" — abwählbar durch Weglassen, deshalb hier
        // ein `Option`, kein Fallback-Pfad.
        let mut tool_schema: Option<Path> = None;
        loop {
            if !input.peek(Ident) {
                break;
            }
            let lookahead = input.fork();
            let candidate: Ident = lookahead.parse()?;
            if candidate != "tool_schema" {
                break;
            }
            let key: Ident = input.parse()?;
            if tool_schema.is_some() {
                return Err(syn::Error::new_spanned(
                    &key,
                    "`tool_schema` doppelt angegeben",
                ));
            }
            input.parse::<Token![=]>()?;
            let path: Path = input.parse()?;
            input.parse::<Token![;]>()?;
            tool_schema = Some(path);
        }

        if input.is_empty() {
            return Err(syn::Error::new(
                Span::call_site(),
                "warden_actions! erfordert mindestens eine Aktion nach \
                 `authorization_proof = ...;`",
            ));
        }

        let mut actions = Vec::new();
        while !input.is_empty() {
            actions.push(input.parse::<ActionEntry>()?);
        }

        Ok(WardenActionsInput {
            authorization_proof,
            tool_schema,
            actions,
        })
    }
}

/// Bildet einen `PascalCase`-Bezeichner auf `kebab-case` ab, kompatibel zu
/// `serde`s `rename_all = "kebab-case"` für Akronym-freie Namen (siehe
/// Moduldoku, Abschnitt „Kebab-case-Umrechnung — bekannte Grenze").
fn to_kebab_case(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 4);
    for (index, ch) in ident.chars().enumerate() {
        if ch.is_uppercase() {
            if index != 0 {
                out.push('-');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Erzeugt die `::harw_tools::JsonSchema`-Tokens für einen einzelnen Skalar.
fn scalar_json_schema(kind: &ScalarKind) -> TokenStream {
    match kind {
        ScalarKind::Id => quote! {
            ::harw_tools::JsonSchema {
                schema_type: ::core::option::Option::Some(::harw_tools::JsonSchemaType::String),
                ..::core::default::Default::default()
            }
        },
        ScalarKind::Integer => quote! {
            ::harw_tools::JsonSchema {
                schema_type: ::core::option::Option::Some(::harw_tools::JsonSchemaType::Integer),
                ..::core::default::Default::default()
            }
        },
        ScalarKind::Bool => quote! {
            ::harw_tools::JsonSchema {
                schema_type: ::core::option::Option::Some(::harw_tools::JsonSchemaType::Boolean),
                ..::core::default::Default::default()
            }
        },
    }
}

/// Erzeugt die `::harw_tools::JsonSchema`-Tokens für ein ganzes Feld
/// (Skalar oder `Vec<Skalar>`).
fn field_json_schema(kind: &FieldKind) -> TokenStream {
    match kind {
        FieldKind::Scalar(scalar) => scalar_json_schema(scalar),
        FieldKind::List(scalar) => {
            let item = scalar_json_schema(scalar);
            quote! {
                ::harw_tools::JsonSchema {
                    schema_type: ::core::option::Option::Some(::harw_tools::JsonSchemaType::Array),
                    items: ::core::option::Option::Some(::std::boxed::Box::new(#item)),
                    ..::core::default::Default::default()
                }
            }
        }
    }
}

/// Erzeugt das strikte `::harw_tools::JsonSchema`-Objekt einer einzelnen
/// Aktionsvariante: Tag-Feld (fester Wert) plus jedes Datenfeld, alle
/// erforderlich, `additionalProperties: false` — damit ein Modell diese
/// Aktion nur in genau dieser Form ausdrücken kann.
fn action_variant_schema(action: &ActionEntry, tag_value: &str) -> TokenStream {
    let mut property_inserts: Vec<TokenStream> = Vec::with_capacity(action.fields.len() + 1);
    let mut required: Vec<String> = vec![TAG_FIELD.to_string()];

    property_inserts.push(quote! {
        __props.insert(#TAG_FIELD.to_string(), ::harw_tools::JsonSchema {
            schema_type: ::core::option::Option::Some(::harw_tools::JsonSchemaType::String),
            enum_values: ::core::option::Option::Some(::std::vec![
                ::harw_tools::serde_json::json!(#tag_value)
            ]),
            ..::core::default::Default::default()
        });
    });

    for field in &action.fields {
        let name = field.ident.to_string();
        let schema = field_json_schema(&field.kind);
        property_inserts.push(quote! { __props.insert(#name.to_string(), #schema); });
        required.push(name);
    }

    let description_tokens = match crate::schema::doc_string(&action.docs) {
        Some(doc) => quote! { description: ::core::option::Option::Some(#doc.to_string()), },
        None => TokenStream::new(),
    };

    quote! {
        ::harw_tools::JsonSchema {
            schema_type: ::core::option::Option::Some(::harw_tools::JsonSchemaType::Object),
            #description_tokens
            properties: ::core::option::Option::Some({
                let mut __props: ::std::collections::BTreeMap<
                    ::std::string::String,
                    ::harw_tools::JsonSchema,
                > = ::std::collections::BTreeMap::new();
                #(#property_inserts)*
                __props
            }),
            required: ::core::option::Option::Some(::std::vec![#(#required.to_string()),*]),
            additional_properties: ::core::option::Option::Some(::std::boxed::Box::new(
                ::harw_tools::AdditionalProperties::Bool(false),
            )),
            ..::core::default::Default::default()
        }
    }
}

/// Erzeugt die gemeinsame Varianten-Kopfzeile (Docs, Name, Felder), die sowohl
/// [`WardenAction`] als auch `ProposedAction` identisch verwenden — beide
/// haben dieselbe Datenform, nur [`WardenActionRequest`] trägt zusätzlich
/// den Autorisierungsbeleg (siehe Moduldoku).
fn action_variant_definition(action: &ActionEntry) -> TokenStream {
    let docs = &action.docs;
    let ident = &action.ident;
    let field_defs: Vec<TokenStream> = action
        .fields
        .iter()
        .map(|field| {
            let field_ident = &field.ident;
            let field_ty = &field.ty;
            quote! { #field_ident: #field_ty }
        })
        .collect();

    quote! {
        #(#docs)*
        #ident { #(#field_defs),* }
    }
}

/// Expander für das `warden_actions!`-Makro.
///
/// # Description
/// Siehe die Moduldoku für die vollständige Begründung. Parst die
/// Deklaration (siehe [`WardenActionsInput`]), prüft doppelte Aktions- und
/// Feldnamen, und erzeugt:
/// 1. `WardenAction` — das intern getaggte Wire-Enum.
/// 2. `ProposedAction` — dieselbe Form ohne Autorisierungsbeleg, plus
///    `impl From<ProposedAction> for WardenAction`.
/// 3. `WardenActionRequest` — `WardenAction` plus Autorisierungsbeleg, die
///    tatsächliche Wire-Nachricht.
/// 4. `WardenActionAudit` — Aktion plus deklarierter Audit-Name.
/// 5. `EscalationStage` plus `WardenAction::is_admissible_from` — die
///    Zulässigkeitsmatrix.
///
/// **Nur wenn die Deklaration `tool_schema = <Pfad>;` enthält**, zusätzlich:
/// 6. `impl ProposedAction { pub fn tool_schema() -> <Pfad> }` — siehe
///    Moduldoku, Abschnitt „`tool_schema` — abwählbar durch Weglassen, nicht
///    durch Einschalten". Fehlt der Schlüssel, wird dieses Impl nicht
///    erzeugt, und **kein** Token der Ausgabe nennt `harw_tools`.
///
/// # Errors
/// - leere Deklaration, oder keine Aktion nach `authorization_proof = ...;`
///   → `syn::Error`.
/// - erstes Element ist nicht `authorization_proof = <Pfad>;` → `syn::Error`.
/// - `tool_schema` mehr als einmal angegeben → `syn::Error`.
/// - eine Aktion ohne `admissible_from: [...]` oder ohne `audit = "..."` →
///   `syn::Error` (siehe Moduldoku, Abschnitt „Die zwei erzwungenen
///   Compile-Fehler").
/// - ein Feldtyp außerhalb der Positivliste, `String` eingeschlossen →
///   `syn::Error` (siehe [`classify_field_type`]).
/// - zwei Aktionen mit demselben Namen, oder zwei Felder derselben Aktion
///   mit demselben Namen → `syn::Error`.
///
/// # Design-doc reference
/// AW5-01-Brief (`warden_actions!`); AW5-02-Befund („`tool_schema`
/// abwählbar").
pub(crate) fn expand_warden_actions(input: TokenStream) -> syn::Result<TokenStream> {
    let parsed: WardenActionsInput = syn::parse2(input)?;

    let mut seen_actions: HashSet<String> = HashSet::new();
    for action in &parsed.actions {
        let action_name = action.ident.to_string();
        if !seen_actions.insert(action_name.clone()) {
            return Err(syn::Error::new_spanned(
                &action.ident,
                format!(
                    "Aktion `{action_name}` ist in dieser Deklaration bereits doppelt vergeben"
                ),
            ));
        }

        let mut seen_fields: HashSet<String> = HashSet::new();
        for field in &action.fields {
            let field_name = field.ident.to_string();
            if !seen_fields.insert(field_name.clone()) {
                return Err(syn::Error::new_spanned(
                    &field.ident,
                    format!(
                        "Feld `{field_name}` ist in Aktion `{action_name}` bereits doppelt \
                         vergeben"
                    ),
                ));
            }
        }
    }

    // Eskalationsstufen in Erstauftrittsreihenfolge über alle Aktionen hinweg
    // sammeln (siehe Moduldoku, Abschnitt „Was pro Deklaration erzeugt wird").
    let mut stage_order: Vec<Ident> = Vec::new();
    let mut stage_seen: HashSet<String> = HashSet::new();
    for action in &parsed.actions {
        for stage in &action.admissible_from {
            if stage_seen.insert(stage.to_string()) {
                stage_order.push(stage.clone());
            }
        }
    }

    let variant_definitions: Vec<TokenStream> = parsed
        .actions
        .iter()
        .map(action_variant_definition)
        .collect();

    let from_arms: Vec<TokenStream> = parsed
        .actions
        .iter()
        .map(|action| {
            let ident = &action.ident;
            let field_idents: Vec<&Ident> = action.fields.iter().map(|f| &f.ident).collect();
            quote! {
                ProposedAction::#ident { #(#field_idents),* } =>
                    WardenAction::#ident { #(#field_idents),* },
            }
        })
        .collect();

    let audit_arms: Vec<TokenStream> = parsed
        .actions
        .iter()
        .map(|action| {
            let ident = &action.ident;
            let audit = &action.audit;
            quote! {
                WardenAction::#ident { .. } => #audit.to_string(),
            }
        })
        .collect();

    let admissible_arms: Vec<TokenStream> = parsed
        .actions
        .iter()
        .map(|action| {
            let ident = &action.ident;
            let body = if action.admissible_from.is_empty() {
                quote! { false }
            } else {
                let stages = &action.admissible_from;
                quote! { matches!(stage, #(EscalationStage::#stages)|*) }
            };
            quote! {
                WardenAction::#ident { .. } => #body,
            }
        })
        .collect();

    // Nur berechnet und nur in die Ausgabe eingefügt, wenn `tool_schema =
    // <Pfad>;` angegeben ist — sonst nennt die Ausgabe `harw_tools` an
    // keiner Stelle (siehe Moduldoku, Abschnitt „tool_schema").
    let tool_schema_impl = match &parsed.tool_schema {
        Some(tool_schema_path) => {
            let schema_variants: Vec<TokenStream> = parsed
                .actions
                .iter()
                .map(|action| {
                    let tag_value = to_kebab_case(&action.ident.to_string());
                    action_variant_schema(action, &tag_value)
                })
                .collect();

            quote! {
                impl ProposedAction {
                    /// Das strikte Tool-Schema: die einzige Form, in der ein Modell
                    /// eine dieser Aktionen ausdrücken kann (`additionalProperties:
                    /// false`, alle Felder erforderlich, `strict: true`). Nur
                    /// erzeugt, weil die Deklaration `tool_schema = <Pfad>;`
                    /// angegeben hat (siehe `warden_actions.rs`-Moduldoku).
                    pub fn tool_schema() -> #tool_schema_path {
                        let mut __warden_action_variants: ::std::vec::Vec<::harw_tools::JsonSchema> =
                            ::std::vec::Vec::new();
                        #(__warden_action_variants.push(#schema_variants);)*

                        ::harw_tools::ToolSpec::Function(::harw_tools::FunctionToolSpec {
                            name: ::harw_tools::ToolName::new("warden_action"),
                            description: "Schlägt eine Warden-Durchsetzungsaktion vor; die Ausführung \
                                          erfordert eine nachträgliche Autorisierung."
                                .to_string(),
                            parameters: ::harw_tools::JsonSchema {
                                any_of: ::core::option::Option::Some(__warden_action_variants),
                                ..::core::default::Default::default()
                            },
                            strict: true,
                        })
                    }
                }
            }
        }
        None => TokenStream::new(),
    };

    let authorization_proof = &parsed.authorization_proof;

    Ok(quote! {
        /// Wire-Enum: die von `warden_actions!` erzeugte Aktionstaxonomie, wie
        /// sie (eingebettet in `WardenActionRequest`) über den Socket zum
        /// Durchsetzer geht. Intern getaggt (`kind`), kebab-case,
        /// `deny_unknown_fields`.
        #[derive(Debug, Clone, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(tag = #TAG_FIELD, rename_all = "kebab-case", deny_unknown_fields)]
        pub enum WardenAction {
            #(#variant_definitions),*
        }

        /// Was ein Agent vorschlagen kann, ohne Autorisierung. Strukturell
        /// identisch zu `WardenAction`, aber ein eigener Typ: Code, das eine
        /// `ProposedAction` hält, kann sie nicht versehentlich unautorisiert
        /// an den Durchsetzer senden — dafür ist immer erst
        /// `WardenActionRequest::new` mit einem Autorisierungsbeleg nötig.
        #[derive(Debug, Clone, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(tag = #TAG_FIELD, rename_all = "kebab-case", deny_unknown_fields)]
        pub enum ProposedAction {
            #(#variant_definitions),*
        }

        impl ::core::convert::From<ProposedAction> for WardenAction {
            fn from(value: ProposedAction) -> Self {
                match value {
                    #(#from_arms)*
                }
            }
        }

        /// Die tatsächliche Wire-Nachricht: eine Aktion plus der
        /// Autorisierungsbeleg, ohne den der Durchsetzer sie ablehnt.
        #[derive(Debug, Clone, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct WardenActionRequest {
            pub action: WardenAction,
            pub proof: #authorization_proof,
        }

        impl WardenActionRequest {
            /// Verpackt eine vorgeschlagene Aktion mit ihrem Autorisierungsbeleg
            /// zur tatsächlichen Wire-Nachricht.
            pub fn new(action: ProposedAction, proof: #authorization_proof) -> Self {
                Self {
                    action: ::core::convert::From::from(action),
                    proof,
                }
            }
        }

        /// Der Audit-Typ: was in die Audit-Kette geschrieben wird — die
        /// ausgeführte Aktion zusammen mit ihrem deklarierten Audit-Namen
        /// (`audit = "..."`).
        #[derive(Debug, Clone, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        pub struct WardenActionAudit {
            pub audit_name: ::std::string::String,
            pub action: WardenAction,
        }

        impl WardenActionAudit {
            /// Baut den Audit-Eintrag für eine bereits ausgeführte Aktion.
            pub fn for_action(action: WardenAction) -> Self {
                let audit_name: ::std::string::String = match &action {
                    #(#audit_arms)*
                };
                Self { audit_name, action }
            }
        }

        /// Zulässigkeitsmatrix-Skelett: die Eskalationsstufen, die in dieser
        /// Deklaration in mindestens einem `admissible_from` genannt sind.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum EscalationStage {
            #(#stage_order),*
        }

        impl WardenAction {
            /// Befragt die Zulässigkeitsmatrix: darf diese Aktion ab `stage`
            /// durchgesetzt werden?
            pub fn is_admissible_from(&self, stage: EscalationStage) -> bool {
                match self {
                    #(#admissible_arms)*
                }
            }
        }

        #tool_schema_impl
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn parse_ok(tokens: TokenStream) -> TestResult<String> {
        Ok(expand_warden_actions(tokens)
            .map_err(ctx("declaration must expand"))?
            .to_string())
    }

    fn parse_err(tokens: TokenStream) -> TestResult<String> {
        let Err(error) = expand_warden_actions(tokens) else {
            return Err(TestError::Unexpected(
                "declaration must be rejected".to_owned(),
            ));
        };
        Ok(error.to_string())
    }

    fn valid_declaration() -> TokenStream {
        quote! {
            authorization_proof = test_support::FakeProof;
            tool_schema = harw_tools::ToolSpec;

            /// Friert eine cgroup ein.
            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                admissible_from: [RuleTriggered, Escalated],
                audit = "warden.freeze_cgroup",
            }
            /// Beendet einen Prozessbaum.
            KillProcessTree {
                cgroup: harw_types::CgroupId,
                admissible_from: [Escalated],
                audit = "warden.kill_process_tree",
            }
        }
    }

    // -- classify_field_type -------------------------------------------------

    #[test]
    fn classify_field_type_accepts_every_allowed_id_type() -> TestResult {
        for name in ALLOWED_ID_TYPES {
            let ty: Type = syn::parse_str(&format!("harw_types::{name}")).map_err(ctx("parses"))?;
            assert!(
                matches!(
                    classify_field_type(&ty),
                    Ok(FieldKind::Scalar(ScalarKind::Id))
                ),
                "must accept {name}"
            );
        }
        Ok(())
    }

    #[test]
    fn classify_field_type_accepts_every_allowed_integer_type_and_bool() -> TestResult {
        for name in ALLOWED_INTEGER_TYPES {
            let ty: Type = syn::parse_str(name).map_err(ctx("parses"))?;
            assert!(
                matches!(
                    classify_field_type(&ty),
                    Ok(FieldKind::Scalar(ScalarKind::Integer))
                ),
                "must accept {name}"
            );
        }
        let ty: Type = syn::parse_quote!(bool);
        assert!(matches!(
            classify_field_type(&ty),
            Ok(FieldKind::Scalar(ScalarKind::Bool))
        ));
        Ok(())
    }

    #[test]
    fn classify_field_type_accepts_vec_of_allowed_scalar() {
        let ty: Type = syn::parse_quote!(Vec<harw_types::CgroupId>);
        assert!(matches!(
            classify_field_type(&ty),
            Ok(FieldKind::List(ScalarKind::Id))
        ));
    }

    #[test]
    fn classify_field_type_rejects_string() -> TestResult {
        let ty: Type = syn::parse_quote!(String);
        let Err(err) = classify_field_type(&ty) else {
            return Err(TestError::Unexpected("String must be rejected".to_owned()));
        };
        assert!(
            err.to_string()
                .contains("insbesondere `String` ist nicht erlaubt")
        );
        Ok(())
    }

    #[test]
    fn classify_field_type_rejects_vec_of_string() -> TestResult {
        let ty: Type = syn::parse_quote!(Vec<String>);
        let Err(err) = classify_field_type(&ty) else {
            return Err(TestError::Unexpected(
                "Vec<String> must be rejected".to_owned(),
            ));
        };
        assert!(err.to_string().contains("Positivliste"));
        Ok(())
    }

    #[test]
    fn classify_field_type_rejects_type_outside_allowlist() -> TestResult {
        let ty: Type = syn::parse_quote!(f64);
        let Err(err) = classify_field_type(&ty) else {
            return Err(TestError::Unexpected("f64 must be rejected".to_owned()));
        };
        assert!(err.to_string().contains("Positivliste"));
        assert!(!err.to_string().contains("insbesondere `String`"));
        Ok(())
    }

    // -- to_kebab_case --------------------------------------------------------

    #[test]
    fn to_kebab_case_converts_pascal_case_action_names() {
        assert_eq!(to_kebab_case("FreezeCgroup"), "freeze-cgroup");
        assert_eq!(to_kebab_case("KillProcessTree"), "kill-process-tree");
    }

    // -- expand_warden_actions: Fehlerfälle ------------------------------------

    #[test]
    fn expand_rejects_empty_declaration() -> TestResult {
        let err = parse_err(quote! {})?;
        assert!(err.contains("darf keine leere Deklaration sein"));
        Ok(())
    }

    #[test]
    fn expand_rejects_declaration_without_any_action() -> TestResult {
        let err = parse_err(quote! {
            authorization_proof = test_support::FakeProof;
        })?;
        assert!(err.contains("erfordert mindestens eine Aktion"));
        Ok(())
    }

    #[test]
    fn expand_rejects_missing_authorization_proof_keyword() -> TestResult {
        let err = parse_err(quote! {
            bogus = test_support::FakeProof;
        })?;
        assert!(err.contains("authorization_proof = <Pfad>;"));
        Ok(())
    }

    #[test]
    fn expand_rejects_action_without_admissible_from() -> TestResult {
        let err = parse_err(quote! {
            authorization_proof = test_support::FakeProof;

            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                audit = "warden.freeze_cgroup",
            }
        })?;
        assert!(err.contains("kein `admissible_from: [...]`"));
        assert!(err.contains("Lücke in der Eskalationsleiter"));
        Ok(())
    }

    #[test]
    fn expand_rejects_action_without_audit() -> TestResult {
        let err = parse_err(quote! {
            authorization_proof = test_support::FakeProof;

            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                admissible_from: [Escalated],
            }
        })?;
        assert!(err.contains("kein `audit = \"...\"`"));
        assert!(err.contains("Eingriff ohne Spur"));
        Ok(())
    }

    #[test]
    fn expand_rejects_empty_audit_name() -> TestResult {
        let err = parse_err(quote! {
            authorization_proof = test_support::FakeProof;

            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                admissible_from: [Escalated],
                audit = "",
            }
        })?;
        assert!(err.contains("darf nicht leer sein"));
        Ok(())
    }

    #[test]
    fn expand_rejects_duplicate_admissible_from() -> TestResult {
        let err = parse_err(quote! {
            authorization_proof = test_support::FakeProof;

            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                admissible_from: [Escalated],
                admissible_from: [RuleTriggered],
                audit = "warden.freeze_cgroup",
            }
        })?;
        assert!(err.contains("`admissible_from` doppelt angegeben"));
        Ok(())
    }

    #[test]
    fn expand_rejects_duplicate_audit() -> TestResult {
        let err = parse_err(quote! {
            authorization_proof = test_support::FakeProof;

            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                admissible_from: [Escalated],
                audit = "warden.freeze_cgroup",
                audit = "warden.freeze_cgroup_again",
            }
        })?;
        assert!(err.contains("`audit` doppelt angegeben"));
        Ok(())
    }

    #[test]
    fn expand_rejects_duplicate_tool_schema() -> TestResult {
        let err = parse_err(quote! {
            authorization_proof = test_support::FakeProof;
            tool_schema = harw_tools::ToolSpec;
            tool_schema = harw_tools::ToolSpec;

            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                admissible_from: [Escalated],
                audit = "warden.freeze_cgroup",
            }
        })?;
        assert!(err.contains("`tool_schema` doppelt angegeben"));
        Ok(())
    }

    #[test]
    fn expand_rejects_string_field() -> TestResult {
        let err = parse_err(quote! {
            authorization_proof = test_support::FakeProof;

            RunCommand {
                cmd: String,
                admissible_from: [Escalated],
                audit = "warden.run_command",
            }
        })?;
        assert!(err.contains("insbesondere `String` ist nicht erlaubt"));
        Ok(())
    }

    #[test]
    fn expand_rejects_duplicate_action_name() -> TestResult {
        let err = parse_err(quote! {
            authorization_proof = test_support::FakeProof;

            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                admissible_from: [Escalated],
                audit = "warden.freeze_cgroup",
            }
            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                admissible_from: [RuleTriggered],
                audit = "warden.freeze_cgroup_again",
            }
        })?;
        assert!(err.contains("bereits doppelt vergeben"));
        Ok(())
    }

    #[test]
    fn expand_rejects_duplicate_field_name() -> TestResult {
        let err = parse_err(quote! {
            authorization_proof = test_support::FakeProof;

            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                cgroup: harw_types::CgroupId,
                admissible_from: [Escalated],
                audit = "warden.freeze_cgroup",
            }
        })?;
        assert!(err.contains("Feld `cgroup`"));
        assert!(err.contains("bereits doppelt vergeben"));
        Ok(())
    }

    // -- expand_warden_actions: Positivfall -------------------------------------

    #[test]
    fn expand_accepts_valid_declaration_and_generates_all_five_items() -> TestResult {
        let tokens = parse_ok(valid_declaration())?;

        assert!(tokens.contains("pub enum WardenAction"));
        assert!(tokens.contains("pub enum ProposedAction"));
        assert!(tokens.contains("pub struct WardenActionRequest"));
        assert!(tokens.contains("pub struct WardenActionAudit"));
        assert!(tokens.contains("pub enum EscalationStage"));
        assert!(tokens.contains("is_admissible_from"));
        assert!(tokens.contains("tool_schema"));
        assert!(tokens.contains("kebab-case"));
        assert!(tokens.contains("deny_unknown_fields"));
        assert!(tokens.contains("From < ProposedAction > for WardenAction"));
        assert!(tokens.contains("test_support :: FakeProof"));
        // Die kebab-case-Namen (`freeze-cgroup`) erzeugt **serde zur
        // Laufzeit** aus `rename_all`; sie stehen nirgends als Literal in den
        // Token. Sie hier zu suchen prüfte serdes Verhalten, nicht das des
        // Makros. Was das Makro steuert, ist das Attribut selbst -- und das
        // ist oben bereits zugesichert (`kebab-case`). Geprüft wird deshalb
        // der Variantenname, den das Makro wirklich emittiert.
        assert!(tokens.contains("FreezeCgroup"));
        assert!(tokens.contains("strict : true"));
        Ok(())
    }

    #[test]
    fn expand_accepts_vec_field_for_multi_target_actions() -> TestResult {
        let tokens = parse_ok(quote! {
            authorization_proof = test_support::FakeProof;
            tool_schema = harw_tools::ToolSpec;

            /// Isoliert mehrere cgroups auf einmal.
            IsolateCgroups {
                cgroups: Vec<harw_types::CgroupId>,
                admissible_from: [Escalated],
                audit = "warden.isolate_cgroups",
            }
        })?;
        assert!(tokens.contains("cgroups : Vec < harw_types :: CgroupId >"));
        assert!(tokens.contains("JsonSchemaType :: Array"));
        Ok(())
    }

    #[test]
    fn expand_allows_empty_admissible_from_and_generates_unreachable_false() -> TestResult {
        let tokens = parse_ok(quote! {
            authorization_proof = test_support::FakeProof;

            NeverAdmissible {
                cgroup: harw_types::CgroupId,
                admissible_from: [],
                audit = "warden.never_admissible",
            }
        })?;
        assert!(tokens.contains("NeverAdmissible { .. } => false"));
        Ok(())
    }

    // -- expand_warden_actions: `tool_schema` abwählbar --------------------------

    /// Das ist der Test, um den es in diesem Knoten geht: ohne
    /// `tool_schema = <Pfad>;` nennt die Ausgabe `harw_tools` an keiner
    /// Stelle, während die übrigen vier Erzeugnisse unverändert entstehen
    /// (siehe Moduldoku, Abschnitt „tool_schema").
    #[test]
    fn expand_without_tool_schema_never_mentions_harw_tools_but_keeps_the_other_four_products()
    -> TestResult {
        let tokens = parse_ok(quote! {
            authorization_proof = test_support::FakeProof;

            /// Friert eine cgroup ein.
            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                admissible_from: [RuleTriggered, Escalated],
                audit = "warden.freeze_cgroup",
            }
            /// Beendet einen Prozessbaum.
            KillProcessTree {
                cgroup: harw_types::CgroupId,
                admissible_from: [Escalated],
                audit = "warden.kill_process_tree",
            }
        })?;

        assert!(
            !tokens.contains("harw_tools"),
            "no token may name harw_tools: {tokens}"
        );
        assert!(!tokens.contains("tool_schema"));

        // Die vier übrigen Erzeugnisse plus die Zulässigkeitsmatrix bleiben
        // unverändert.
        assert!(tokens.contains("pub enum WardenAction"));
        assert!(tokens.contains("pub enum ProposedAction"));
        assert!(tokens.contains("pub struct WardenActionRequest"));
        assert!(tokens.contains("pub struct WardenActionAudit"));
        assert!(tokens.contains("pub enum EscalationStage"));
        assert!(tokens.contains("is_admissible_from"));
        assert!(tokens.contains("kebab-case"));
        assert!(tokens.contains("deny_unknown_fields"));
        assert!(tokens.contains("From < ProposedAction > for WardenAction"));
        assert!(tokens.contains("test_support :: FakeProof"));
        // `freeze-cgroup` erzeugt serde zur Laufzeit aus `rename_all`; der
        // Variantenname ist das, was dieses Makro emittiert.
        assert!(tokens.contains("FreezeCgroup"));
        // `kill-process-tree` erzeugt serde zur Laufzeit aus `rename_all`; der
        // Variantenname ist das, was dieses Makro emittiert.
        assert!(tokens.contains("KillProcessTree"));
        Ok(())
    }

    /// Die Zulässigkeitsmatrix und die vier übrigen Erzeugnisse sind mit und
    /// ohne `tool_schema` byteidentisch — nur das `tool_schema()`-Impl wird
    /// angehängt, sonst ändert sich nichts an der Ausgabe.
    #[test]
    fn expand_with_and_without_tool_schema_share_identical_output_for_the_other_five_items()
    -> TestResult {
        let without = parse_ok(quote! {
            authorization_proof = test_support::FakeProof;

            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                admissible_from: [Escalated],
                audit = "warden.freeze_cgroup",
            }
        })?;
        let with = parse_ok(quote! {
            authorization_proof = test_support::FakeProof;
            tool_schema = harw_tools::ToolSpec;

            FreezeCgroup {
                cgroup: harw_types::CgroupId,
                admissible_from: [Escalated],
                audit = "warden.freeze_cgroup",
            }
        })?;

        let marker = "impl ProposedAction";
        let split_at = with
            .find(marker)
            .ok_or(TestError::Missing("tool_schema impl must be appended"))?;
        // `trim_end`: `TokenStream::to_string()` hängt je nach Endstück ein
        // Leerzeichen an. Der Unterschied ist ein Formatierungsartefakt, kein
        // Unterschied in der Ausgabe -- ihn zu vergleichen prüfte `quote`,
        // nicht dieses Makro.
        assert_eq!(
            without.trim_end(),
            with[..split_at].trim_end(),
            "the five items independent of tool_schema must be identical with and without it"
        );
        assert!(with.contains(marker));
        assert!(!without.contains(marker));
        Ok(())
    }
}
