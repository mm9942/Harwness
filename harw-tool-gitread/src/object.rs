//! Git-Objekte: Art, Rohdaten und Parser für Commits, Bäume und Tags.
//!
//! Alle Parser sind tolerant gegenüber Rauschen und geben bei kaputten Daten
//! einen Fehler statt zu paniken. Pfade und Namen sind rohe Bytes (Git
//! erzwingt kein UTF-8); [`lossy`] macht daraus Anzeigetext.

use crate::oid::{OID_LEN, Oid};

/// Art eines Git-Objekts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Commit.
    Commit,
    /// Baum (Verzeichnis).
    Tree,
    /// Blob (Dateiinhalt).
    Blob,
    /// Annotiertes Tag.
    Tag,
}

impl Kind {
    /// Name wie im Objektkopf.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Tree => "tree",
            Self::Blob => "blob",
            Self::Tag => "tag",
        }
    }

    /// Aus dem Namen im Objektkopf.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "commit" => Some(Self::Commit),
            "tree" => Some(Self::Tree),
            "blob" => Some(Self::Blob),
            "tag" => Some(Self::Tag),
            _ => None,
        }
    }
}

/// Ein gelesenes Objekt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    /// Art.
    pub kind: Kind,
    /// Rohdaten (ohne Objektkopf).
    pub data: Vec<u8>,
}

/// Anzeigetext für rohe Bytes (ungültiges UTF-8 wird ersetzt).
#[must_use]
pub fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Autor-/Committer-Zeile.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Signature {
    /// Name.
    pub name: String,
    /// E-Mail.
    pub email: String,
    /// Unix-Sekunden.
    pub when: i64,
    /// Zeitzonenversatz in Minuten.
    pub tz_minutes: i32,
}

/// Ein geparster Commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Wurzelbaum.
    pub tree: Oid,
    /// Eltern in Reihenfolge.
    pub parents: Vec<Oid>,
    /// Autor.
    pub author: Signature,
    /// Committer.
    pub committer: Signature,
    /// Commit-Nachricht (ohne Kopf).
    pub message: String,
}

impl Commit {
    /// Erste Zeile der Nachricht.
    #[must_use]
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or("")
    }

    /// Nachricht ohne Betreffzeile und führende Leerzeilen.
    #[must_use]
    pub fn body(&self) -> &str {
        let rest = self.message.split_once('\n').map_or("", |(_, rest)| rest);
        rest.trim_start_matches('\n')
    }
}

fn parse_signature(text: &str) -> Signature {
    // `Name <mail> 1700000000 +0200`
    let Some(open) = text.rfind('<') else {
        return Signature {
            name: text.trim().to_owned(),
            ..Signature::default()
        };
    };
    let Some(close) = text[open..].find('>').map(|i| open + i) else {
        return Signature {
            name: text[..open].trim().to_owned(),
            ..Signature::default()
        };
    };
    let name = text[..open].trim().to_owned();
    let email = text[open + 1..close].to_owned();
    let mut rest = text[close + 1..].split_whitespace();
    let when = rest.next().and_then(|w| w.parse().ok()).unwrap_or(0);
    let tz_minutes = rest.next().and_then(parse_tz).unwrap_or(0);
    Signature {
        name,
        email,
        when,
        tz_minutes,
    }
}

fn parse_tz(text: &str) -> Option<i32> {
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hours: i32 = digits[..2].parse().ok()?;
    let minutes: i32 = digits[2..].parse().ok()?;
    Some(sign * (hours * 60 + minutes))
}

/// Parst einen Commit.
///
/// # Errors
/// Meldung, wenn `tree` fehlt oder ungültig ist.
pub fn parse_commit(data: &[u8]) -> Result<Commit, String> {
    let text = String::from_utf8_lossy(data);
    let (headers, message) = match text.split_once("\n\n") {
        Some((h, m)) => (h, m.to_owned()),
        None => (text.trim_end_matches('\n'), String::new()),
    };
    let mut tree = None;
    let mut parents = Vec::new();
    let mut author = Signature::default();
    let mut committer = Signature::default();
    for line in headers.lines() {
        // Fortsetzungszeilen (gpgsig, mergetag) beginnen mit einem Leerzeichen.
        if line.starts_with(' ') {
            continue;
        }
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "tree" => tree = Oid::from_hex(value.trim()),
            "parent" => {
                if let Some(parent) = Oid::from_hex(value.trim()) {
                    parents.push(parent);
                }
            }
            "author" => author = parse_signature(value),
            "committer" => committer = parse_signature(value),
            _ => {}
        }
    }
    Ok(Commit {
        tree: tree.ok_or_else(|| "commit has no valid tree line".to_owned())?,
        parents,
        author,
        committer,
        message,
    })
}

/// Ein Baumeintrag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Dateimodus (`0o100644`, `0o100755`, `0o120000`, `0o040000`, `0o160000`).
    pub mode: u32,
    /// Name (rohe Bytes).
    pub name: Vec<u8>,
    /// Objekt-ID.
    pub oid: Oid,
}

impl TreeEntry {
    /// `true` für Verzeichnisse.
    #[must_use]
    pub fn is_tree(&self) -> bool {
        self.mode & 0o170_000 == 0o040_000
    }

    /// `true` für Gitlinks (Submodule).
    #[must_use]
    pub fn is_gitlink(&self) -> bool {
        self.mode & 0o170_000 == 0o160_000
    }
}

/// Parst einen Baum.
///
/// # Errors
/// Meldung bei abgeschnittenen oder ungültigen Einträgen.
pub fn parse_tree(data: &[u8]) -> Result<Vec<TreeEntry>, String> {
    let mut entries = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let space = data[pos..]
            .iter()
            .position(|b| *b == b' ')
            .ok_or("tree entry without mode")?
            + pos;
        let mode_text =
            std::str::from_utf8(&data[pos..space]).map_err(|_| "tree entry mode is not text")?;
        let mode = u32::from_str_radix(mode_text, 8).map_err(|_| "tree entry mode is not octal")?;
        let nul = data[space + 1..]
            .iter()
            .position(|b| *b == 0)
            .ok_or("tree entry without NUL")?
            + space
            + 1;
        let name = data[space + 1..nul].to_vec();
        let oid_end = nul + 1 + OID_LEN;
        let oid = data
            .get(nul + 1..oid_end)
            .and_then(Oid::from_bytes)
            .ok_or("tree entry with truncated id")?;
        entries.push(TreeEntry { mode, name, oid });
        pos = oid_end;
    }
    Ok(entries)
}

/// Ein annotiertes Tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Zielobjekt.
    pub target: Oid,
    /// Art des Ziels.
    pub target_kind: Kind,
    /// Tag-Name.
    pub name: String,
    /// Tagger, falls vorhanden.
    pub tagger: Option<Signature>,
    /// Nachricht.
    pub message: String,
}

/// Parst ein Tag-Objekt.
///
/// # Errors
/// Meldung, wenn `object`/`type` fehlen.
pub fn parse_tag(data: &[u8]) -> Result<Tag, String> {
    let text = String::from_utf8_lossy(data);
    let (headers, message) = text
        .split_once("\n\n")
        .map_or((text.as_ref(), String::new()), |(h, m)| (h, m.to_owned()));
    let mut target = None;
    let mut kind = None;
    let mut name = String::new();
    let mut tagger = None;
    for line in headers.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "object" => target = Oid::from_hex(value.trim()),
            "type" => kind = Kind::from_name(value.trim()),
            "tag" => name = value.trim().to_owned(),
            "tagger" => tagger = Some(parse_signature(value)),
            _ => {}
        }
    }
    Ok(Tag {
        target: target.ok_or("tag has no valid object line")?,
        target_kind: kind.ok_or("tag has no valid type line")?,
        name,
        tagger,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    const TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    const PARENT: &str = "ce013625030ba8dba906f756967f9e9ca394464a";

    #[test]
    fn parses_commits_with_signatures_parents_and_gpgsig() -> TestResult {
        let text = format!(
            "tree {TREE}\nparent {PARENT}\nparent {PARENT}\nauthor Ada Lovelace <ada@example.com> 1700000000 +0130\ncommitter Bob <b@x> 1700000100 -0500\ngpgsig -----BEGIN PGP SIGNATURE-----\n \n abc\n -----END PGP SIGNATURE-----\n\nSubject line\n\nBody text\nmore\n"
        );
        let commit = parse_commit(text.as_bytes()).map_err(TestError::Unexpected)?;
        assert_eq!(commit.tree.hex(), TREE);
        assert_eq!(commit.parents.len(), 2);
        assert_eq!(
            commit.author,
            Signature {
                name: "Ada Lovelace".into(),
                email: "ada@example.com".into(),
                when: 1_700_000_000,
                tz_minutes: 90
            }
        );
        assert_eq!(commit.committer.tz_minutes, -300);
        assert_eq!(commit.subject(), "Subject line");
        assert_eq!(commit.body(), "Body text\nmore\n");
        Ok(())
    }

    #[test]
    fn commit_edge_cases() -> TestResult {
        assert!(parse_commit(b"").is_err());
        assert!(parse_commit(b"author x <y> 1 +0000\n\nmsg").is_err());
        assert!(
            parse_commit(format!("tree {TREE}").as_bytes())
                .map_err(TestError::Unexpected)?
                .message
                .is_empty()
        );
        let weird = parse_commit(
            format!("tree {TREE}\nauthor broken\ncommitter <no-close 12\n\nm").as_bytes(),
        )
        .map_err(TestError::Unexpected)?;
        assert_eq!(weird.author.name, "broken");
        assert_eq!(weird.committer.when, 0);
        let invalid_utf8 =
            parse_commit(&[b"tree ".as_slice(), TREE.as_bytes(), b"\n\n\xff\xfe"].concat())
                .map_err(TestError::Unexpected)?;
        assert!(invalid_utf8.message.contains('\u{fffd}'));
        let no_subject =
            parse_commit(format!("tree {TREE}\n\n").as_bytes()).map_err(TestError::Unexpected)?;
        assert_eq!((no_subject.subject(), no_subject.body()), ("", ""));
        Ok(())
    }

    fn tree_bytes(entries: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (mode, name, hex) in entries {
            out.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            if let Some(oid) = Oid::from_hex(hex) {
                out.extend_from_slice(oid.as_bytes());
            }
        }
        out
    }

    #[test]
    fn parses_trees() -> TestResult {
        let data = tree_bytes(&[
            ("100644", "a.txt", PARENT),
            ("40000", "dir", TREE),
            ("160000", "sub", PARENT),
            ("120000", "lnk", PARENT),
        ]);
        let entries = parse_tree(&data).map_err(TestError::Unexpected)?;
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].name, b"a.txt");
        assert!(entries[1].is_tree() && !entries[0].is_tree());
        assert!(entries[2].is_gitlink());
        assert_eq!(entries[3].mode, 0o120_000);
        assert!(parse_tree(b"").map_err(TestError::Unexpected)?.is_empty());
        Ok(())
    }

    #[test]
    fn broken_trees_are_errors_not_panics() -> TestResult {
        for bad in [
            &b"100644"[..],
            b"100644 name",
            b"100644 name\0short",
            b"zzz name\0aaaaaaaaaaaaaaaaaaaa",
            b"\xff\xff \0",
            b"100644 a\0\x01\x02",
        ] {
            assert!(parse_tree(bad).is_err(), "{bad:?}");
        }
        Ok(())
    }

    #[test]
    fn parses_tags() -> TestResult {
        let text = format!(
            "object {PARENT}\ntype commit\ntag v1.0\ntagger T <t@x> 1700000000 +0000\n\nrelease notes\n"
        );
        let tag = parse_tag(text.as_bytes()).map_err(TestError::Unexpected)?;
        assert_eq!(
            (
                tag.target.hex().as_str(),
                tag.target_kind,
                tag.name.as_str()
            ),
            (PARENT, Kind::Commit, "v1.0")
        );
        assert_eq!(tag.tagger.map(|t| t.name), Some("T".to_owned()));
        assert!(parse_tag(b"type commit\n\n").is_err());
        assert!(parse_tag(format!("object {PARENT}\ntype bogus\n\n").as_bytes()).is_err());
        Ok(())
    }

    #[test]
    fn kinds_roundtrip() -> TestResult {
        for kind in [Kind::Commit, Kind::Tree, Kind::Blob, Kind::Tag] {
            assert_eq!(Kind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(Kind::from_name("nope"), None);
        Ok(())
    }
}
