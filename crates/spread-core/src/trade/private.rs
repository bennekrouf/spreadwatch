//! Files only the current user may read: the hot wallet and `trade.toml`.
//!
//! Anyone who can read the keyfile can spend from it, and `trade.toml` can
//! hold the Jupiter API key. On macOS and Linux that means no group or other
//! permission bits. On Windows there are no mode bits, so the file's access
//! list is read instead: an entry letting Everyone, Users, Authenticated
//! Users, Guests or Anonymous read it is the Windows equivalent of `chmod 644`.

use std::io;
use std::path::Path;

/// Why other users can read `path`, or `None` when only its owner can.
pub fn exposure(path: &Path) -> io::Result<Option<String>> {
    imp::exposure(path)
}

/// The command that makes `path` private, for messages that ask the user to.
pub fn fix_hint(path: &Path) -> String {
    imp::fix_hint(path)
}

/// Makes `path` readable by the current user only.
pub fn restrict(path: &Path) -> io::Result<()> {
    imp::restrict(path)
}

#[cfg(unix)]
mod imp {
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    pub fn exposure(path: &Path) -> io::Result<Option<String>> {
        let mode = fs::metadata(path)?.permissions().mode() & 0o777;
        Ok((mode & 0o077 != 0).then(|| format!("mode {mode:o}")))
    }

    pub fn fix_hint(path: &Path) -> String {
        format!("chmod 600 {}", path.display())
    }

    pub fn restrict(path: &Path) -> io::Result<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::Command;
    use std::ptr;

    use windows_sys::Win32::Foundation::{LocalFree, GENERIC_ALL, GENERIC_READ};
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        GetAce, IsWellKnownSid, WinAnonymousSid, WinAuthenticatedUserSid, WinBuiltinGuestsSid,
        WinBuiltinUsersSid, WinWorldSid, ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, WELL_KNOWN_SID_TYPE,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_READ_DATA;

    /// Keeps `icacls` and `whoami` from flashing a console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const READ_RIGHTS: u32 = FILE_READ_DATA | GENERIC_READ | GENERIC_ALL;
    const SHARED: [(WELL_KNOWN_SID_TYPE, &str); 5] = [
        (WinWorldSid, "Everyone"),
        (WinBuiltinUsersSid, "Users"),
        (WinAuthenticatedUserSid, "Authenticated Users"),
        (WinBuiltinGuestsSid, "Guests"),
        (WinAnonymousSid, "Anonymous"),
    ];

    pub fn exposure(path: &Path) -> io::Result<Option<String>> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated; on success `sd` owns the buffer
        // `dacl` points into and is released with LocalFree below.
        let err = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut sd,
            )
        };
        if err != 0 {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        let found = unsafe { shared_reader(dacl) };
        // SAFETY: `sd` was allocated by GetNamedSecurityInfoW.
        unsafe { LocalFree(sd) };
        Ok(found)
    }

    /// SAFETY: `dacl` is null or a valid ACL.
    unsafe fn shared_reader(dacl: *mut ACL) -> Option<String> {
        if dacl.is_null() {
            return Some("it has no access list, so everyone can read it".into());
        }
        for i in 0..(*dacl).AceCount as u32 {
            let mut ace: *mut core::ffi::c_void = ptr::null_mut();
            if GetAce(dacl, i, &mut ace) == 0 {
                continue;
            }
            let ace = ace as *const ACCESS_ALLOWED_ACE;
            if (*ace).Header.AceType != ACCESS_ALLOWED_ACE_TYPE || (*ace).Mask & READ_RIGHTS == 0 {
                continue;
            }
            let sid = ptr::addr_of!((*ace).SidStart) as *mut core::ffi::c_void;
            if let Some((_, name)) = SHARED
                .iter()
                .find(|(kind, _)| IsWellKnownSid(sid, *kind) != 0)
            {
                return Some(format!("{name} can read it"));
            }
        }
        None
    }

    pub fn fix_hint(path: &Path) -> String {
        format!(
            "icacls \"{}\" /inheritance:r /grant:r \"%USERNAME%:F\"",
            path.display()
        )
    }

    /// Drops inherited entries and grants only the current user, named by
    /// SID: account names are localised and can be ambiguous on a domain.
    pub fn restrict(path: &Path) -> io::Result<()> {
        let sid = current_user_sid()?;
        let out = Command::new("icacls")
            .arg(path)
            .args(["/inheritance:r", "/grant:r", &format!("*{sid}:F")])
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "icacls failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        match exposure(path)? {
            None => Ok(()),
            Some(why) => Err(io::Error::other(format!(
                "still shared after icacls: {why}"
            ))),
        }
    }

    /// `whoami /user /fo csv /nh` prints `"DOMAIN\user","S-1-5-21-..."`.
    fn current_user_sid() -> io::Result<String> {
        let out = Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.trim()
            .rsplit(',')
            .next()
            .map(|s| s.trim_matches('"').to_owned())
            .filter(|s| s.starts_with("S-1-"))
            .ok_or_else(|| {
                io::Error::other(format!(
                    "could not read the current user's SID from whoami: {}",
                    text.trim()
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn restricted_file_is_private() {
        let dir = std::env::temp_dir().join(format!("spreadwatch-private-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret.json");
        fs::write(&path, "[]").unwrap();
        restrict(&path).unwrap();
        assert_eq!(exposure(&path).unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn world_readable_file_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("spreadwatch-shared-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret.json");
        fs::write(&path, "[]").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(exposure(&path).unwrap().as_deref(), Some("mode 644"));
        let _ = fs::remove_dir_all(&dir);
    }
}
