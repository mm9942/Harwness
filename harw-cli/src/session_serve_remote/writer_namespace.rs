//! Process-lifetime exclusion for a fresh, remote-only hosted store.
//!
//! This is not a Hub assignment, an old-writer retirement proof, or protection
//! against other programs with the same UID. Existing stores are never adopted.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use harw_fsutil::{
    OpenMode, ensure_owned_directory, ensure_private_regular, mkdir_at, open_beneath,
    open_dir_at, open_dir_nofollow, same_file_at,
};

const ROOT: &str = "private-writer-v1";
const LOCK: &str = "writer.lock";
const HOST: &str = "host";
const SESSIONS: &str = "sessions";

static ENTRY: Mutex<()> = Mutex::new(());
// Intentionally never released by ingress shutdown or startup unwind: detached
// in-process workers can still own store handles after the listener has stopped.
static ACTIVE: OnceLock<WriterNamespace> = OnceLock::new();

struct WriterNamespace {
    remote: PathBuf,
    root: File,
    lock: File,
    host: File,
    sessions: File,
}

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "private remote writer namespace refused")
}

fn private(dir: &File) -> io::Result<()> {
    ensure_owned_directory(dir)?;
    if dir.metadata()?.mode() & 0o077 != 0 {
        return Err(refused());
    }
    Ok(())
}

impl WriterNamespace {
    fn open(remote: &Path) -> io::Result<Self> {
        if !remote.is_absolute() || remote.canonicalize()? != remote {
            return Err(refused());
        }
        let parent = File::from(open_dir_nofollow(remote)?);
        private(&parent)?;
        let created = mkdir_at(parent.as_fd(), Path::new(ROOT), 0o700)?;
        let root = open_dir_at(parent.as_fd(), Path::new(ROOT))?;
        private(&root)?;

        // The locked file doubles as a durable marker. A pre-existing
        // unmarked or partially initialized directory is not auto-repaired.
        let mode = if created {
            OpenMode { read: true, ..OpenMode::write_create_new(0o600) }
        } else {
            OpenMode::read_write()
        };
        let mut lock = open_beneath(root.as_fd(), Path::new(LOCK), mode)?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock =>
                io::Error::new(io::ErrorKind::WouldBlock, "private remote writer is already active"),
            std::fs::TryLockError::Error(error) => error,
        })?;
        ensure_private_regular(&lock)?;
        if lock.metadata()?.nlink() != 1 || !same_file_at(root.as_fd(), Path::new(LOCK), &lock)? {
            return Err(refused());
        }

        if created
            && (!mkdir_at(root.as_fd(), Path::new(HOST), 0o700)?
                || !mkdir_at(root.as_fd(), Path::new(SESSIONS), 0o700)?)
        {
            return Err(refused());
        }
        let host = open_dir_at(root.as_fd(), Path::new(HOST))?;
        let sessions = open_dir_at(root.as_fd(), Path::new(SESSIONS))?;
        private(&host)?;
        private(&sessions)?;
        let marker = marker(&root, &lock, &host, &sessions)?;
        if created {
            // Publish the marker only after the two private child directories.
            root.sync_all()?;
            lock.write_all(marker.as_bytes())?;
            lock.sync_all()?;
            root.sync_all()?;
            parent.sync_all()?;
        } else {
            let mut prior = Vec::new();
            (&lock).take(marker.len() as u64 + 1).read_to_end(&mut prior)?;
            if prior.as_slice() != marker.as_bytes() {
                return Err(refused());
            }
        }

        let namespace = Self { remote: remote.to_owned(), root, lock, host, sessions };
        namespace.confirm()?;
        Ok(namespace)
    }

    fn confirm(&self) -> io::Result<()> {
        let parent = File::from(open_dir_nofollow(&self.remote)?);
        private(&parent)?;
        private(&self.root)?;
        private(&self.host)?;
        private(&self.sessions)?;
        ensure_private_regular(&self.lock)?;
        if self.lock.metadata()?.nlink() != 1
            || !same_file_at(parent.as_fd(), Path::new(ROOT), &self.root)?
            || !same_file_at(self.root.as_fd(), Path::new(LOCK), &self.lock)?
            || !same_file_at(self.root.as_fd(), Path::new(HOST), &self.host)?
            || !same_file_at(self.root.as_fd(), Path::new(SESSIONS), &self.sessions)?
        {
            return Err(refused());
        }
        // The existing stores are path-based. Pin both data directory
        // inodes and refuse environments without usable procfs FDs. The
        // explicit final dot permits O_NOFOLLOW on the final path component;
        // the kernel resolves only this process's permanently held FD link.
        for dir in [&self.host, &self.sessions] {
            let anchor = anchored(dir);
            let reopened = File::from(open_dir_nofollow(&anchor)?).metadata()?;
            let expected = dir.metadata()?;
            if (reopened.dev(), reopened.ino()) != (expected.dev(), expected.ino()) {
                return Err(refused());
            }
        }
        Ok(())
    }
}

fn anchored(dir: &File) -> PathBuf {
    PathBuf::from("/proc/self/fd").join(dir.as_raw_fd().to_string()).join(".")
}

fn marker(root: &File, lock: &File, host: &File, sessions: &File) -> io::Result<String> {
    let root = root.metadata()?;
    let lock = lock.metadata()?;
    let host = host.metadata()?;
    let sessions = sessions.metadata()?;
    Ok(format!(
        "harw-private-remote-writer-v1\n{}:{}\n{}:{}\n{}:{}\n{}:{}\n",
        root.dev(), root.ino(), lock.dev(), lock.ino(),
        host.dev(), host.ino(), sessions.dev(), sessions.ino(),
    ))
}

pub(super) fn acquire(remote: &Path) -> io::Result<()> {
    let _serial = ENTRY.lock().map_err(|_| refused())?;
    if ACTIVE.get().is_some() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "restart the process before reopening the private remote store"));
    }
    ACTIVE.set(WriterNamespace::open(remote)?).map_err(|_| refused())
}

pub(super) fn active_paths(remote: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let namespace = ACTIVE.get().filter(|store| store.remote == remote).ok_or_else(refused)?;
    namespace.confirm()?;
    Ok((anchored(&namespace.host), anchored(&namespace.sessions)))
}

/// Bind a creation receipt to the exact currently guarded private namespace.
pub(super) fn active_namespace_id(remote: &Path) -> io::Result<[u8; 32]> {
    let namespace = ACTIVE.get().filter(|store| store.remote == remote).ok_or_else(refused)?;
    namespace.confirm()?;
    let marker = marker(&namespace.root, &namespace.lock, &namespace.host, &namespace.sessions)?;
    Ok(*blake3::hash(marker.as_bytes()).as_bytes())
}

/// Read-only workstation/operator access. Never acquire the producer lock or
/// create, repair or adopt a directory. Pin child FDs during the whole read.
pub(super) fn read_committed<T>(remote: &Path, read: impl FnOnce(&Path, [u8; 32]) -> io::Result<T>) -> io::Result<T> {
    if !remote.is_absolute() || remote.canonicalize()? != remote {
        return Err(refused());
    }
    let parent = File::from(open_dir_nofollow(remote)?);
    private(&parent)?;
    let root = open_dir_at(parent.as_fd(), Path::new(ROOT))?;
    private(&root)?;
    let lock = open_beneath(root.as_fd(), Path::new(LOCK), OpenMode::read_only())?;
    let host = open_dir_at(root.as_fd(), Path::new(HOST))?;
    let sessions = open_dir_at(root.as_fd(), Path::new(SESSIONS))?;
    let namespace = WriterNamespace { remote: remote.to_owned(), root, lock, host, sessions };
    namespace.confirm()?;
    let expected = marker(&namespace.root, &namespace.lock, &namespace.host, &namespace.sessions)?;
    let check_marker = || -> io::Result<()> {
        let mut bytes = Vec::new();
        let file = open_beneath(namespace.root.as_fd(), Path::new(LOCK), OpenMode::read_only())?;
        if !same_file_at(namespace.root.as_fd(), Path::new(LOCK), &namespace.lock)?
            || !same_file_at(namespace.root.as_fd(), Path::new(LOCK), &file)?
        {
            return Err(refused());
        }
        file.take(expected.len() as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.as_slice() != expected.as_bytes() {
            return Err(refused());
        }
        Ok(())
    };
    check_marker()?;
    let output = read(&anchored(&namespace.host), *blake3::hash(expected.as_bytes()).as_bytes())?;
    namespace.confirm()?;
    check_marker()?;
    Ok(output)
}


/// Filesystem address for the co-located proof-only socket. Use exclusively
/// with the held private namespace and recheck its exact inode before serving.
pub(super) fn readiness_socket_path(remote: &Path) -> io::Result<PathBuf> {
    let namespace = ACTIVE.get().filter(|store| store.remote == remote).ok_or_else(refused)?;
    namespace.confirm()?;
    Ok(namespace.remote.join(ROOT).join("readiness-v2.sock"))
}
