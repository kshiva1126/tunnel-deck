//! Small bounded application log with fixed-count rotation.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_BACKUPS: usize = 3;

pub struct RotatingLog {
    path: PathBuf,
    max_bytes: u64,
    backups: usize,
}

impl RotatingLog {
    pub fn new(path: impl Into<PathBuf>, max_bytes: u64, backups: usize) -> Self {
        Self {
            path: path.into(),
            max_bytes,
            backups,
        }
    }

    /// Writes an application-owned message. Callers must pass classified,
    /// fixed text rather than captured SSH output or environment values.
    pub fn write(&self, level: &str, message: &str) -> io::Result<()> {
        let line = format!("{}\t{}\n", single_line(level), single_line(message));
        if fs::metadata(&self.path)
            .map(|m| m.len() + line.len() as u64 > self.max_bytes)
            .unwrap_or(false)
        {
            self.rotate()?;
        }
        validate_log(&self.path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.path)?;
        validate_metadata(&file.metadata()?)?;
        file.write_all(line.as_bytes())
    }

    fn rotate(&self) -> io::Result<()> {
        if self.backups == 0 {
            return match fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            };
        }
        for index in (1..self.backups).rev() {
            let from = rotated(&self.path, index);
            let to = rotated(&self.path, index + 1);
            if from.exists() {
                fs::rename(from, to)?;
            }
        }
        if self.path.exists() {
            fs::rename(&self.path, rotated(&self.path, 1))?;
        }
        Ok(())
    }
}

fn rotated(path: &Path, index: usize) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(format!(".{index}"));
    PathBuf::from(value)
}
fn validate_log(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_metadata(&metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
fn validate_metadata(metadata: &fs::Metadata) -> io::Result<()> {
    if !metadata.is_file()
        || metadata.uid() != crate::platform::private_fs::current_uid()
        || metadata.permissions().mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe log file ownership, type, permissions, or links",
        ))
    } else {
        Ok(())
    }
}
fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotates_at_limit_and_never_writes_multiline_records() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("app.log");
        let log = RotatingLog::new(&path, 12, 2);
        log.write("info\nforged", "first\nline").unwrap();
        log.write("info", "second").unwrap();
        assert!(rotated(&path, 1).exists());
        assert!(!fs::read_to_string(&path).unwrap().contains('\r'));
        assert!(
            !fs::read_to_string(rotated(&path, 1))
                .unwrap()
                .contains("\nforged")
        );
    }

    #[test]
    fn rotated_path_preserves_non_utf8_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let path = PathBuf::from(std::ffi::OsString::from_vec(b"log-\xff".to_vec()));
        assert_eq!(rotated(&path, 2).as_os_str().as_bytes(), b"log-\xff.2");
    }
}
