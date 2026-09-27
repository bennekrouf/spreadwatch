//! The hot wallet: a Solana CLI keyfile that only the current user can read.
//! What "only" means per OS is in `private`.

use std::fs;
use std::io;
use std::path::Path;

use solana_keypair::{write_keypair, Keypair};

use super::private;

#[derive(Debug)]
pub enum LoadError {
    Missing,
    Invalid(String),
}

/// Refuses a keyfile other users could read: anyone with that file can spend.
pub fn load(path: &Path) -> Result<Keypair, LoadError> {
    match fs::metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(LoadError::Missing),
        Err(e) => return Err(LoadError::Invalid(e.to_string())),
    }
    match private::exposure(path) {
        Ok(None) => {}
        Ok(Some(why)) => {
            return Err(LoadError::Invalid(format!(
                "{} is readable by other users ({why}); run: {}",
                path.display(),
                private::fix_hint(path)
            )))
        }
        Err(e) => {
            return Err(LoadError::Invalid(format!(
                "cannot check who can read {}: {e}",
                path.display()
            )))
        }
    }
    solana_keypair::read_keypair_file(path).map_err(|e| LoadError::Invalid(e.to_string()))
}

/// Writes a fresh keypair. Never overwrites: an existing file may hold funds.
pub fn create(path: &Path) -> io::Result<Keypair> {
    if let Some(dir) = path.parent() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
    }

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    // Private before the key is written: Windows has no mode to create the
    // file with, so this is what protects it there. Give up rather than
    // write a key others can read.
    if let Err(e) = private::restrict(path) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(e);
    }

    let keypair = Keypair::new();
    write_keypair(&keypair, &mut file).map_err(|e| io::Error::other(e.to_string()))?;
    file.sync_all()?;
    Ok(keypair)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_signer::Signer;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spreadwatch-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("wallet.json")
    }

    #[test]
    fn create_then_load_round_trips_and_never_overwrites() {
        let path = temp_path("roundtrip");
        let created = create(&path).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(created.pubkey(), loaded.pubkey());
        assert!(create(&path).is_err());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_world_readable_keyfile() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_path("perms");
        create(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(load(&path), Err(LoadError::Invalid(_))));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_file_is_reported_as_missing() {
        assert!(matches!(
            load(&temp_path("missing")),
            Err(LoadError::Missing)
        ));
    }
}
