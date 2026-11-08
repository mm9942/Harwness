//! Integrationstest gegen den echten Kernel (nur root, `#[ignore]`):
//! isoliert eine temporäre cgroup über [`NftNetlink`] und hebt die
//! Isolation wieder auf — mit Datenverkehr als Beleg.
//!
//! # Was der Test belegt
//! - Kodierung und Antwortauswertung funktionieren gegen den Kernel
//!   (Isolieren, Aufheben, Alles-Aufheben, jeweils wiederholt).
//! - Kernel-ID = Inode des cgroup2-Verzeichnisses und `level` = Tiefe unter
//!   der cgroup2-Wurzel auch für eine **nicht**-Wurzel-cgroup: nach dem
//!   Isolieren erreicht UDP von einem Socket der cgroup den Empfänger nicht
//!   mehr (`output`), und UDP an einen Socket der cgroup kommt nicht mehr an
//!   (`input`); fremde Sockets bleiben unberührt; nach dem Aufheben geht
//!   beides wieder.
//!
//! # Voraussetzungen
//! root; cgroup v2 (auch Hybrid-Modus mit `/sys/fs/cgroup/unified`), im
//! initialen cgroup-Namespace; `unshare` (util-linux); Kernel ≥ 5.13 mit
//! `nft_socket`. `lo` bringt der Test selbst hoch (`RTM_NEWLINK` über
//! `NETLINK_ROUTE`), iproute2 ist nicht nötig.
//!
//! # Seiteneffekte
//! Der Test startet sich selbst mit `unshare --net` in einem frischen
//! Netzwerk-Namespace neu: der nftables-Regelsatz des Hosts bleibt
//! unberührt. Nur die temporäre cgroup `harw-dod-nft-test-<pid>` entsteht
//! direkt unter der cgroup2-Wurzel; der Test verschiebt sich kurz hinein
//! und wieder zurück und entfernt sie am Ende (auch bei Fehlschlag).
//!
//! ```text
//! sudo -E cargo test -p harw-dod-nft --test kernel -- --ignored --test-threads=1
//! ```

#![cfg(target_os = "linux")]

use std::fs;
use std::io;
use std::net::UdpSocket;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use harw_dod_nft::{CgroupRule, NftNetlink};
use rustix::net::netlink::SocketAddrNetlink;
use rustix::net::{AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType};

harw_test_support::define_test_error!(
    pub(crate),
    Io(std::io::Error) => "I/O",
    Nft(harw_dod_nft::NftError) => "nft"
);

/// Markiert den Neustart im privaten Netzwerk-Namespace.
const IN_NETNS: &str = "HARW_DOD_NFT_KERNEL_TEST_IN_NETNS";

/// Name dieses Tests (für `--exact` beim Neustart).
const TEST_NAME: &str = "test_real_kernel_isolates_and_releases_a_temporary_cgroup";

/// Wartezeit auf ein UDP-Datagramm.
const RECV_WAIT: Duration = Duration::from_millis(300);

#[test]
#[ignore = "root only: creates a temporary cgroup and changes nftables in a private network namespace"]
fn test_real_kernel_isolates_and_releases_a_temporary_cgroup() -> TestResult {
    if std::env::var_os(IN_NETNS).is_some() {
        run_in_private_netns()
    } else {
        rerun_in_private_netns()
    }
}

// Startet diesen Test in einem frischen Netzwerk-Namespace neu.
fn rerun_in_private_netns() -> TestResult {
    require_root()?;
    let exe = std::env::current_exe()?;
    let status = Command::new("unshare")
        .arg("--net")
        .arg("--")
        .arg(exe)
        .args([TEST_NAME, "--exact", "--ignored", "--test-threads=1"])
        .env(IN_NETNS, "1")
        .status()
        .map_err(ctx("start unshare (util-linux)"))?;
    if status.success() {
        Ok(())
    } else {
        Err(TestError::Unexpected(format!(
            "the test failed in the private network namespace: {status}"
        )))
    }
}

// Der eigentliche Test (im privaten Netzwerk-Namespace).
fn run_in_private_netns() -> TestResult {
    require_root()?;
    loopback_up()?;
    let mount = cgroup2_mount()?;
    let home = own_cgroup(&mount)?;
    let name = format!("harw-dod-nft-test-{}", std::process::id());
    let cgroup = TempCgroup::create(&mount, &name, home)?;
    let kernel_id = fs::metadata(&cgroup.path)?.ino();
    let suffix = hex(name.as_bytes());
    // Direkt unter der cgroup2-Wurzel: Ebene 1.
    let rule = CgroupRule::new(kernel_id, 1, &suffix)?;

    // Die cgroup eines Sockets steht bei seiner Erzeugung fest.
    let outside_rx = receiver()?;
    let outside_tx = UdpSocket::bind("127.0.0.1:0")?;
    cgroup.enter()?;
    let inside_rx = receiver()?;
    let inside_tx = UdpSocket::bind("127.0.0.1:0")?;
    cgroup.leave()?;

    let nft = NftNetlink::open()?;
    assert!(delivered(&inside_tx, &outside_rx, b"before-out")?);
    assert!(delivered(&outside_tx, &inside_rx, b"before-in")?);

    nft.isolate(&rule)?;
    nft.isolate(&rule)?;
    assert!(
        !delivered(&inside_tx, &outside_rx, b"isolated-out")?,
        "the output chain must drop packets of the cgroup's sockets"
    );
    assert!(
        !delivered(&outside_tx, &inside_rx, b"isolated-in")?,
        "the input chain must drop packets to the cgroup's sockets"
    );
    assert!(
        delivered(&outside_tx, &outside_rx, b"unrelated")?,
        "sockets outside the cgroup keep their network"
    );

    nft.release(&rule)?;
    nft.release(&rule)?;
    nft.release_chains(&suffix)?;
    assert!(delivered(&inside_tx, &outside_rx, b"released-out")?);
    assert!(delivered(&outside_tx, &inside_rx, b"released-in")?);

    nft.isolate(&rule)?;
    assert!(!delivered(&inside_tx, &outside_rx, b"again-out")?);
    nft.release_all()?;
    nft.release_all()?;
    assert!(delivered(&inside_tx, &outside_rx, b"all-released-out")?);
    assert!(delivered(&outside_tx, &inside_rx, b"all-released-in")?);
    Ok(())
}

// Verlangt die effektive uid 0 (`/proc/self/status`, Zeile `Uid:`).
fn require_root() -> TestResult {
    let status = fs::read_to_string("/proc/self/status")?;
    let effective = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|uids| uids.split_whitespace().nth(1));
    if effective == Some("0") {
        Ok(())
    } else {
        Err(TestError::Unexpected(
            "this test needs root (run with sudo, see the module docs)".to_owned(),
        ))
    }
}

// Bringt `lo` im frischen Netzwerk-Namespace hoch wie `ip link set lo up`,
// aber ohne iproute2: ein `RTM_NEWLINK` über `NETLINK_ROUTE`. Konstanten:
// `RTM_NEWLINK` = 16 (`linux/rtnetlink.h:28`), `struct ifinfomsg` = 16 Byte
// (`rtnetlink.h:561-568`), `IFF_UP` = 1 (`linux/if.h:83`), `NLM_F_REQUEST |
// NLM_F_ACK` = 0x5 (`linux/netlink.h:62`, `:64`), `NLMSG_ERROR` = 2
// (`netlink.h:113`). `lo` trägt in jedem Netzwerk-Namespace den Index 1 (der
// Kernel legt es zuerst an). Mit Kernel 6.18 in einem frischen
// Netzwerk-Namespace geprobt (Bestätigung `0`, danach UDP über 127.0.0.1).
fn loopback_up() -> TestResult {
    // NETLINK_ROUTE ist Protokoll 0; rustix nimmt dafür `None`.
    let fd = rustix::net::socket_with(
        AddressFamily::NETLINK,
        SocketType::RAW,
        SocketFlags::CLOEXEC,
        None,
    )
    .map_err(io::Error::from)?;
    let mut request = Vec::with_capacity(32);
    // struct nlmsghdr: len, type, flags, seq, pid (Host-Reihenfolge).
    request.extend_from_slice(&32_u32.to_ne_bytes());
    request.extend_from_slice(&16_u16.to_ne_bytes());
    request.extend_from_slice(&0x5_u16.to_ne_bytes());
    request.extend_from_slice(&1_u32.to_ne_bytes());
    request.extend_from_slice(&0_u32.to_ne_bytes());
    // struct ifinfomsg: AF_UNSPEC, Füllbyte, Typ, Index 1 (`lo`), Flags und
    // Änderungsmaske je IFF_UP.
    request.extend_from_slice(&[0, 0]);
    request.extend_from_slice(&0_u16.to_ne_bytes());
    request.extend_from_slice(&1_i32.to_ne_bytes());
    request.extend_from_slice(&1_u32.to_ne_bytes());
    request.extend_from_slice(&1_u32.to_ne_bytes());
    let kernel = SocketAddrNetlink::new(0, 0);
    rustix::net::sendto(&fd, &request, SendFlags::empty(), &kernel).map_err(io::Error::from)?;
    // Der Kernel antwortet im `sendto`: ein NLMSG_ERROR, `int error` bei 16.
    let mut reply = [0_u8; 256];
    let (len, _) = rustix::net::recv(&fd, reply.as_mut_slice(), RecvFlags::DONTWAIT)
        .map_err(io::Error::from)?;
    let answer = reply.get(..len).ok_or_else(missing("RTM_NEWLINK reply"))?;
    let msg_type = answer
        .get(4..6)
        .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
        .map(u16::from_ne_bytes);
    let error = answer
        .get(16..20)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(i32::from_ne_bytes);
    if msg_type == Some(2) && error == Some(0) {
        Ok(())
    } else {
        Err(TestError::Unexpected(format!(
            "RTM_NEWLINK for lo failed: type {msg_type:?}, error {error:?}"
        )))
    }
}

// Einhängepunkt der cgroup2-Hierarchie, deren Wurzel eingehängt ist
// (`/proc/self/mountinfo`: Feld 4 = Wurzel, Feld 5 = Einhängepunkt).
fn cgroup2_mount() -> TestResult<PathBuf> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    for line in mountinfo.lines() {
        let Some((fields, filesystem)) = line.split_once(" - ") else {
            continue;
        };
        if filesystem.split(' ').next() != Some("cgroup2") {
            continue;
        }
        let mut fields = fields.split(' ');
        let root = fields.nth(3);
        let mount_point = fields.next();
        if root == Some("/")
            && let Some(mount_point) = mount_point
            && !mount_point.contains('\\')
        {
            return Ok(PathBuf::from(mount_point));
        }
    }
    Err(TestError::Unexpected(
        "no cgroup2 mount of the hierarchy root (cgroup v2 without cgroup namespace required)"
            .to_owned(),
    ))
}

// Die eigene cgroup in der cgroup2-Hierarchie (`/proc/self/cgroup`, `0::`).
fn own_cgroup(mount: &Path) -> TestResult<PathBuf> {
    let cgroups = fs::read_to_string("/proc/self/cgroup")?;
    let relative = cgroups
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(missing("cgroup v2 entry in /proc/self/cgroup"))?;
    Ok(mount.join(relative.trim_start_matches('/')))
}

// Kleinbuchstaben-Hex.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::new();
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            if let Some(digit) = DIGITS.get(usize::from(nibble)) {
                out.push(char::from(*digit));
            }
        }
    }
    out
}

// UDP-Empfänger auf 127.0.0.1 mit kurzer Wartezeit.
fn receiver() -> TestResult<UdpSocket> {
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    socket.set_read_timeout(Some(RECV_WAIT))?;
    Ok(socket)
}

// Ob `payload` von `tx` bei `rx` ankommt. Ein Drop am `output`-Hook zeigt
// sich beim Senden als `EPERM`, einer am `input`-Hook als Ausbleiben.
fn delivered(tx: &UdpSocket, rx: &UdpSocket, payload: &[u8]) -> TestResult<bool> {
    let target = rx.local_addr()?;
    match tx.send_to(payload, target) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    let mut buf = [0_u8; 64];
    // Höchstens einige verspätete Datagramme früherer Schritte überspringen.
    for _ in 0..8 {
        match rx.recv_from(&mut buf) {
            Ok((len, _)) if buf.get(..len) == Some(payload) => return Ok(true),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

/// Eine temporäre cgroup; beim Fallenlassen kehrt der Prozess in seine
/// cgroup zurück, und die temporäre wird entfernt.
struct TempCgroup {
    path: PathBuf,
    home: PathBuf,
}

impl TempCgroup {
    fn create(mount: &Path, name: &str, home: PathBuf) -> TestResult<Self> {
        let path = mount.join(name);
        fs::create_dir(&path)?;
        Ok(Self { path, home })
    }

    // Verschiebt den ganzen Prozess in die temporäre cgroup.
    fn enter(&self) -> TestResult {
        move_process(&self.path)
    }

    // Verschiebt den ganzen Prozess zurück.
    fn leave(&self) -> TestResult {
        move_process(&self.home)
    }
}

impl Drop for TempCgroup {
    fn drop(&mut self) {
        // Aufräumen nach bestem Bemühen: `drop` kann keinen Fehler melden;
        // eine übrig gebliebene leere cgroup ist harmlos.
        let _ = move_process(&self.home);
        let _ = fs::remove_dir(&self.path);
    }
}

// Schreibt die eigene pid nach `<cgroup>/cgroup.procs`.
fn move_process(cgroup: &Path) -> TestResult {
    fs::write(cgroup.join("cgroup.procs"), std::process::id().to_string())?;
    Ok(())
}
