//! Versioned desired-configuration contract.

use serde::{Deserialize, Deserializer, Serialize, de};
use uuid::Uuid;

mod conversion;
mod store;
pub use store::{ConfigStore, Migration, PersistedConfig, StoreError};

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub const fn allows(self, event: Self) -> bool {
        event.priority() <= self.priority()
    }

    const fn priority(self) -> u8 {
        match self {
            Self::Error => 0,
            Self::Warn => 1,
            Self::Info => 2,
            Self::Debug => 3,
            Self::Trace => 4,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub theme: Theme,
    pub log_level: LogLevel,
    pub default_reconnect: bool,
    pub default_auto_start: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigV2 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub settings: Settings,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigV1 {
    #[serde(deserialize_with = "deserialize_v1")]
    pub schema_version: u32,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

fn deserialize_v1<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == 1 {
        Ok(version)
    } else {
        Err(de::Error::custom("expected schema version 1"))
    }
}

fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == SCHEMA_VERSION {
        Ok(version)
    } else {
        Err(de::Error::custom(format!(
            "unsupported schema version {version}; expected {SCHEMA_VERSION}"
        )))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Rule {
    Local {
        id: Uuid,
        name: String,
        ssh_host_alias: String,
        bind_address: String,
        bind_port: u16,
        destination_host: String,
        destination_port: u16,
        auto_start: bool,
        reconnect: bool,
    },
    Remote {
        id: Uuid,
        name: String,
        ssh_host_alias: String,
        bind_address: String,
        bind_port: u16,
        destination_host: String,
        destination_port: u16,
        auto_start: bool,
        reconnect: bool,
    },
    Dynamic {
        id: Uuid,
        name: String,
        ssh_host_alias: String,
        bind_address: String,
        bind_port: u16,
        auto_start: bool,
        reconnect: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::{ConfigV2, Rule, SCHEMA_VERSION};

    #[test]
    fn fixture_round_trips_without_irrelevant_fields() {
        let fixture = include_str!("../../tests/fixtures/config_v1.toml");
        let config: super::ConfigV1 = toml::from_str(fixture).expect("valid fixture");

        assert_eq!(config.schema_version, 1);
        assert_eq!(config.rules.len(), 3);
        assert!(matches!(config.rules[2], Rule::Dynamic { .. }));

        let domain = config.clone().into_domain().expect("valid domain rules");
        let converted = ConfigV2::from_domain(&domain, Default::default()).unwrap();
        assert_eq!(converted.schema_version, SCHEMA_VERSION);

        let encoded = toml::to_string_pretty(&converted).expect("serialize configuration");
        let reparsed: ConfigV2 = toml::from_str(&encoded).expect("round-trip configuration");
        assert_eq!(reparsed, converted);

        let dynamic_table = encoded
            .split("[[rules]]")
            .nth(3)
            .expect("dynamic rule table");
        assert!(!dynamic_table.contains("destination_"));
    }

    #[test]
    fn unknown_schema_version_is_rejected() {
        let error = toml::from_str::<ConfigV2>("schema_version = 99\n")
            .expect_err("unknown schema must fail");
        assert!(error.to_string().contains("unsupported schema version 99"));
    }

    #[test]
    fn unknown_rule_fields_are_rejected() {
        let fixture = include_str!("../../tests/fixtures/config_v1.toml");
        let modified = fixture.replacen("reconnect = false", "reconnect = false\nextra = true", 1);
        assert!(toml::from_str::<super::ConfigV1>(&modified).is_err());
    }
}
