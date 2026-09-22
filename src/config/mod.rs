//! Versioned desired-configuration contract.

use serde::{Deserialize, Deserializer, Serialize, de};
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub rules: Vec<Rule>,
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
    use super::{ConfigV1, Rule, SCHEMA_VERSION};

    #[test]
    fn fixture_round_trips_without_irrelevant_fields() {
        let fixture = include_str!("../../tests/fixtures/config_v1.toml");
        let config: ConfigV1 = toml::from_str(fixture).expect("valid fixture");

        assert_eq!(config.schema_version, SCHEMA_VERSION);
        assert_eq!(config.rules.len(), 3);
        assert!(matches!(config.rules[2], Rule::Dynamic { .. }));

        let encoded = toml::to_string_pretty(&config).expect("serialize configuration");
        let reparsed: ConfigV1 = toml::from_str(&encoded).expect("round-trip configuration");
        assert_eq!(reparsed, config);

        let dynamic_table = encoded
            .split("[[rules]]")
            .nth(3)
            .expect("dynamic rule table");
        assert!(!dynamic_table.contains("destination_"));
    }

    #[test]
    fn unknown_schema_version_is_rejected() {
        let error = toml::from_str::<ConfigV1>("schema_version = 2\n")
            .expect_err("unknown schema must fail");
        assert!(error.to_string().contains("unsupported schema version 2"));
    }

    #[test]
    fn unknown_rule_fields_are_rejected() {
        let fixture = include_str!("../../tests/fixtures/config_v1.toml");
        let modified = fixture.replacen("reconnect = false", "reconnect = false\nextra = true", 1);
        assert!(toml::from_str::<ConfigV1>(&modified).is_err());
    }
}
