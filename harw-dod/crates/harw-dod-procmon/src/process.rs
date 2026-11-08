//! Prozesskontext für `harw_dod_signals::Actor::process` (PL-100 WP0.15).
//!
//! # Verantwortungsbereich
//! Bildet aus dem, was ein Prozessstart-Record über den auslösenden Prozess
//! trägt, einen [`harw_dod_signals::ProcessContext`] — statt `pid`, `ppid`
//! und `comm` wie bisher an der Abbildung auf `SecurityEvent` zu verwerfen.
//! Drei Bausteine:
//!
//! - [`display_comm`] macht einen rohen, **angreiferkontrollierten**
//!   `comm` darstellungssicher: höchstens
//!   [`ProcessContext::KERNEL_COMM_BYTES`] Byte (Linux' `TASK_COMM_LEN`),
//!   danach escapt über den gemeinsamen Escaper `harw-untrusted-text`
//!   (PL-100 WP0.8) und auf [`ProcessContext::COMM_MAX_CHARS`] Zeichen
//!   begrenzt.
//! - [`process_context_v1`] für den versionierten Wire-Pfad: `pid` ist die
//!   `tgid` des Wire-Kopfs (die pid im Sinn des Userlands), `ppid` die vom
//!   BPF-Programm gelesene `tgid` des `real_parent`. Der v1-Kopf trägt nur
//!   die **reale** UID (`bpf_get_current_uid_gid`), keine effektive —
//!   `euid` bleibt deshalb `None`, statt die reale UID als effektive
//!   auszugeben.
//! - [`exec_process_context`] für den älteren `ExecEvent`-Pfad, dessen
//!   Payload `pid`, `ppid`, die **effektive** UID und `comm` trägt.
//!
//! Eine `0` gilt überall als „unbekannt": der Kernel liefert `ppid = 0`,
//! wenn das BPF-Programm den Elternprozess nicht lesen konnte, und
//! `pid = 0` ist nie ein Akteur (Idle-Task).
//!
//! # Nebenläufigkeit
//! Reine, zustandslose Funktionen; von beliebig vielen Threads aufrufbar.
//!
//! # Fehler
//! Keine — alle Funktionen sind total und liefern `None` für Fehlendes.
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::TaskIdentity;
//! use harw_dod_procmon::process::{display_comm, process_context_v1};
//!
//! let task = TaskIdentity { tgid: 10, pid: 11, ppid: 9, uid: 1_000, cgroup_id: 77 };
//! let process = process_context_v1(&task, Some("sshd"));
//! assert_eq!(process.as_ref().map(|p| p.pid), Some(10));
//! assert_eq!(process.and_then(|p| p.ppid), Some(9));
//! assert_eq!(display_comm("evil\u{1b}[2J"), Some("evil\\u{001b}[2J".to_owned()));
//! ```

use harw_dod_bpf::TaskIdentity;
use harw_dod_signals::ProcessContext;

use crate::event::ExecEvent;

/// Macht einen rohen Prozessnamen (`comm`) darstellungssicher.
///
/// # Description
/// Kürzt `raw` zuerst an einer Zeichengrenze auf höchstens
/// [`ProcessContext::KERNEL_COMM_BYTES`] Byte (mehr kann der Kernel nicht
/// geliefert haben; ein verlustbehaftet dekodierter Puffer kann durch
/// Ersatzzeichen länger geworden sein) und escapt das Ergebnis danach
/// einzeilig über `harw_untrusted_text::escape_line`: Steuer-, Format-,
/// Bidi- und unsichtbare Zeichen werden zu sichtbarem `\u{…}`, die Ausgabe
/// hat höchstens [`ProcessContext::COMM_MAX_CHARS`] Zeichen (bei Kürzung
/// endet sie auf `…`).
///
/// # Arguments
/// - `raw` (`&str`): der rohe `comm`, etwa aus
///   `harw_dod_bpf::event::read_fixed_c_str`.
///
/// # Returns
/// Den escapten Namen; `None`, wenn `raw` leer ist.
///
/// # Examples
/// ```rust
/// use harw_dod_procmon::process::display_comm;
///
/// assert_eq!(display_comm("sshd"), Some("sshd".to_owned()));
/// assert_eq!(display_comm(""), None);
/// assert_eq!(display_comm("a\u{202E}b"), Some("a\\u{202e}b".to_owned()));
/// ```
#[must_use]
pub fn display_comm(raw: &str) -> Option<String> {
    let mut bounded = String::with_capacity(ProcessContext::KERNEL_COMM_BYTES);
    for ch in raw.chars() {
        let Some(next) = bounded.len().checked_add(ch.len_utf8()) else {
            break;
        };
        if next > ProcessContext::KERNEL_COMM_BYTES {
            break;
        }
        bounded.push(ch);
    }
    if bounded.is_empty() {
        return None;
    }
    Some(harw_untrusted_text::escape_line(
        &bounded,
        ProcessContext::COMM_MAX_CHARS,
    ))
}

/// Baut den Prozesskontext eines versionierten v1-Wire-Records.
///
/// # Description
/// Siehe Moduldoku: `pid` = `task.tgid`, `ppid` = `task.ppid` (die `tgid`
/// des `real_parent`; `0` → `None`), `comm` über [`display_comm`], `euid`
/// immer `None` (der v1-Kopf trägt nur die reale UID).
///
/// # Arguments
/// - `task` (`&TaskIdentity`): die Identität aus dem gemeinsamen Wire-Kopf.
/// - `comm` (`Option<&str>`): der rohe Prozessname, falls der Record einen
///   trägt (Exec ja, TCP-Connect nein).
///
/// # Returns
/// `Some(ProcessContext)`; `None`, wenn `task.tgid == 0`.
///
/// # Examples
/// ```rust
/// use harw_dod_bpf::TaskIdentity;
/// use harw_dod_procmon::process::process_context_v1;
///
/// let task = TaskIdentity { tgid: 0, pid: 0, ppid: 0, uid: 0, cgroup_id: 0 };
/// assert_eq!(process_context_v1(&task, None), None);
/// ```
#[must_use]
pub fn process_context_v1(task: &TaskIdentity, comm: Option<&str>) -> Option<ProcessContext> {
    let base = ProcessContext::from_pid(task.tgid)?;
    Some(ProcessContext {
        ppid: (task.ppid != 0).then_some(task.ppid),
        comm: comm.and_then(display_comm),
        ..base
    })
}

/// Baut den Prozesskontext eines älteren, aus dem Payload gedeuteten
/// [`ExecEvent`].
///
/// # Description
/// `pid` = `exec.pid`, `ppid` = `exec.ppid` (`0` → `None`), `comm` über
/// [`display_comm`], `euid` = `exec.uid` — das ältere Payload-Layout trägt
/// laut [`crate::event`]-Moduldoku die **effektive** UID.
///
/// # Arguments
/// - `exec` (`&ExecEvent`): das gedeutete Ereignis.
///
/// # Returns
/// `Some(ProcessContext)`; `None`, wenn `exec.pid == 0`.
///
/// # Examples
/// ```rust
/// use harw_dod_procmon::ExecEvent;
/// use harw_dod_procmon::process::exec_process_context;
/// use harw_types::ContentDigest;
///
/// let exec = ExecEvent {
///     pid: 4_242,
///     ppid: 1,
///     uid: 0,
///     comm: "sshd".to_owned(),
///     filename: "/usr/sbin/sshd".to_owned(),
///     argv_digest: ContentDigest::of(b"unused"),
/// };
/// let process = exec_process_context(&exec);
/// assert_eq!(process.and_then(|p| p.euid), Some(0));
/// ```
#[must_use]
pub fn exec_process_context(exec: &ExecEvent) -> Option<ProcessContext> {
    let base = ProcessContext::from_pid(exec.pid)?;
    Some(ProcessContext {
        ppid: (exec.ppid != 0).then_some(exec.ppid),
        comm: display_comm(&exec.comm),
        euid: Some(exec.uid),
        ..base
    })
}

#[cfg(test)]
mod tests {
    use harw_dod_bpf::TaskIdentity;
    use harw_dod_signals::ProcessContext;
    use harw_types::ContentDigest;

    use super::{display_comm, exec_process_context, process_context_v1};
    use crate::event::ExecEvent;
    use crate::test_support::{TestError, TestResult};

    fn task() -> TaskIdentity {
        TaskIdentity {
            tgid: 10,
            pid: 11,
            ppid: 9,
            uid: 1_000,
            cgroup_id: 77,
        }
    }

    #[test]
    fn test_display_comm_keeps_a_plain_name() {
        assert_eq!(display_comm("sshd"), Some("sshd".to_owned()));
    }

    #[test]
    fn test_display_comm_maps_empty_to_none() {
        assert_eq!(display_comm(""), None);
    }

    #[test]
    fn test_display_comm_escapes_terminal_and_bidi_controls() -> TestResult {
        let escaped = display_comm("a\u{1b}[31m\u{202E}")
            .ok_or(TestError::Missing("nicht-leerer comm"))?;
        assert_eq!(escaped, "a\\u{001b}[31m\\u{202e}");
        assert!(harw_untrusted_text::is_clean(&escaped));
        Ok(())
    }

    #[test]
    fn test_display_comm_bounds_input_to_task_comm_len_bytes() -> TestResult {
        let long = display_comm("abcdefghijklmnopqrstuvwxyz")
            .ok_or(TestError::Missing("nicht-leerer comm"))?;
        assert_eq!(long, "abcdefghijklmnop");
        assert_eq!(long.len(), ProcessContext::KERNEL_COMM_BYTES);

        // Zweibyte-Zeichen: nie mitten in einem Zeichen schneiden.
        let wide = display_comm("ééééééééé").ok_or(TestError::Missing("nicht-leerer comm"))?;
        assert_eq!(wide, "éééééééé");
        assert!(wide.len() <= ProcessContext::KERNEL_COMM_BYTES);
        Ok(())
    }

    #[test]
    fn test_display_comm_output_never_exceeds_comm_max_chars() -> TestResult {
        // Sechzehn Steuerzeichen ergäben escapt 128 Zeichen.
        let hostile = "\u{1}".repeat(ProcessContext::KERNEL_COMM_BYTES);
        let escaped =
            display_comm(&hostile).ok_or(TestError::Missing("nicht-leerer comm"))?;
        assert!(escaped.chars().count() <= ProcessContext::COMM_MAX_CHARS);
        assert!(escaped.ends_with('…'));
        assert!(harw_untrusted_text::is_clean(&escaped));
        Ok(())
    }

    #[test]
    fn test_process_context_v1_keeps_tgid_parent_and_escaped_comm() {
        assert_eq!(
            process_context_v1(&task(), Some("cur\u{7}l")),
            Some(ProcessContext {
                pid: 10,
                ppid: Some(9),
                comm: Some("cur\\u{0007}l".to_owned()),
                euid: None,
            })
        );
    }

    #[test]
    fn test_process_context_v1_maps_zero_ids_to_absent() {
        let mut orphan = task();
        orphan.ppid = 0;
        assert_eq!(
            process_context_v1(&orphan, None),
            Some(ProcessContext {
                pid: 10,
                ppid: None,
                comm: None,
                euid: None,
            })
        );

        let mut idle = task();
        idle.tgid = 0;
        assert_eq!(process_context_v1(&idle, Some("swapper")), None);
    }

    #[test]
    fn test_exec_process_context_carries_effective_uid_of_the_legacy_layout() {
        let exec = ExecEvent {
            pid: 4_242,
            ppid: 0,
            uid: 0,
            comm: "sshd".to_owned(),
            filename: "/usr/sbin/sshd".to_owned(),
            argv_digest: ContentDigest::of(b"-D"),
        };
        assert_eq!(
            exec_process_context(&exec),
            Some(ProcessContext {
                pid: 4_242,
                ppid: None,
                comm: Some("sshd".to_owned()),
                euid: Some(0),
            })
        );

        let no_pid = ExecEvent { pid: 0, ..exec };
        assert_eq!(exec_process_context(&no_pid), None);
    }
}
