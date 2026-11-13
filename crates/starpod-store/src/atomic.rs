//! Atomic file replacement.
//!
//! Every file this crate writes goes through [`write_atomic`], and the reason
//! is a crash-consistency requirement from GOAL.md §5: the application must
//! "handle corrupted, missing, or outdated stored data gracefully", and
//! ARCHITECTURE §5 says the mechanism is "write temp → fsync → rename".
//!
//! The sequence is:
//!
//! 1. Write the new contents to a temporary file **in the same directory** as
//!    the target. Same directory matters: a rename is only atomic within a
//!    filesystem, and `/tmp` is frequently a different one.
//! 2. `fsync` the temporary file, so the bytes are on the device before the
//!    name starts pointing at them.
//! 3. `fsync` the containing directory (where the platform supports it), so
//!    the rename itself survives a power loss.
//! 4. Rename the temporary file over the target.
//!
//! On Windows, step 4 uses `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`.
//! `std::fs::rename` on Windows does *not* replace an existing file — it
//! fails with `ERROR_ALREADY_EXISTS` — so the naive implementation would
//! break on the second write of every file. (ARCHITECTURE §5, risk R10.)
//!
//! The temporary file is given a name that includes the process id and a
//! counter, so two Starpod instances (or two threads) never collide on it.
//! A leftover temporary file from a crashed run is harmless: it is never read,
//! and [`write_atomic`] removes any stale temporary for the same target
//! before starting.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{StoreError, io_at};
use crate::permissions::{restrict_dir_to_current_user, restrict_to_current_user};

/// A counter that makes temporary file names unique within a process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The permissions a newly created file gets before it is renamed into place.
///
/// The file is restricted while it is still under its temporary name, so it
/// is never briefly world-readable under its final name.
#[cfg(unix)]
const TEMP_FILE_MODE: u32 = 0o600;

/// Write `bytes` to `path`, replacing it atomically.
///
/// The parent directory is created if it does not exist, with restrictive
/// permissions. The resulting file is readable only by the current user on
/// every platform.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_dir(parent)?;

    let tmp = temp_path_for(path);
    // A stale temporary file from a crashed run would otherwise be overwritten
    // anyway, but removing it first keeps the directory from accumulating them.
    let _ = std::fs::remove_file(&tmp);

    let result = write_and_replace(&tmp, path, bytes);
    if result.is_err() {
        // The rename did not happen, so the temporary file is ours to remove.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Create the directory `path` and all of its parents, restricted to the
/// current user.
///
/// On unix the mode is `0700`; on Windows the directory inherits the
/// per-user ACL of its parent and is additionally given an explicit
/// user-only DACL.
pub fn ensure_dir(path: &Path) -> Result<(), StoreError> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    match std::fs::create_dir(path) {
        Ok(()) => {}
        // Another process (or another thread) may have created it between
        // the `is_dir` check and here. That is fine.
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {
            return Ok(());
        }
        Err(err) => return Err(io_at(path, err)),
    }
    // A *directory* needs the traversal bit as well as the read and write
    // bits: a directory with mode `0600` cannot be entered, so nothing
    // inside it could be read or written.
    restrict_dir_to_current_user(path)?;
    Ok(())
}

/// The temporary path used for `target`.
fn temp_path_for(target: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".to_string());
    let temp_name = format!(".{file_name}.tmp-{}-{}", std::process::id(), counter);
    match target.parent() {
        Some(parent) => parent.join(temp_name),
        None => PathBuf::from(temp_name),
    }
}

/// Write the bytes to the temporary file, flush them, and move them into place.
fn write_and_replace(tmp: &Path, target: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(TEMP_FILE_MODE);
    }

    let mut file = options.open(tmp).map_err(|e| io_at(tmp, e))?;
    file.write_all(bytes).map_err(|e| io_at(tmp, e))?;
    file.flush().map_err(|e| io_at(tmp, e))?;
    // `sync_all` fsyncs both the data and the metadata, which is what makes
    // the rename below meaningful after a power loss.
    file.sync_all().map_err(|e| io_at(tmp, e))?;
    drop(file);

    // Restrict the permissions *before* the file becomes visible under its
    // final name.
    restrict_to_current_user(tmp)?;

    replace_file(tmp, target)?;

    // Make the rename itself durable. Not every platform supports fsync on a
    // directory; where it does not, the rename is still atomic, just not
    // guaranteed to survive a power cut.
    #[cfg(unix)]
    if let Some(parent) = target.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }

    Ok(())
}

/// Move `from` over `to`, replacing `to` if it exists.
#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> Result<(), StoreError> {
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let from_w = wide_null(from);
    let to_w = wide_null(to);
    // MOVEFILE_REPLACE_EXISTING makes the rename replace an existing target;
    // MOVEFILE_WRITE_THROUGH makes the operation wait for the bytes to reach
    // the device rather than returning once the metadata is cached.
    let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the
    // call, and the flags are a plain bitmask.
    unsafe { MoveFileExW(PCWSTR(from_w.as_ptr()), PCWSTR(to_w.as_ptr()), flags) }
        .map_err(|err| StoreError::io(to, std::io::Error::other(err.to_string())))?;
    Ok(())
}

/// Move `from` over `to`, replacing `to` if it exists.
#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> Result<(), StoreError> {
    // `std::fs::rename` replaces an existing destination atomically on unix.
    std::fs::rename(from, to).map_err(|e| io_at(to, e))
}

/// Encode `path` as a NUL-terminated UTF-16 string.
#[cfg(windows)]
fn wide_null(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn writing_creates_the_file_with_the_given_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");
        write_atomic(&target, b"hello").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"hello");
    }

    #[test]
    fn writing_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a").join("b").join("state.json");
        write_atomic(&target, b"hello").unwrap();
        assert!(target.is_file());
    }

    #[test]
    fn writing_twice_replaces_the_contents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");
        write_atomic(&target, b"first").unwrap();
        write_atomic(&target, b"second").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second");
    }

    #[test]
    fn writing_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");
        write_atomic(&target, b"hello").unwrap();
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "state.json")
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[test]
    fn a_failed_write_does_not_leave_a_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where a file is expected makes the write fail.
        let target = dir.path().join("state.json");
        fs::create_dir(&target).unwrap();
        assert!(write_atomic(&target, b"hello").is_err());
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "state.json")
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_write_leaves_the_previous_contents_intact() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");
        write_atomic(&target, b"good").unwrap();
        // Make the *parent* unwritable so the temporary file cannot be
        // created; the existing file must survive untouched.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
        assert!(write_atomic(&target, b"bad").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"good");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn replacing_over_an_existing_file_succeeds_on_windows() {
        // This is the case `std::fs::rename` fails on Windows; the
        // `MoveFileExW` path is what makes the second write work.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");
        write_atomic(&target, b"first").unwrap();
        write_atomic(&target, b"second").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second");
    }

    #[test]
    fn two_temporary_names_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");
        let a = temp_path_for(&target);
        let b = temp_path_for(&target);
        assert_ne!(a, b);
    }

    #[cfg(unix)]
    #[test]
    fn the_written_file_is_not_readable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");
        write_atomic(&target, b"hello").unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode was {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn the_created_directory_is_not_readable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested").join("state.json");
        write_atomic(&target, b"hello").unwrap();
        let mode = fs::metadata(dir.path().join("nested"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "mode was {mode:o}");
    }
}
