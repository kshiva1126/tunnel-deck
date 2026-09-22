use std::{fmt, net::IpAddr, num::NonZeroU16, str::FromStr};

use uuid::Uuid;

use super::{
    host::HostAlias,
    validation::{Field, ValidationError},
};

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RuleId(Uuid);

impl RuleId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for RuleId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RuleName(String);

impl RuleName {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        validate_text(&value, Field::RuleName, 128, true)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuleName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Port(NonZeroU16);

impl Port {
    pub fn new(value: u16, field: Field) -> Result<Self, ValidationError> {
        NonZeroU16::new(value)
            .map(Self)
            .ok_or(ValidationError::PortZero { field })
    }

    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl fmt::Display for Port {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct BindAddress(String);

impl BindAddress {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value == "localhost" || value == "*" || IpAddr::from_str(&value).is_ok() {
            return Ok(Self(value));
        }
        Err(ValidationError::InvalidBindAddress { value })
    }

    pub fn loopback() -> Self {
        Self(DEFAULT_BIND_ADDRESS.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BindAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DestinationHost(String);

impl DestinationHost {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        validate_text(&value, Field::DestinationHost, 253, false)?;
        if value.contains(':') && IpAddr::from_str(&value).is_err() {
            return Err(ValidationError::InvalidDestinationHost { value });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DestinationHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rule {
    id: RuleId,
    name: RuleName,
    ssh_host_alias: HostAlias,
    forwarding: Forwarding,
    auto_start: bool,
    reconnect: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Forwarding {
    Local(LocalForwarding),
    Remote(RemoteForwarding),
    Dynamic(DynamicForwarding),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalForwarding {
    bind_address: BindAddress,
    bind_port: Port,
    destination_host: DestinationHost,
    destination_port: Port,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteForwarding {
    bind_address: BindAddress,
    bind_port: Port,
    destination_host: DestinationHost,
    destination_port: Port,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicForwarding {
    bind_address: BindAddress,
    bind_port: Port,
}

impl Rule {
    pub fn local(
        id: RuleId,
        name: impl Into<String>,
        ssh_host_alias: impl Into<String>,
        bind_port: u16,
        destination_host: impl Into<String>,
        destination_port: u16,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            id,
            name: RuleName::new(name)?,
            ssh_host_alias: HostAlias::new(ssh_host_alias)?,
            forwarding: Forwarding::Local(LocalForwarding {
                bind_address: BindAddress::loopback(),
                bind_port: Port::new(bind_port, Field::BindPort)?,
                destination_host: DestinationHost::new(destination_host)?,
                destination_port: Port::new(destination_port, Field::DestinationPort)?,
            }),
            auto_start: false,
            reconnect: false,
        })
    }

    pub fn remote(
        id: RuleId,
        name: impl Into<String>,
        ssh_host_alias: impl Into<String>,
        bind_port: u16,
        destination_host: impl Into<String>,
        destination_port: u16,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            id,
            name: RuleName::new(name)?,
            ssh_host_alias: HostAlias::new(ssh_host_alias)?,
            forwarding: Forwarding::Remote(RemoteForwarding {
                bind_address: BindAddress::loopback(),
                bind_port: Port::new(bind_port, Field::BindPort)?,
                destination_host: DestinationHost::new(destination_host)?,
                destination_port: Port::new(destination_port, Field::DestinationPort)?,
            }),
            auto_start: false,
            reconnect: false,
        })
    }

    pub fn dynamic(
        id: RuleId,
        name: impl Into<String>,
        ssh_host_alias: impl Into<String>,
        bind_port: u16,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            id,
            name: RuleName::new(name)?,
            ssh_host_alias: HostAlias::new(ssh_host_alias)?,
            forwarding: Forwarding::Dynamic(DynamicForwarding {
                bind_address: BindAddress::loopback(),
                bind_port: Port::new(bind_port, Field::BindPort)?,
            }),
            auto_start: false,
            reconnect: false,
        })
    }

    pub fn with_bind_address(
        mut self,
        bind_address: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let bind_address = BindAddress::new(bind_address)?;
        match &mut self.forwarding {
            Forwarding::Local(value) => value.bind_address = bind_address,
            Forwarding::Remote(value) => value.bind_address = bind_address,
            Forwarding::Dynamic(value) => value.bind_address = bind_address,
        }
        Ok(self)
    }

    pub fn with_policy(mut self, auto_start: bool, reconnect: bool) -> Self {
        self.auto_start = auto_start;
        self.reconnect = reconnect;
        self
    }

    pub fn rename(&mut self, name: impl Into<String>) -> Result<(), ValidationError> {
        self.name = RuleName::new(name)?;
        Ok(())
    }

    pub const fn id(&self) -> RuleId {
        self.id
    }

    pub fn name(&self) -> &RuleName {
        &self.name
    }

    pub fn ssh_host_alias(&self) -> &HostAlias {
        &self.ssh_host_alias
    }

    pub fn forwarding(&self) -> &Forwarding {
        &self.forwarding
    }

    pub const fn auto_start(&self) -> bool {
        self.auto_start
    }

    pub const fn reconnect(&self) -> bool {
        self.reconnect
    }

    pub fn local_listener(&self) -> Option<(&BindAddress, Port)> {
        match &self.forwarding {
            Forwarding::Local(value) => Some((&value.bind_address, value.bind_port)),
            Forwarding::Dynamic(value) => Some((&value.bind_address, value.bind_port)),
            Forwarding::Remote(_) => None,
        }
    }
}

impl LocalForwarding {
    pub fn bind_address(&self) -> &BindAddress {
        &self.bind_address
    }
    pub const fn bind_port(&self) -> Port {
        self.bind_port
    }
    pub fn destination_host(&self) -> &DestinationHost {
        &self.destination_host
    }
    pub const fn destination_port(&self) -> Port {
        self.destination_port
    }
}

impl RemoteForwarding {
    pub fn bind_address(&self) -> &BindAddress {
        &self.bind_address
    }
    pub const fn bind_port(&self) -> Port {
        self.bind_port
    }
    pub fn destination_host(&self) -> &DestinationHost {
        &self.destination_host
    }
    pub const fn destination_port(&self) -> Port {
        self.destination_port
    }
}

impl DynamicForwarding {
    pub fn bind_address(&self) -> &BindAddress {
        &self.bind_address
    }
    pub const fn bind_port(&self) -> Port {
        self.bind_port
    }
}

fn validate_text(
    value: &str,
    field: Field,
    max_characters: usize,
    allow_spaces: bool,
) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Empty { field });
    }
    if value.trim() != value {
        return Err(ValidationError::SurroundingWhitespace { field });
    }
    if value.chars().count() > max_characters {
        return Err(ValidationError::TooLong {
            field,
            max_characters,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ValidationError::ControlCharacter { field });
    }
    if !allow_spaces && value.chars().any(char::is_whitespace) {
        return Err(ValidationError::Whitespace { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{BindAddress, Forwarding, Rule, RuleId};
    use crate::domain::validation::{Field, ValidationError};

    fn id() -> RuleId {
        RuleId::from_uuid(Uuid::from_u128(1))
    }

    #[test]
    fn new_ids_use_uuid_v4() {
        assert_eq!(RuleId::new().as_uuid().get_version_num(), 4);
    }

    #[test]
    fn all_variants_are_typed_and_default_to_safe_policy() {
        let local = Rule::local(id(), "web", "server", 3000, "localhost", 3000).unwrap();
        let remote = Rule::remote(id(), "callback", "server", 8080, "127.0.0.1", 8080).unwrap();
        let dynamic = Rule::dynamic(id(), "proxy", "server", 1080).unwrap();

        for rule in [&local, &remote, &dynamic] {
            assert!(!rule.auto_start());
            assert!(!rule.reconnect());
            assert_eq!(rule.ssh_host_alias().as_str(), "server");
        }
        assert!(matches!(local.forwarding(), Forwarding::Local(_)));
        assert!(matches!(remote.forwarding(), Forwarding::Remote(_)));
        assert!(matches!(dynamic.forwarding(), Forwarding::Dynamic(_)));
        assert_eq!(local.local_listener().unwrap().0.as_str(), "127.0.0.1");
    }

    #[test]
    fn policy_and_explicit_non_loopback_bind_require_opt_in() {
        let rule = Rule::dynamic(id(), "proxy", "server", 1080)
            .unwrap()
            .with_bind_address("0.0.0.0")
            .unwrap()
            .with_policy(true, true);
        assert_eq!(rule.local_listener().unwrap().0.as_str(), "0.0.0.0");
        assert!(rule.auto_start());
        assert!(rule.reconnect());
    }

    #[test]
    fn invalid_names_aliases_addresses_hosts_and_ports_are_rejected() {
        assert!(matches!(
            Rule::dynamic(id(), "", "server", 1080),
            Err(ValidationError::Empty {
                field: Field::RuleName
            })
        ));
        assert!(matches!(
            Rule::dynamic(id(), "proxy", "-oProxyCommand=bad", 1080),
            Err(ValidationError::LooksLikeOption {
                field: Field::SshHostAlias
            })
        ));
        assert!(matches!(
            Rule::dynamic(id(), "proxy", "bad alias", 1080),
            Err(ValidationError::Whitespace {
                field: Field::SshHostAlias
            })
        ));
        assert!(matches!(
            Rule::dynamic(id(), "proxy", "server", 0),
            Err(ValidationError::PortZero {
                field: Field::BindPort
            })
        ));
        assert!(matches!(
            Rule::local(id(), "web", "server", 3000, "bad:host", 3000),
            Err(ValidationError::InvalidDestinationHost { .. })
        ));
        assert!(matches!(
            BindAddress::new("not an address"),
            Err(ValidationError::InvalidBindAddress { .. })
        ));
    }

    #[test]
    fn text_validation_rejects_whitespace_control_characters_and_length() {
        assert_eq!(
            Rule::dynamic(id(), " proxy", "server", 1080).unwrap_err(),
            ValidationError::SurroundingWhitespace {
                field: Field::RuleName,
            }
        );
        assert_eq!(
            Rule::dynamic(id(), "pro\nxy", "server", 1080).unwrap_err(),
            ValidationError::ControlCharacter {
                field: Field::RuleName,
            }
        );
        assert_eq!(
            Rule::dynamic(id(), "x".repeat(129), "server", 1080).unwrap_err(),
            ValidationError::TooLong {
                field: Field::RuleName,
                max_characters: 128,
            }
        );
        assert_eq!(
            Rule::local(id(), "web", "server", 3000, "web server", 80).unwrap_err(),
            ValidationError::Whitespace {
                field: Field::DestinationHost,
            }
        );
        assert_eq!(
            Rule::local(id(), "web", "server", 3000, "x".repeat(254), 80).unwrap_err(),
            ValidationError::TooLong {
                field: Field::DestinationHost,
                max_characters: 253,
            }
        );
        assert_eq!(
            Rule::local(id(), "web", "server", 3000, "host", 0).unwrap_err(),
            ValidationError::PortZero {
                field: Field::DestinationPort,
            }
        );
    }

    #[test]
    fn ipv6_addresses_are_accepted_without_brackets() {
        let rule = Rule::local(id(), "web", "server", 3000, "::1", 3000)
            .unwrap()
            .with_bind_address("::1")
            .unwrap();
        assert_eq!(rule.local_listener().unwrap().0.as_str(), "::1");
    }

    #[test]
    fn rule_name_can_be_changed_only_to_another_valid_name() {
        let mut rule = Rule::dynamic(id(), "proxy", "server", 1080).unwrap();
        rule.rename("browser proxy").unwrap();
        assert_eq!(rule.name().as_str(), "browser proxy");
        assert!(rule.rename(" ").is_err());
        assert_eq!(rule.name().as_str(), "browser proxy");
    }
}
