//! Unix filesystem operations shared by Linux and macOS. Operations within an
//! application directory use its open descriptor, never a re-resolved pathname.
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

pub fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

fn check(metadata: &fs::Metadata, uid: u32, directory: bool) -> io::Result<()> {
    let mode = if directory { 0o700 } else { 0o600 };
    if metadata.uid() != uid
        || metadata.mode() & 0o7777 != mode
        || (if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.nlink() != 1
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe file type, ownership, permissions, or hard links",
        ));
    }
    Ok(())
}

pub fn validate_directory(path: &Path, uid: u32) -> io::Result<()> {
    let path: PathBuf = path.components().collect();
    check(&fs::symlink_metadata(path)?, uid, true)
}

pub struct PrivateDirectory {
    file: File,
    uid: u32,
}
impl PrivateDirectory {
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory must be absolute",
            ));
        }
        // Strip trailing separators/dot components so O_NOFOLLOW applies to
        // the application directory itself, including caller-supplied paths.
        let path: PathBuf = path.components().collect();
        // Ancestors (HOME, Library, XDG roots) are not application-owned and
        // are never chmod'ed. Newly created ancestors are private.
        create_directories(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let uid = current_uid();
        check(&file.metadata()?, uid, true)?;
        Ok(Self { file, uid })
    }

    pub fn read(&self, name: &str) -> io::Result<File> {
        let file = self.open_file(name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        check(&file.metadata()?, self.uid, false)?;
        Ok(file)
    }

    pub fn create(&self, name: &str) -> io::Result<File> {
        let file = self.open_file(name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600)?;
        if let Err(error) = check(&file.metadata()?, self.uid, false) {
            let _ = self.remove(name);
            return Err(error);
        }
        Ok(file)
    }

    fn open_file(&self, name: &str, flags: i32, mode: libc::mode_t) -> io::Result<File> {
        check(&self.file.metadata()?, self.uid, true)?;
        let name = component(name)?;
        // SAFETY: the directory descriptor and C string remain valid. The
        // returned descriptor is owned exactly once by File.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub fn rename(&self, source: &str, target: &str) -> io::Result<()> {
        let source = component(source)?;
        let target = component(target)?;
        // SAFETY: all descriptors and strings remain valid during the call.
        let result = unsafe {
            libc::renameat(
                self.file.as_raw_fd(),
                source.as_ptr(),
                self.file.as_raw_fd(),
                target.as_ptr(),
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn remove(&self, name: &str) -> io::Result<()> {
        let name = component(name)?;
        // SAFETY: descriptor and C string are valid; flags select a file.
        let result = unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

fn component(name: &str) -> io::Result<CString> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one file name",
        ));
    }
    CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid file name"))
}

fn create_directories(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "application directory is not a directory or is a symlink",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                create_directories(parent)?;
            }
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {
                    if let Some(parent) = path.parent() {
                        File::open(parent)?.sync_all()?;
                    }
                    Ok(())
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    validate_directory(path, current_uid())
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    fn private_tempdir() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
    use super::*;
    #[test]
    fn wrong_owner_is_rejected_for_files_and_directories() {
        let root = private_tempdir();
        let directory = PrivateDirectory::open(root.path()).unwrap();
        let file = directory.create("private").unwrap();
        let other = current_uid().wrapping_add(1);
        assert!(check(&file.metadata().unwrap(), other, false).is_err());
        assert!(validate_directory(root.path(), other).is_err());
    }
}
