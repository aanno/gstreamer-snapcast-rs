//! Host-wide unique client instance numbers.
//!
//! A Snapcast server identifies a client by its host id (the MAC address by
//! default) plus `#<instance>` for instances other than 1. Several clients on one
//! host, e.g. one for line-out and one for a Bluetooth speaker, therefore need
//! different instance numbers, or the server mixes them up.
//!
//! Instance numbers are claimed with lock files, one per number, in
//! `$XDG_RUNTIME_DIR/gst-snapcast-engine` (or the temp dir). A lock is held as long as
//! its [`InstanceLock`] lives and disappears with the process, even after a crash,
//! so stale numbers cannot pile up. This coordinates all engine clients on the host,
//! across processes; C++ snapclients don't take part.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// Highest instance number tried for automatic allocation.
const MAX_INSTANCE: u32 = 256;

/// A claimed instance number, released when dropped.
#[derive(Debug)]
pub(crate) struct InstanceLock {
    pub(crate) number: u32,
    _file: File,
}

/// Where the lock files live.
pub(crate) fn lock_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("gst-snapcast-engine")
}

fn try_claim(dir: &Path, number: u32) -> io::Result<Option<InstanceLock>> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join(format!("instance-{number}.lock")))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock {
            number,
            _file: file,
        })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// Claims the lowest free instance number (from 1).
pub(crate) fn claim_free(dir: &Path) -> io::Result<InstanceLock> {
    std::fs::create_dir_all(dir)?;
    for number in 1..=MAX_INSTANCE {
        if let Some(lock) = try_claim(dir, number)? {
            return Ok(lock);
        }
    }
    Err(io::Error::other(format!(
        "all {MAX_INSTANCE} client instance numbers are in use"
    )))
}

/// Claims a given instance number; `Ok(None)` if another client holds it.
pub(crate) fn claim(dir: &Path, number: u32) -> io::Result<Option<InstanceLock>> {
    std::fs::create_dir_all(dir)?;
    try_claim(dir, number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gst-snapcast-engine-instance-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn free_numbers_are_claimed_in_order_and_reused() {
        let dir = temp_dir("order");
        let a = claim_free(&dir).unwrap();
        let b = claim_free(&dir).unwrap();
        assert_eq!((a.number, b.number), (1, 2));
        drop(a);
        // The lowest free number again: a restarted client gets its old number back.
        assert_eq!(claim_free(&dir).unwrap().number, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_explicit_number_reports_when_it_is_taken() {
        let dir = temp_dir("explicit");
        let held = claim(&dir, 3).unwrap().expect("3 is free");
        assert!(claim(&dir, 3).unwrap().is_none(), "3 is held");
        // Automatic allocation skips nothing but held numbers.
        assert_eq!(claim_free(&dir).unwrap().number, 1);
        drop(held);
        assert!(claim(&dir, 3).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
