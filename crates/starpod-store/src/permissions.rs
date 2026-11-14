//! Restricting a file or directory to the current user.
//!
//! Two platforms, two mechanisms, one contract: **nothing this crate writes
//! may be readable by another user on the machine.**
//!
//! - **unix** — `chmod 0600` for files and `0700` for directories. The
//!   credential is the file mode, and the owner is whoever created the file.
//!   This is the same mechanism the engine uses for its own controller file
//!   (`meow-common-0.29.0/src/controller_file.rs` writes `0600`/`0700`).
//! - **Windows** — a DACL that grants full access to the current user's SID
//!   and to nobody else, with the inheritance flag cleared so the entries do
//!   not propagate to child objects. The file's *owner* is the current user
//!   by default, and Windows has no POSIX-style "other" class, so the DACL is
//!   what carries the restriction.
//!
//! ARCHITECTURE §5.1 requires the generated engine configuration to be
//! "written with mode `0600` on unix, an ACL on Windows" precisely because
//! that file contains node credentials and subscription URLs.

use std::path::Path;

use crate::error::StoreError;

/// The access mask that grants full control over a file.
///
/// `FILE_ALL_ACCESS`, from `Win32::Storage::FileSystem`. The value is
/// `0x001F01FF`, not `0x003FFFFF`: the latter is the "standard access
/// rights plus everything" mask, and Windows canonicalises the two down to
/// the standard rights, so the value actually stored in the ACE is this one.
#[cfg(windows)]
const FILE_ALL_ACCESS: u32 = 0x001F_01FF;

/// Restrict `path` so that only the current user can read or write it.
///
/// On unix this sets mode `0600`. On Windows it replaces the DACL with one
/// that grants the current user full control and denies everyone else.
pub fn restrict_to_current_user(path: &Path) -> Result<(), StoreError> {
    // The file has to exist: on unix the mode is read off the inode, and on
    // Windows the DACL is applied to an existing object. Reporting a missing
    // file as an I/O error (rather than a permission error) keeps the two
    // failure modes distinguishable on every platform.
    if !path.exists() {
        return Err(StoreError::io(
            path,
            std::io::Error::new(std::io::ErrorKind::NotFound, "the file does not exist"),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .map_err(|e| StoreError::io(path, e))?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms).map_err(|e| StoreError::io(path, e))?;
    }
    #[cfg(windows)]
    {
        restrict_windows(path)?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        // There is no third supported platform; if one is ever added, the
        // compiler will point at this arm rather than silently leaving the
        // file world-readable.
        return Err(StoreError::Permissions {
            path: path.to_path_buf(),
            reason: "this platform has no supported permission mechanism".to_string(),
        });
    }
    Ok(())
}

/// Restrict `path` so that only the current user can traverse into it.
///
/// On unix this sets mode `0700`. On Windows a directory inherits the
/// per-user ACL of its parent, so the explicit DACL is applied here too.
pub fn restrict_dir_to_current_user(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .map_err(|e| StoreError::io(path, e))?
            .permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(path, perms).map_err(|e| StoreError::io(path, e))?;
    }
    #[cfg(windows)]
    {
        restrict_windows(path)?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        return Err(StoreError::Permissions {
            path: path.to_path_buf(),
            reason: "this platform has no supported permission mechanism".to_string(),
        });
    }
    Ok(())
}

/// Replace the DACL of `path` with one that only grants the current user.
///
/// The DACL is built explicitly rather than through an SDDL string, because
/// the SDDL form would have to embed a SID string that has to be looked up
/// and formatted — and getting that formatting wrong silently produces a DACL
/// that grants nothing (or everything). Here the SID comes straight from the
/// process token.
#[cfg(windows)]
fn restrict_windows(path: &Path) -> Result<(), StoreError> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
    use windows::Win32::Security::{
        ACL, ACL_REVISION, AddAccessAllowedAce, DACL_SECURITY_INFORMATION, GetLengthSid,
        GetTokenInformation, InitializeAcl, PROTECTED_DACL_SECURITY_INFORMATION, PSID, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // --- find the current user's SID -------------------------------------
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always
    // valid, and the token handle is closed on every path out.
    let mut token: HANDLE = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|e| permission_error(path, &format!("could not open the process token: {e}")))?;

    // The first call sizes the buffer, the second fills it. The buffer has
    // to be DWORD-aligned, which is why it is a `Vec<u32>` and not a
    // `Vec<u8>`.
    let mut needed = 0u32;
    let sized = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    if sized.is_ok() || needed == 0 {
        let _ = unsafe { CloseHandle(token) };
        return Err(permission_error(
            path,
            "the process token did not report a user SID",
        ));
    }

    let mut words = vec![0u32; needed.div_ceil(4) as usize];
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(words.as_mut_ptr() as *mut std::ffi::c_void),
            needed,
            &mut needed,
        )
    };
    let _ = unsafe { CloseHandle(token) };
    read.map_err(|e| permission_error(path, &format!("could not read the token user: {e}")))?;

    let token_user = unsafe { &*(words.as_ptr() as *const TOKEN_USER) };
    let sid: PSID = token_user.User.Sid;

    // --- build the DACL ---------------------------------------------------
    // One ACE granting full access to the current user.
    //
    // The ACE is added with `AddAccessAllowedAce` rather than being laid out
    // by hand: the windows-rs projection of `ACCESS_ALLOWED_ACE` models the
    // variable-length SID as a fixed `u32`, so writing the SID by hand means
    // assuming an offset. `AddAccessAllowedAce` uses the real layout.
    //
    // The second argument is the ACE *revision*, not a set of flags.
    // `AddAccessAllowedAce` always creates the ACE with zero flags, so the
    // entry does not inherit — which is what a user-only DACL wants.
    let sid_len = unsafe { GetLengthSid(sid) } as usize;
    let ace_size = 8 + sid_len;
    let acl_size = std::mem::size_of::<ACL>() + ace_size;

    // The ACL has to be DWORD-aligned, so it is allocated as words.
    let mut acl_words = vec![0u32; acl_size.div_ceil(4) as usize];
    let acl_ptr = acl_words.as_mut_ptr() as *mut ACL;

    // SAFETY: `acl_words` is a correctly sized, writable, DWORD-aligned
    // buffer and the revision is the one `InitializeAcl` documents for a
    // plain (non-canonical) ACL.
    unsafe { InitializeAcl(acl_ptr, acl_size as u32, ACL_REVISION) }
        .map_err(|e| permission_error(path, &format!("could not initialize the ACL: {e}")))?;

    // SAFETY: the ACL was just initialised with room for one ACE of
    // `ace_size` bytes, and the SID is a live, owned SID.
    unsafe {
        AddAccessAllowedAce(acl_ptr, ACL_REVISION, FILE_ALL_ACCESS, sid)
            .map_err(|e| permission_error(path, &format!("could not add the access ACE: {e}")))?;
    }

    // --- apply the DACL ---------------------------------------------------
    // `PROTECTED_DACL_SECURITY_INFORMATION` stops the DACL from inheriting
    // the parent's entries, so the "everyone else" entries that
    // `%LOCALAPPDATA%` normally carries are not merged in.
    let info = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
    let name = windows::core::HSTRING::from(path.as_os_str());
    // SAFETY: the name is a live UTF-16 string and the ACL is a live,
    // correctly sized buffer for the duration of the call.
    let status = unsafe {
        SetNamedSecurityInfoW(
            &name,
            SE_FILE_OBJECT,
            info,
            None,
            None,
            Some(acl_ptr as *const ACL),
            None,
        )
    };
    if status.0 != 0 {
        return Err(permission_error(
            path,
            &format!("could not apply the DACL: {}", status.0),
        ));
    }
    Ok(())
}

/// Build a [`StoreError::Permissions`] with a human-readable reason.
#[cfg(windows)]
fn permission_error(path: &Path, reason: &str) -> StoreError {
    StoreError::Permissions {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricting_a_file_succeeds_and_the_file_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.txt");
        std::fs::write(&file, b"token").unwrap();
        restrict_to_current_user(&file).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"token");
    }

    #[test]
    fn restricting_a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = restrict_to_current_user(&dir.path().join("nope")).unwrap_err();
        assert!(matches!(err, StoreError::Io { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn a_restricted_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.txt");
        std::fs::write(&file, b"token").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        restrict_to_current_user(&file).unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode was {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn a_restricted_directory_is_mode_0700() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        restrict_dir_to_current_user(&sub).unwrap();
        let mode = std::fs::metadata(&sub).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "mode was {mode:o}");
    }

    #[cfg(windows)]
    #[test]
    fn a_restricted_file_can_still_be_written_by_the_current_user() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.txt");
        std::fs::write(&file, b"token").unwrap();
        restrict_to_current_user(&file).unwrap();
        // The DACL grants the current user full control, so the file is still
        // writable. If the ACE were wrong, this would fail.
        std::fs::write(&file, b"other").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"other");
    }

    #[cfg(windows)]
    #[test]
    fn a_restricted_file_can_still_be_deleted_by_the_current_user() {
        // Deleting a file needs DELETE permission on the *parent* directory,
        // so this exercises the file's own ACE as well: the file itself must
        // be openable for the delete to complete.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.txt");
        std::fs::write(&file, b"token").unwrap();
        restrict_to_current_user(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert!(!file.exists());
    }

    #[cfg(windows)]
    #[test]
    fn a_restricted_directory_can_still_hold_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        restrict_dir_to_current_user(&sub).unwrap();
        std::fs::write(sub.join("child.txt"), b"x").unwrap();
        assert!(sub.join("child.txt").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn the_applied_dacl_has_exactly_one_ace_and_no_inherited_entries() {
        // This is the assertion that actually proves the restriction. It
        // reads the DACL back off the file and checks that the only entry is
        // ours. If the `PROTECTED_DACL_SECURITY_INFORMATION` flag were
        // missing, the parent's "everyone else" entries would be merged in
        // and the count would be greater than one.
        use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
        use windows::Win32::Security::{
            ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
        };

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.txt");
        std::fs::write(&file, b"token").unwrap();
        restrict_to_current_user(&file).unwrap();

        let name = windows::core::HSTRING::from(file.as_os_str());
        let mut dacl: *mut windows::Win32::Security::ACL = std::ptr::null_mut();
        // SAFETY: the name is a live UTF-16 string, and the DACL pointer is
        // filled in by the API when the call succeeds.
        let status = unsafe {
            GetNamedSecurityInfoW(
                &name,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(&mut dacl),
                None,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status.0, 0, "could not read the DACL back: {}", status.0);
        assert!(!dacl.is_null(), "the file has no DACL at all");

        let mut info = ACL_SIZE_INFORMATION::default();
        // SAFETY: `dacl` is a live ACL and `info` is a correctly sized
        // buffer for the requested information class.
        let read = unsafe {
            windows::Win32::Security::GetAclInformation(
                dacl,
                &mut info as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        };
        assert!(read.is_ok(), "could not read the ACL size: {read:?}");
        assert_eq!(
            info.AceCount, 1,
            "the DACL has {} entries; only ours should be there",
            info.AceCount
        );

        // The single entry must be an allow-ACE that grants full access.
        let mut ace: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: index 0 exists because `AceCount` is 1, and the pointer is
        // filled in by the API.
        unsafe { windows::Win32::Security::GetAce(dacl, 0, &mut ace) }.unwrap();
        assert!(!ace.is_null());
        // SAFETY: an allow-ACE starts with a header followed by a 4-byte
        // access mask; the pointer is a live ACE of that shape.
        let allowed = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
        assert_eq!(allowed.Header.AceType, 0, "the entry is not an allow-ACE");
        assert_eq!(
            allowed.Mask, FILE_ALL_ACCESS,
            "the entry does not grant full access"
        );
    }
}
