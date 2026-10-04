//! File modes for the catalog and `~/.bpm`: only the current user can read them.

use std::fs::{self, OpenOptions};
use std::path::Path;

/// Create an empty file readable and writable only by the current user. An empty
/// file is a valid empty SQLite database, so SQLite's `-wal` and `-shm` files,
/// which copy the database file's mode, start out private too.
pub fn create_user_file(path: &Path) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?;
    set_user_file(path)
}

pub fn set_user_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    let _ = path;
    Ok(())
}

pub fn set_user_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let _ = path;
    Ok(())
}

/// The permission bits of `path` when group or other users have any access to
/// it, or `None` when they have none or `path` does not exist.
pub fn shared_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).ok()?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Some(mode);
        }
    }
    let _ = path;
    None
}
