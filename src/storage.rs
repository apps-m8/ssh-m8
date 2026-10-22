use crate::model::Library;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub struct Store {
    pub directory: PathBuf,
    _lock: File,
}

impl Drop for Store {
    fn drop(&mut self) {
        // Release immediately even if a forked child has not reached exec yet.
        let _ = self._lock.unlock();
    }
}

impl Store {
    pub fn open(directory: PathBuf) -> Result<(Self, Library), String> {
        fs::create_dir_all(&directory).map_err(|e| format!("Cannot create library folder: {e}"))?;
        let directory = fs::canonicalize(directory).map_err(|e| e.to_string())?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join(".lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock().map_err(|_| {
            "ssh-m8 is already using this library. Close the other window first.".to_string()
        })?;
        let store = Self {
            directory,
            _lock: lock,
        };
        let library = match fs::read(store.directory.join("library.json")) {
            Ok(bytes) => {
                let data: Library = serde_json::from_slice(&bytes).map_err(|e| {
                    format!("Cannot read library.json: {e}. Your file has been left untouched.")
                })?;
                data.validate()?;
                data
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Library::default(),
            Err(e) => return Err(format!("Cannot read the library: {e}")),
        };
        Ok((store, library))
    }

    pub fn save(&self, library: &Library) -> Result<(), String> {
        library.validate()?;
        let bytes = serde_json::to_vec_pretty(library).map_err(|e| e.to_string())?;
        atomic_write(&self.directory.join("library.json"), &bytes)
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().ok_or("Invalid library path")?)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist(path)
        .map_err(|e| format!("Cannot save the library: {}", e.error))?;
    Ok(())
}

pub fn default_directory() -> Result<PathBuf, String> {
    if let Some(path) =
        std::env::var_os("SSH_M8_DATA_DIR").or_else(|| std::env::var_os("SSH_MANAGER_DATA_DIR"))
    {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").ok_or("Cannot find your home folder.")?;
    let support = PathBuf::from(home).join("Library/Application Support");
    Ok(library_in(&support))
}

fn library_in(support: &Path) -> PathBuf {
    let current = support.join("ssh-m8");
    let legacy = support.join("SSH Manager");
    // Credential accounts include the library path; leave existing libraries in place.
    if !current.join("library.json").exists() && legacy.join("library.json").exists() {
        legacy
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rename_preserves_existing_library_and_credential_paths() {
        let support = tempfile::tempdir().unwrap();
        let current = support.path().join("ssh-m8");
        let legacy = support.path().join("SSH Manager");
        assert_eq!(library_in(support.path()), current);
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("library.json"), b"{}").unwrap();
        fs::create_dir(&current).unwrap();
        assert_eq!(library_in(support.path()), legacy);
        fs::write(current.join("library.json"), b"{}").unwrap();
        assert_eq!(library_in(support.path()), current);
    }

    #[test]
    fn save_is_private_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let (store, lib) = Store::open(dir.path().to_owned()).unwrap();
        store.save(&lib).unwrap();
        assert_eq!(
            fs::metadata(dir.path().join("library.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(Store::open(dir.path().to_owned()).is_err());
        drop(store);
        assert_eq!(Store::open(dir.path().to_owned()).unwrap().1.version, 1);
    }
    #[test]
    fn invalid_files_are_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("library.json"), b"broken").unwrap();
        assert!(Store::open(dir.path().to_owned()).is_err());
        assert_eq!(
            fs::read(dir.path().join("library.json")).unwrap(),
            b"broken"
        );
    }
}
