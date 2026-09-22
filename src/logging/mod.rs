//! Small bounded application log with fixed-count rotation.

use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    path::Path,
    sync::Mutex,
};

use crate::platform::private_fs::PrivateDirectory;

pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_BACKUPS: usize = 3;

pub struct RotatingLog {
    directory: PrivateDirectory,
    name: OsString,
    max_bytes: u64,
    backups: usize,
    writer: Mutex<()>,
}

impl RotatingLog {
    pub fn open(path: &Path, max_bytes: u64, backups: usize) -> io::Result<Self> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "log path has no parent directory",
            )
        })?;
        let name = path
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid log file name"))?
            .to_os_string();
        let log = Self {
            directory: PrivateDirectory::open(parent)?,
            name,
            max_bytes,
            backups,
            writer: Mutex::new(()),
        };
        log.validate()?;
        Ok(log)
    }

    /// Validates every path that rotation may read, replace, or remove.
    pub fn validate(&self) -> io::Result<()> {
        validate_log(&self.directory, &self.name)?;
        for index in 1..=self.backups {
            validate_log(&self.directory, &rotated_name(&self.name, index))?;
        }
        Ok(())
    }

    /// Writes an application-owned message. Callers must pass classified,
    /// fixed text rather than captured SSH output or environment values.
    pub fn write(&self, level: &str, message: &str) -> io::Result<()> {
        let line = format!("{}\t{}\n", single_line(level), single_line(message));
        if line.len() as u64 > self.max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log record exceeds the configured size limit",
            ));
        }
        let _writer = self
            .writer
            .lock()
            .map_err(|_| io::Error::other("log writer lock is unavailable"))?;
        self.validate()?;
        let should_rotate = match self.directory.read(&self.name) {
            Ok(file) => file.metadata()?.len() + line.len() as u64 > self.max_bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        if should_rotate {
            self.rotate()?;
        }
        let mut file = self.directory.append(&self.name)?;
        file.write_all(line.as_bytes())?;
        file.flush()
    }

    fn rotate(&self) -> io::Result<()> {
        if self.backups == 0 {
            return match self.directory.remove(&self.name) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            };
        }
        let oldest = rotated_name(&self.name, self.backups);
        match self.directory.remove(&oldest) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        for index in (1..self.backups).rev() {
            let from = rotated_name(&self.name, index);
            let to = rotated_name(&self.name, index + 1);
            match self.directory.rename(&from, &to) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        match self
            .directory
            .rename(&self.name, rotated_name(&self.name, 1))
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.directory.sync()?;
        Ok(())
    }
}

fn rotated_name(name: &OsStr, index: usize) -> OsString {
    let mut rotated = name.to_os_string();
    rotated.push(format!(".{index}"));
    rotated
}

fn validate_log(directory: &PrivateDirectory, name: &OsStr) -> io::Result<()> {
    match directory.read(name) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
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
    use std::{fs, os::unix::fs::PermissionsExt};

    fn private_tempdir() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    #[test]
    fn rotates_at_limit_and_never_writes_multiline_records() {
        let root = private_tempdir();
        let path = root.path().join("app.log");
        let log = RotatingLog::open(&path, 32, 2).unwrap();
        log.write("info\nforged", "first\nline").unwrap();
        log.write("info", "second").unwrap();
        log.write("info", "third").unwrap();
        assert!(
            path.with_file_name(rotated_name(path.file_name().unwrap(), 1))
                .exists()
        );
        assert!(!fs::read_to_string(&path).unwrap().contains('\r'));
        assert!(
            !fs::read_to_string(path.with_file_name(rotated_name(path.file_name().unwrap(), 1)))
                .unwrap()
                .contains("\nforged")
        );
    }

    #[test]
    fn rejects_unsafe_active_and_rotation_files() {
        use std::os::unix::fs::symlink;

        let root = private_tempdir();
        let path = root.path().join("app.log");
        let log = RotatingLog::open(&path, 64, 2).unwrap();
        fs::write(&path, "unsafe").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(log.write("info", "event").is_err());

        fs::remove_file(&path).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, "keep").unwrap();
        symlink(
            &outside,
            path.with_file_name(rotated_name(path.file_name().unwrap(), 1)),
        )
        .unwrap();
        assert!(log.write("info", "event").is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "keep");
    }

    #[test]
    fn enforces_mode_size_generations_and_rejects_hardlinks() {
        let root = private_tempdir();
        let path = root.path().join("app.log");
        let log = RotatingLog::open(&path, 20, 2).unwrap();
        for _ in 0..5 {
            log.write("info", "event").unwrap();
        }
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert!(fs::metadata(&path).unwrap().len() <= 20);
        assert!(
            fs::metadata(path.with_file_name(rotated_name(path.file_name().unwrap(), 1)))
                .unwrap()
                .len()
                <= 20
        );
        assert!(
            path.with_file_name(rotated_name(path.file_name().unwrap(), 2))
                .exists()
        );
        assert!(
            !path
                .with_file_name(rotated_name(path.file_name().unwrap(), 3))
                .exists()
        );

        let linked = root.path().join("linked");
        fs::hard_link(&path, &linked).unwrap();
        assert_eq!(
            log.write("info", "event").unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn rotated_path_preserves_non_utf8_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let name = std::ffi::OsString::from_vec(b"log-\xff".to_vec());
        assert_eq!(rotated_name(&name, 2).as_os_str().as_bytes(), b"log-\xff.2");
    }

    #[test]
    fn retained_directory_descriptor_survives_parent_path_replacement() {
        let root = private_tempdir();
        let visible = root.path().join("logs");
        fs::create_dir(&visible).unwrap();
        fs::set_permissions(&visible, fs::Permissions::from_mode(0o700)).unwrap();
        let path = visible.join("app.log");
        let log = RotatingLog::open(&path, 64, 2).unwrap();

        let held = root.path().join("held-logs");
        fs::rename(&visible, &held).unwrap();
        fs::create_dir(&visible).unwrap();
        fs::set_permissions(&visible, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&path, "replacement").unwrap();

        log.write("info", "event").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
        assert_eq!(
            fs::read_to_string(held.join("app.log")).unwrap(),
            "info\tevent\n"
        );
    }
}
