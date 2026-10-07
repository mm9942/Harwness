//! `sys.id` — `id` und `whoami` in reinem Rust (`rustix::process`).
//!
//! Liefert reale und effektive IDs, Namen aus `/etc/passwd`/`/etc/group` und
//! die Zusatzgruppen. Die Flags `-u`, `-g`, `-G`, `-n`, `-r` bestimmen den
//! Text wie bei GNU `id`; `whoami` entspricht `id -un` (effektiver Benutzer).

use harw_tool_fsread::blocking::run_blocking;
use harw_tool_fsread::budget::{fail, flag, ok};
use harw_tool_fsread::users::UserDb;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::json;

/// Name des Werkzeugs.
pub const TOOL: &str = "sys.id";

/// Argumente für `sys.id`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct IdArgs {
    /// -u / --user: select the user ID for the text output.
    #[serde(default)]
    pub user: Option<bool>,
    /// -g / --group: select the group ID for the text output.
    #[serde(default)]
    pub group: Option<bool>,
    /// -G / --groups: select all group IDs for the text output.
    #[serde(default)]
    pub groups: Option<bool>,
    /// -n / --name: print names instead of numbers (needs user, group or groups).
    #[serde(default)]
    pub name: Option<bool>,
    /// -r / --real: use the real instead of the effective ID (needs user, group or groups).
    #[serde(default)]
    pub real: Option<bool>,
    /// whoami: only the effective user name (conflicts with the id flags).
    #[serde(default)]
    pub whoami: Option<bool>,
}

/// Rohdaten einer Identität.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Reale UID.
    pub uid: u32,
    /// Effektive UID.
    pub euid: u32,
    /// Reale GID.
    pub gid: u32,
    /// Effektive GID.
    pub egid: u32,
    /// Zusatzgruppen in `getgroups`-Reihenfolge (ohne Duplikate).
    pub groups: Vec<u32>,
}

/// Liest die Identität dieses Prozesses.
#[must_use]
pub fn current_identity() -> Identity {
    // Reihenfolge von `getgroups` bleibt erhalten (wie bei GNU `id`).
    let mut groups: Vec<u32> = Vec::new();
    for group in rustix::process::getgroups().unwrap_or_default() {
        let raw = group.as_raw();
        if !groups.contains(&raw) {
            groups.push(raw);
        }
    }
    Identity {
        uid: rustix::process::getuid().as_raw(),
        euid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getgid().as_raw(),
        egid: rustix::process::getegid().as_raw(),
        groups,
    }
}

/// Führt `sys.id` für `identity` aus.
#[must_use]
pub fn run(identity: &Identity, db: &UserDb, args: &IdArgs) -> ToolOutput {
    let (user, group, groups, name, real, whoami) = (
        flag(args.user),
        flag(args.group),
        flag(args.groups),
        flag(args.name),
        flag(args.real),
        flag(args.whoami),
    );
    if whoami && (user || group || groups || name || real) {
        return fail(
            TOOL,
            "whoami conflicts with user, group, groups, name and real",
        );
    }
    if [user, group, groups].iter().filter(|f| **f).count() > 1 {
        return fail(TOOL, "user, group and groups are mutually exclusive");
    }
    if (name || real) && !(user || group || groups) {
        return fail(TOOL, "name and real need one of user, group or groups");
    }
    let uid = if real { identity.uid } else { identity.euid };
    let gid = if real { identity.gid } else { identity.egid };
    let user_name = |id: u32| db.user_or_id(id);
    let group_name = |id: u32| db.group_or_id(id);
    // Wie GNU `id`: die gewählte Hauptgruppe zuerst, dann die übrigen in `getgroups`-Reihenfolge.
    let mut all_groups = vec![gid];
    for group in &identity.groups {
        if !all_groups.contains(group) {
            all_groups.push(*group);
        }
    }
    let text = if whoami {
        user_name(identity.euid)
    } else if user {
        if name {
            user_name(uid)
        } else {
            uid.to_string()
        }
    } else if group {
        if name {
            group_name(gid)
        } else {
            gid.to_string()
        }
    } else if groups {
        all_groups
            .iter()
            .map(|g| if name { group_name(*g) } else { g.to_string() })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        let mut line = format!(
            "uid={}({}) gid={}({})",
            identity.uid,
            user_name(identity.uid),
            identity.gid,
            group_name(identity.gid)
        );
        if identity.euid != identity.uid {
            line.push_str(&format!(
                " euid={}({})",
                identity.euid,
                user_name(identity.euid)
            ));
        }
        if identity.egid != identity.gid {
            line.push_str(&format!(
                " egid={}({})",
                identity.egid,
                group_name(identity.egid)
            ));
        }
        let listed: Vec<String> = all_groups
            .iter()
            .map(|g| format!("{g}({})", group_name(*g)))
            .collect();
        line.push_str(&format!(" groups={}", listed.join(",")));
        line
    };
    ok(
        TOOL,
        text.clone(),
        json!({
            "text": text,
            "uid": identity.uid,
            "euid": identity.euid,
            "gid": identity.gid,
            "egid": identity.egid,
            "user": user_name(identity.uid),
            "effective_user": user_name(identity.euid),
            "group": group_name(identity.gid),
            "effective_group": group_name(identity.egid),
            "groups": all_groups.iter().map(|g| json!({"gid": g, "name": group_name(*g)})).collect::<Vec<_>>(),
            "whoami": user_name(identity.euid),
            "truncated": false,
        }),
    )
}

/// Zeigt Benutzer- und Gruppenidentität wie `id` und `whoami`.
#[harw_macros::tool(
    name = "sys.id",
    description = "Shows who the agent process runs as like id and whoami: real/effective uid and gid with names and supplementary groups; flags user (-u), group (-g), groups (-G), name (-n), real (-r) shape the text exactly like GNU id; whoami=true gives only the effective user name. Use when you need the current user, uid or group membership instead of running id or whoami. Returns JSON {text, uid, euid, gid, egid, user, group, groups:[{gid,name}], whoami}. Read-only.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_id(_context: &ToolExecutionContext, args: IdArgs) -> Result<ToolOutput, ToolsError> {
    run_blocking(TOOL, move || {
        run(&current_identity(), &UserDb::load(), &args)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, error_of, json_of};
    use serde_json::Value;

    fn db() -> UserDb {
        UserDb::parse(
            "root:x:0:0\nalice:x:1000:1000\nbob:x:1001:1001\n",
            "root:x:0:\nalice:x:1000:\nstaff:x:50:\nbob:x:1001:\n",
        )
    }

    fn who() -> Identity {
        Identity {
            uid: 1000,
            euid: 1001,
            gid: 1000,
            egid: 50,
            groups: vec![50, 1000],
        }
    }

    fn id(args: Value) -> TestResult<Value> {
        json_of(run(&who(), &db(), &serde_json::from_value(args)?))
    }

    #[test]
    fn full_form_matches_gnu_id_layout() -> TestResult {
        let value = id(json!({}))?;
        assert_eq!(
            value["text"],
            "uid=1000(alice) gid=1000(alice) euid=1001(bob) egid=50(staff) groups=50(staff),1000(alice)"
        );
        assert_eq!(value["whoami"], "bob");
        Ok(())
    }

    #[test]
    fn flag_forms_match_gnu_id() -> TestResult {
        assert_eq!(id(json!({"user": true}))?["text"], "1001");
        assert_eq!(id(json!({"user": true, "real": true}))?["text"], "1000");
        assert_eq!(id(json!({"user": true, "name": true}))?["text"], "bob");
        assert_eq!(id(json!({"group": true, "name": true}))?["text"], "staff");
        assert_eq!(id(json!({"group": true, "real": true}))?["text"], "1000");
        assert_eq!(id(json!({"groups": true}))?["text"], "50 1000");
        assert_eq!(
            id(json!({"groups": true, "name": true}))?["text"],
            "staff alice"
        );
        assert_eq!(id(json!({"whoami": true}))?["text"], "bob");
        Ok(())
    }

    #[test]
    fn conflicting_flags_are_rejected() -> TestResult {
        for bad in [
            json!({"whoami": true, "user": true}),
            json!({"user": true, "group": true}),
            json!({"name": true}),
            json!({"real": true}),
        ] {
            error_of(run(&who(), &db(), &serde_json::from_value(bad)?))?;
        }
        assert!(serde_json::from_value::<IdArgs>(json!({"context": true})).is_err());
        Ok(())
    }

    #[test]
    fn unknown_ids_stay_numeric() -> TestResult {
        let identity = Identity {
            uid: 4242,
            euid: 4242,
            gid: 4343,
            egid: 4343,
            groups: vec![],
        };
        let value = json_of(run(&identity, &db(), &serde_json::from_value(json!({}))?))?;
        assert_eq!(
            value["text"],
            "uid=4242(4242) gid=4343(4343) groups=4343(4343)"
        );
        Ok(())
    }

    #[test]
    fn real_identity_matches_whoami_logic() -> TestResult {
        let identity = current_identity();
        assert_eq!(identity.euid, rustix::process::geteuid().as_raw());
        let value = json_of(run(
            &identity,
            &UserDb::load(),
            &serde_json::from_value(json!({"user": true}))?,
        ))?;
        assert_eq!(value["text"], identity.euid.to_string());
        Ok(())
    }
}
