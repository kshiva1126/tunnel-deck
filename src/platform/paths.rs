//! Path selection from explicit inputs, with filesystem checks for runtime overrides.
use super::Platform;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paths {
    pub config: PathBuf,
    pub log: PathBuf,
    pub runtime: PathBuf,
}

#[derive(Default)]
pub struct XdgOverrides<'a> {
    pub config: Option<&'a Path>,
    pub state: Option<&'a Path>,
    pub runtime: Option<&'a Path>,
}

impl Paths {
    /// Does not read environment variables or create directories. Relative XDG
    /// values are ignored. Runtime overrides must also pass ownership checks.
    pub fn resolve(
        platform: Platform,
        home: &Path,
        uid: u32,
        xdg: XdgOverrides<'_>,
    ) -> std::io::Result<Self> {
        if !home.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "home must be absolute",
            ));
        }
        let config = xdg
            .config
            .filter(|p| p.is_absolute())
            .map(|p| p.join("tunnel-deck"))
            .unwrap_or_else(|| {
                home.join(match platform {
                    Platform::Linux => ".config/tunnel-deck",
                    Platform::MacOs => "Library/Application Support/TunnelDeck",
                })
            });
        let state = xdg
            .state
            .filter(|p| p.is_absolute())
            .map(|p| p.join("tunnel-deck"))
            .unwrap_or_else(|| {
                home.join(match platform {
                    Platform::Linux => ".local/state/tunnel-deck",
                    Platform::MacOs => "Library/Logs/TunnelDeck",
                })
            });
        let runtime = xdg
            .runtime
            .filter(|p| p.is_absolute() && super::private_fs::validate_directory(p, uid).is_ok())
            .map(|p| p.join("tunnel-deck"))
            .unwrap_or_else(|| {
                PathBuf::from(match platform {
                    Platform::Linux => "/tmp",
                    Platform::MacOs => "/private/tmp",
                })
                .join(format!("tunnel-deck-{uid}"))
            });
        Ok(Self {
            config: config.join("config.toml"),
            log: state.join("tunnel-deck.log"),
            runtime,
        })
    }

    pub fn socket_path(&self, platform: Platform) -> std::io::Result<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        let path = self.runtime.join("tdeck.sock");
        let limit = match platform {
            Platform::Linux => 108,
            Platform::MacOs => 104,
        };
        if path.as_os_str().as_bytes().len() >= limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Unix socket path is too long",
            ));
        }
        Ok(path)
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
    use crate::platform::private_fs::current_uid;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn both_platform_defaults_are_resolved_without_reading_home() {
        let home = Path::new("/injected/home");
        let linux = Paths::resolve(Platform::Linux, home, 123, XdgOverrides::default()).unwrap();
        assert_eq!(linux.config, home.join(".config/tunnel-deck/config.toml"));
        assert_eq!(
            linux.log,
            home.join(".local/state/tunnel-deck/tunnel-deck.log")
        );
        assert_eq!(linux.runtime, Path::new("/tmp/tunnel-deck-123"));
        let mac = Paths::resolve(Platform::MacOs, home, 123, XdgOverrides::default()).unwrap();
        assert_eq!(
            mac.config,
            home.join("Library/Application Support/TunnelDeck/config.toml")
        );
        assert_eq!(
            mac.log,
            home.join("Library/Logs/TunnelDeck/tunnel-deck.log")
        );
        assert_eq!(mac.runtime, Path::new("/private/tmp/tunnel-deck-123"));
    }

    #[test]
    fn absolute_overrides_work_on_both_platforms_and_relative_values_are_ignored() {
        let root = private_tempdir();
        for platform in [Platform::Linux, Platform::MacOs] {
            let paths = Paths::resolve(
                platform,
                root.path(),
                current_uid(),
                XdgOverrides {
                    config: Some(root.path()),
                    state: Some(root.path()),
                    runtime: Some(root.path()),
                },
            )
            .unwrap();
            assert_eq!(paths.config, root.path().join("tunnel-deck/config.toml"));
            assert_eq!(paths.log, root.path().join("tunnel-deck/tunnel-deck.log"));
            assert_eq!(paths.runtime, root.path().join("tunnel-deck"));
            let relative = Some(Path::new("relative"));
            assert_eq!(
                Paths::resolve(
                    platform,
                    root.path(),
                    current_uid(),
                    XdgOverrides {
                        config: relative,
                        state: relative,
                        runtime: relative
                    }
                )
                .unwrap(),
                Paths::resolve(
                    platform,
                    root.path(),
                    current_uid(),
                    XdgOverrides::default()
                )
                .unwrap()
            );
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn unsafe_runtime_override_falls_back() {
        let root = private_tempdir();
        let runtime = root.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let link = root.path().join("link");
        symlink(&runtime, &link).unwrap();
        let resolve = |path: &Path, uid| {
            Paths::resolve(
                Platform::Linux,
                root.path(),
                uid,
                XdgOverrides {
                    runtime: Some(path),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        assert_eq!(
            resolve(&link, current_uid()).runtime,
            PathBuf::from(format!("/tmp/tunnel-deck-{}", current_uid()))
        );
        let other_uid = current_uid().wrapping_add(1);
        assert_eq!(
            resolve(&runtime, other_uid).runtime,
            PathBuf::from(format!("/tmp/tunnel-deck-{other_uid}"))
        );
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            resolve(&runtime, current_uid()).runtime,
            PathBuf::from(format!("/tmp/tunnel-deck-{}", current_uid()))
        );
    }

    #[test]
    fn socket_length_includes_multibyte_paths_and_terminator() {
        for (platform, limit) in [(Platform::Linux, 108), (Platform::MacOs, 104)] {
            let mut paths = Paths {
                config: PathBuf::new(),
                log: PathBuf::new(),
                runtime: PathBuf::from(format!("/{}", "a".repeat(limit - 13))),
            };
            assert!(paths.socket_path(platform).is_ok());
            paths.runtime.push("é");
            assert!(paths.socket_path(platform).is_err());
        }
    }
}
