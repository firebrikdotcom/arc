use crate::FfiError;
use std::path::{Path, PathBuf};

/// Normalized database identity, independent of credentials and configuration.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CoreKey(Identity);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Identity {
    Sqlite(PathBuf),
    Postgres(String),
}

impl CoreKey {
    /// Resolve filesystem identity synchronously; call off a UI thread.
    pub fn sqlite_file(path: PathBuf) -> Result<Self, FfiError> {
        let invalid = || FfiError::InvalidDatabasePath.logged("database_identity");
        let input = path.to_str().ok_or_else(invalid)?;
        if input.is_empty() || input.contains(':') || input.contains('?') || input.contains('#') {
            return Err(invalid());
        }
        let resolved = match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let resolved = path.canonicalize().map_err(|_| invalid())?;
                if !resolved.is_file() {
                    return Err(invalid());
                }
                resolved
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = path.file_name().ok_or_else(invalid)?;
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let parent = parent.canonicalize().map_err(|_| invalid())?;
                if !parent.is_dir() {
                    return Err(invalid());
                }
                parent.join(name)
            }
            Err(_) => return Err(invalid()),
        };
        resolved.to_str().ok_or_else(invalid)?;
        Ok(Self(Identity::Sqlite(resolved)))
    }

    /// App-assigned stable, nonsecret identity; DSNs are captured separately.
    pub fn postgres(namespace: String) -> Result<Self, FfiError> {
        if namespace.trim().is_empty() {
            return Err(FfiError::InvalidConfiguration.logged("database_identity"));
        }
        Ok(Self(Identity::Postgres(namespace)))
    }

    pub fn sqlite_path(&self) -> Option<&Path> {
        match &self.0 {
            Identity::Sqlite(path) => Some(path),
            Identity::Postgres(_) => None,
        }
    }
}
