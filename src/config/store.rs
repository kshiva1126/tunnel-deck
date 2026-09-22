use super::{ConfigV1, SCHEMA_VERSION};
use crate::{
    domain::{rule::Rule, validation::ValidationError},
    platform::private_fs::PrivateDirectory,
};
use std::{
    io::{self, Read, Write},
    path::Path,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("configuration I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid configuration TOML: {0}")]
    Decode(#[from] toml::de::Error),
    #[error("configuration serialization failed: {0}")]
    Encode(#[from] toml::ser::Error),
    #[error("unsupported schema version {0}")]
    UnsupportedSchema(u32),
    #[error("invalid rule: {0}")]
    InvalidRule(#[from] ValidationError),
    #[error("invalid rule set: {0:?}")]
    InvalidRules(Vec<ValidationError>),
    #[error("configuration was replaced, but directory sync failed: {0}")]
    DurabilityUncertain(io::Error),
}

/// Explicit migration registration; no historical format is shipped in v1.
/// The caller must only supply a migration for a documented older schema.
pub trait Migration {
    fn source_version(&self) -> u32;
    fn migrate(&self, original: &str) -> Result<ConfigV1, StoreError>;
}

/// Persistence boundary for the future sole writer (the daemon). Opening a
/// store does not confer a daemon lock; clients must not use it for mutations.
pub struct ConfigStore {
    directory: PrivateDirectory,
}

impl ConfigStore {
    pub fn open(directory: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            directory: PrivateDirectory::open(directory)?,
        })
    }

    /// Remove one known abandoned save after the caller has excluded live writers
    /// (the future daemon lock). Never removes backups, config, or unrelated files.
    pub fn discard_abandoned_save(&mut self, id: uuid::Uuid) -> Result<(), StoreError> {
        let name = format!(".config.{id}.tmp");
        self.directory.read(&name)?;
        self.directory.remove(&name)?;
        self.directory.sync()?;
        Ok(())
    }

    pub fn load(&self) -> Result<Option<Vec<Rule>>, StoreError> {
        self.read_original()?.map(|text| decode(&text)).transpose()
    }

    pub fn save(&mut self, rules: &[Rule]) -> Result<(), StoreError> {
        let text = toml::to_string_pretty(&ConfigV1::from_domain(rules)?)?;
        // Never silently overwrite unknown schemas or invalid existing data.
        self.load()?;
        self.replace(text.as_bytes(), |_| Ok(()))
    }

    /// Backup is created exclusively and synced before invoking migration.
    /// On migration failure it is retained and the original remains untouched.
    pub fn load_with_migration(
        &mut self,
        migration: &dyn Migration,
    ) -> Result<Option<Vec<Rule>>, StoreError> {
        let Some(original) = self.read_original()? else {
            return Ok(None);
        };
        let version = schema(&original)?;
        if version == SCHEMA_VERSION {
            return decode(&original).map(Some);
        }
        if version >= SCHEMA_VERSION || version != migration.source_version() {
            return Err(StoreError::UnsupportedSchema(version));
        }
        let backup_name = format!("config.toml.v{version}.{}.bak", uuid::Uuid::new_v4());
        let mut backup = Temporary::new(&self.directory, backup_name)?;
        backup.file.write_all(original.as_bytes())?;
        backup.file.flush()?;
        backup.file.sync_all()?;
        self.directory.sync()?;
        backup.keep = true;
        let rules = migration.migrate(&original)?.into_domain()?;
        let text = toml::to_string_pretty(&ConfigV1::from_domain(&rules)?)?;
        self.replace(text.as_bytes(), |_| Ok(()))?;
        Ok(Some(rules))
    }

    fn read_original(&self) -> Result<Option<String>, StoreError> {
        let mut file = match self.directory.read("config.toml") {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(Some(text))
    }

    fn replace(
        &self,
        bytes: &[u8],
        mut checkpoint: impl FnMut(Stage) -> io::Result<()>,
    ) -> Result<(), StoreError> {
        let name = format!(".config.{}.tmp", uuid::Uuid::new_v4());
        let mut temporary = Temporary::new(&self.directory, name)?;
        checkpoint(Stage::Created)?;
        temporary.file.write_all(bytes)?;
        checkpoint(Stage::Written)?;
        temporary.file.flush()?;
        temporary.file.sync_all()?;
        checkpoint(Stage::Synced)?;
        // Validate the current destination immediately before replacement.
        match self.directory.read("config.toml") {
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        self.directory.rename(&temporary.name, "config.toml")?;
        temporary.keep = true;
        checkpoint(Stage::Renamed)
            .and_then(|()| self.directory.sync())
            .map_err(StoreError::DurabilityUncertain)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Created,
    Written,
    Synced,
    Renamed,
}

struct Temporary<'a> {
    directory: &'a PrivateDirectory,
    name: String,
    file: std::fs::File,
    keep: bool,
}
impl<'a> Temporary<'a> {
    fn new(directory: &'a PrivateDirectory, name: String) -> io::Result<Self> {
        let file = directory.create(&name)?;
        Ok(Self {
            directory,
            name,
            file,
            keep: false,
        })
    }
}
impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = self.directory.remove(&self.name);
        }
    }
}

fn schema(text: &str) -> Result<u32, StoreError> {
    #[derive(serde::Deserialize)]
    struct Header {
        schema_version: u32,
    }
    Ok(toml::from_str::<Header>(text)?.schema_version)
}
fn decode(text: &str) -> Result<Vec<Rule>, StoreError> {
    let version = schema(text)?;
    if version != SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema(version));
    }
    toml::from_str::<ConfigV1>(text)?.into_domain()
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
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    fn fixture() -> Vec<Rule> {
        decode(include_str!("../../tests/fixtures/config_v1.toml")).unwrap()
    }
    fn entries(path: &Path) -> usize {
        fs::read_dir(path).unwrap().count()
    }

    #[test]
    fn round_trip_and_permissions_in_isolated_directory() {
        let root = private_tempdir();
        let path = root.path().join("nested/config");
        let mut store = ConfigStore::open(&path).unwrap();
        assert_eq!(store.load().unwrap(), None);
        let mut rules = fixture();
        rules[0] = rules[0]
            .clone()
            .with_bind_address("::1")
            .unwrap()
            .with_policy(true, true);
        store.save(&rules).unwrap();
        assert_eq!(store.load().unwrap(), Some(rules.clone()));
        let dto: ConfigV1 =
            toml::from_str(&fs::read_to_string(path.join("config.toml")).unwrap()).unwrap();
        assert_eq!(dto, ConfigV1::from_domain(&rules).unwrap());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(path.join("config.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(entries(&path), 1);
    }

    #[test]
    fn failure_before_rename_preserves_original_and_removes_temporary() {
        for stage in [Stage::Created, Stage::Written, Stage::Synced] {
            let root = private_tempdir();
            let mut store = ConfigStore::open(root.path()).unwrap();
            store.save(&fixture()).unwrap();
            let original = fs::read(root.path().join("config.toml")).unwrap();
            assert!(
                store
                    .replace(b"schema_version = 1\n", |at| if at == stage {
                        Err(io::Error::other("injected write failure"))
                    } else {
                        Ok(())
                    })
                    .is_err()
            );
            assert_eq!(fs::read(root.path().join("config.toml")).unwrap(), original);
            assert_eq!(entries(root.path()), 1);
        }
    }

    #[test]
    fn rename_failure_preserves_original_and_cleans_up() {
        let root = private_tempdir();
        let mut store = ConfigStore::open(root.path()).unwrap();
        store.save(&fixture()).unwrap();
        let original = fs::read(root.path().join("config.toml")).unwrap();
        let result = store.replace(b"schema_version = 1\n", |stage| {
            if stage == Stage::Synced {
                // Remove the source to force renameat itself to fail, even
                // when tests run as root and permission failures are bypassed.
                let temporary = fs::read_dir(root.path())?
                    .map(|entry| entry.unwrap().path())
                    .find(|path| path.extension().is_some_and(|ext| ext == "tmp"))
                    .unwrap();
                fs::remove_file(temporary)?;
            }
            Ok(())
        });
        assert!(matches!(result, Err(StoreError::Io(error))
            if error.kind() == io::ErrorKind::NotFound));
        assert_eq!(fs::read(root.path().join("config.toml")).unwrap(), original);
        assert_eq!(entries(root.path()), 1);
    }

    #[test]
    fn unsafe_abandoned_save_is_not_deleted() {
        let root = private_tempdir();
        let mut store = ConfigStore::open(root.path()).unwrap();
        store.save(&fixture()).unwrap();
        let id = uuid::Uuid::new_v4();
        let name = root.path().join(format!(".config.{id}.tmp"));
        symlink(root.path().join("config.toml"), &name).unwrap();
        assert!(store.discard_abandoned_save(id).is_err());
        assert!(fs::symlink_metadata(&name).unwrap().is_symlink());
        assert_eq!(store.load().unwrap(), Some(fixture()));
    }

    #[test]
    fn sync_failure_after_rename_reports_committed_but_uncertain() {
        let root = private_tempdir();
        let mut store = ConfigStore::open(root.path()).unwrap();
        store.save(&fixture()).unwrap();
        assert!(matches!(
            store.replace(b"schema_version = 1\n", |at| if at == Stage::Renamed {
                Err(io::Error::other("sync failure"))
            } else {
                Ok(())
            }),
            Err(StoreError::DurabilityUncertain(_))
        ));
        assert_eq!(store.load().unwrap(), Some(vec![]));
        assert_eq!(entries(root.path()), 1);
    }

    #[test]
    fn unknown_schema_and_invalid_domain_never_overwrite_original() {
        let root = private_tempdir();
        let mut store = ConfigStore::open(root.path()).unwrap();
        for text in [
            "schema_version = 99\n".to_owned(),
            include_str!("../../tests/fixtures/config_v1.toml")
                .replace("bind_port = 1080", "bind_port = 0"),
        ] {
            store.replace(text.as_bytes(), |_| Ok(())).unwrap();
            assert!(store.load().is_err());
            assert!(store.save(&fixture()).is_err());
            assert_eq!(
                fs::read_to_string(root.path().join("config.toml")).unwrap(),
                text
            );
        }
    }

    #[test]
    fn unsafe_files_and_directories_are_rejected_without_repair() {
        let root = private_tempdir();
        let directory = root.path().join("config");
        let mut store = ConfigStore::open(&directory).unwrap();
        store.save(&fixture()).unwrap();
        let file = directory.join("config.toml");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.load().is_err());
        assert!(store.save(&fixture()).is_err());
        fs::remove_file(&file).unwrap();
        let target = root.path().join("unrelated");
        fs::write(&target, "untouched").unwrap();
        symlink(&target, &file).unwrap();
        assert!(store.load().is_err());
        assert!(store.save(&fixture()).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ConfigStore::open(&directory).is_err());
        let link = root.path().join("link");
        symlink(&directory, &link).unwrap();
        assert!(ConfigStore::open(&link).is_err());
        assert!(ConfigStore::open(&link.join("")).is_err());
    }

    struct TestMigration {
        fail: bool,
    }
    impl Migration for TestMigration {
        fn source_version(&self) -> u32 {
            0
        }
        fn migrate(&self, _: &str) -> Result<ConfigV1, StoreError> {
            if self.fail {
                return Err(io::Error::other("migration failed").into());
            }
            ConfigV1::from_domain(&fixture())
        }
    }
    #[test]
    fn migration_keeps_exact_private_backup_on_success_and_failure() {
        for fail in [false, true] {
            let root = private_tempdir();
            let mut store = ConfigStore::open(root.path()).unwrap();
            let original = b"schema_version = 0\n# test-only migration input\n";
            store.replace(original, |_| Ok(())).unwrap();
            let result = store.load_with_migration(&TestMigration { fail });
            assert_eq!(result.is_err(), fail);
            let backup = fs::read_dir(root.path())
                .unwrap()
                .map(|v| v.unwrap().path())
                .find(|p| p.extension().is_some_and(|v| v == "bak"))
                .unwrap();
            assert_eq!(fs::read(&backup).unwrap(), original);
            assert_eq!(
                fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
                0o600
            );
            if fail {
                assert_eq!(fs::read(root.path().join("config.toml")).unwrap(), original);
            } else {
                assert_eq!(store.load().unwrap(), Some(fixture()));
            }
        }
    }

    #[test]
    fn invalid_migration_output_preserves_original_and_backup_exists_before_conversion() {
        struct InvalidMigration<'a> {
            directory: &'a Path,
        }
        impl Migration for InvalidMigration<'_> {
            fn source_version(&self) -> u32 {
                0
            }

            fn migrate(&self, original: &str) -> Result<ConfigV1, StoreError> {
                let backups: Vec<_> = fs::read_dir(self.directory)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| path.extension().is_some_and(|ext| ext == "bak"))
                    .collect();
                assert_eq!(backups.len(), 1);
                assert_eq!(fs::read_to_string(&backups[0]).unwrap(), original);
                assert_eq!(
                    fs::read_to_string(self.directory.join("config.toml")).unwrap(),
                    original
                );
                // Syntactically valid DTO output must still pass domain validation.
                Ok(toml::from_str(
                    &include_str!("../../tests/fixtures/config_v1.toml")
                        .replace("bind_port = 1080", "bind_port = 0"),
                )
                .unwrap())
            }
        }

        let root = private_tempdir();
        let mut store = ConfigStore::open(root.path()).unwrap();
        let original = b"schema_version = 0\n# keep this exact source\n";
        store.replace(original, |_| Ok(())).unwrap();
        assert!(matches!(
            store.load_with_migration(&InvalidMigration {
                directory: root.path(),
            }),
            Err(StoreError::InvalidRule(ValidationError::PortZero { .. }))
        ));
        assert_eq!(fs::read(root.path().join("config.toml")).unwrap(), original);
        assert_eq!(entries(root.path()), 2);
    }

    #[test]
    fn abandoned_temporary_file_is_never_loaded_or_reused() {
        let root = private_tempdir();
        let mut store = ConfigStore::open(root.path()).unwrap();
        store.save(&fixture()).unwrap();
        // Simulate process death after partial write; leave the orphan behind.
        let id = uuid::Uuid::new_v4();
        let name = format!(".config.{id}.tmp");
        let mut orphan = store.directory.create(&name).unwrap();
        orphan.write_all(b"partial TOML").unwrap();
        drop(orphan);
        drop(store);
        let mut reopened = ConfigStore::open(root.path()).unwrap();
        assert_eq!(reopened.load().unwrap(), Some(fixture()));
        reopened.save(&[]).unwrap();
        assert_eq!(reopened.load().unwrap(), Some(vec![]));
        assert_eq!(fs::read(root.path().join(&name)).unwrap(), b"partial TOML");
        // No broad automatic deletion: a future daemon lock must precede orphan
        // collection. A private orphan can be explicitly removed by its owner.
        reopened.discard_abandoned_save(id).unwrap();
        assert_eq!(entries(root.path()), 1);
    }
    #[test]
    fn invalid_rule_set_does_not_replace_valid_configuration() {
        let root = private_tempdir();
        let mut store = ConfigStore::open(root.path()).unwrap();
        let valid = fixture();
        store.save(&valid).unwrap();
        let invalid = vec![valid[0].clone(), valid[0].clone()];
        assert!(matches!(
            store.save(&invalid),
            Err(StoreError::InvalidRules(_))
        ));
        assert_eq!(store.load().unwrap(), Some(valid));
        assert_eq!(entries(root.path()), 1);
    }

    #[test]
    fn future_schema_is_not_migrated_or_backed_up() {
        let root = private_tempdir();
        let mut store = ConfigStore::open(root.path()).unwrap();
        store.replace(b"schema_version = 2\n", |_| Ok(())).unwrap();
        assert!(matches!(
            store.load_with_migration(&TestMigration { fail: false }),
            Err(StoreError::UnsupportedSchema(2))
        ));
        assert_eq!(entries(root.path()), 1);
        assert_eq!(
            fs::read(root.path().join("config.toml")).unwrap(),
            b"schema_version = 2\n"
        );
    }

    #[test]
    fn hard_link_is_rejected_and_held_directory_is_used_after_path_replacement() {
        let root = private_tempdir();
        let original_dir = root.path().join("config");
        let moved_dir = root.path().join("moved");
        let mut store = ConfigStore::open(&original_dir).unwrap();
        store.save(&fixture()).unwrap();
        let link = root.path().join("hardlink");
        fs::hard_link(original_dir.join("config.toml"), &link).unwrap();
        assert!(store.load().is_err());
        assert!(store.save(&[]).is_err());
        fs::remove_file(link).unwrap();
        fs::rename(&original_dir, &moved_dir).unwrap();
        let _replacement = ConfigStore::open(&original_dir).unwrap();
        store.save(&[]).unwrap();
        assert_eq!(store.load().unwrap(), Some(vec![]));
        assert!(moved_dir.join("config.toml").exists());
        assert!(!original_dir.join("config.toml").exists());
    }
}
