use std::net::IpAddr;

use thiserror::Error;

use super::rule::{BindAddress, Rule};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Field {
    RuleName,
    SshHostAlias,
    BindPort,
    DestinationHost,
    DestinationPort,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ValidationError {
    #[error("{field:?} must not be empty")]
    Empty { field: Field },
    #[error("{field:?} must not have leading or trailing whitespace")]
    SurroundingWhitespace { field: Field },
    #[error("{field:?} must not contain whitespace")]
    Whitespace { field: Field },
    #[error("{field:?} must not contain control characters")]
    ControlCharacter { field: Field },
    #[error("{field:?} must be at most {max_characters} characters")]
    TooLong { field: Field, max_characters: usize },
    #[error("{field:?} must not look like a command-line option")]
    LooksLikeOption { field: Field },
    #[error("port for {field:?} must be in the range 1..=65535")]
    PortZero { field: Field },
    #[error("invalid bind address: {value}")]
    InvalidBindAddress { value: String },
    #[error("invalid forwarding destination host: {value}")]
    InvalidDestinationHost { value: String },
    #[error("duplicate rule name: {name}")]
    DuplicateName { name: String },
    #[error("duplicate local listener: {address}:{port}")]
    DuplicateLocalListener { address: String, port: u16 },
}

pub fn validate_rule_set(rules: &[Rule]) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        if rules[..index]
            .iter()
            .any(|other| other.name() == rule.name())
        {
            errors.push(ValidationError::DuplicateName {
                name: rule.name().as_str().to_owned(),
            });
        }

        let Some((address, port)) = rule.local_listener() else {
            continue;
        };
        if rules[..index].iter().any(|other| {
            other
                .local_listener()
                .is_some_and(|(other_address, other_port)| {
                    port == other_port && bind_addresses_overlap(address, other_address)
                })
        }) {
            errors.push(ValidationError::DuplicateLocalListener {
                address: address.as_str().to_owned(),
                port: port.get(),
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn bind_addresses_overlap(left: &BindAddress, right: &BindAddress) -> bool {
    let left = BindingClass::from(left);
    let right = BindingClass::from(right);
    match (&left, &right) {
        (BindingClass::Any, _) | (_, BindingClass::Any) => true,
        (BindingClass::V4Any, BindingClass::V4(_) | BindingClass::Localhost)
        | (BindingClass::V4(_) | BindingClass::Localhost, BindingClass::V4Any)
        | (BindingClass::V6Any, BindingClass::V6(_) | BindingClass::Localhost)
        | (BindingClass::V6(_) | BindingClass::Localhost, BindingClass::V6Any) => true,
        (BindingClass::Localhost, BindingClass::Localhost) => true,
        (BindingClass::Localhost, BindingClass::V4(value))
        | (BindingClass::V4(value), BindingClass::Localhost) => value.is_loopback(),
        (BindingClass::Localhost, BindingClass::V6(value))
        | (BindingClass::V6(value), BindingClass::Localhost) => value.is_loopback(),
        (BindingClass::V4(left), BindingClass::V4(right)) => left == right,
        (BindingClass::V6(left), BindingClass::V6(right)) => left == right,
        _ => false,
    }
}

enum BindingClass {
    Any,
    Localhost,
    V4Any,
    V6Any,
    V4(std::net::Ipv4Addr),
    V6(std::net::Ipv6Addr),
}

impl From<&BindAddress> for BindingClass {
    fn from(value: &BindAddress) -> Self {
        match value.as_str() {
            "*" => Self::Any,
            "localhost" => Self::Localhost,
            other => match other.parse::<IpAddr>() {
                Ok(IpAddr::V4(address)) if address.is_unspecified() => Self::V4Any,
                Ok(IpAddr::V6(address)) if address.is_unspecified() => Self::V6Any,
                Ok(IpAddr::V4(address)) => Self::V4(address),
                Ok(IpAddr::V6(address)) => Self::V6(address),
                // BindAddress construction prevents this. Treating a broken
                // invariant as maximally overlapping keeps validation safe.
                Err(_) => Self::Any,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{ValidationError, validate_rule_set};
    use crate::domain::rule::{Rule, RuleId};

    fn id(number: u128) -> RuleId {
        RuleId::from_uuid(Uuid::from_u128(number))
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let rules = [
            Rule::dynamic(id(1), "proxy", "host", 1080).unwrap(),
            Rule::local(id(2), "proxy", "host", 3000, "localhost", 3000).unwrap(),
        ];
        let errors = validate_rule_set(&rules).unwrap_err();
        assert!(matches!(errors[0], ValidationError::DuplicateName { .. }));
    }

    #[test]
    fn overlapping_local_listeners_are_rejected() {
        let rules = [
            Rule::dynamic(id(1), "proxy", "host", 3000)
                .unwrap()
                .with_bind_address("0.0.0.0")
                .unwrap(),
            Rule::local(id(2), "web", "host", 3000, "localhost", 3000).unwrap(),
        ];
        let errors = validate_rule_set(&rules).unwrap_err();
        assert!(matches!(
            errors[0],
            ValidationError::DuplicateLocalListener { .. }
        ));
    }

    #[test]
    fn remote_listener_does_not_conflict_locally() {
        let rules = [
            Rule::dynamic(id(1), "proxy", "host", 3000).unwrap(),
            Rule::remote(id(2), "remote", "host", 3000, "localhost", 3000).unwrap(),
        ];
        assert_eq!(validate_rule_set(&rules), Ok(()));
    }

    #[test]
    fn distinct_addresses_and_ports_do_not_conflict() {
        let rules = [
            Rule::dynamic(id(1), "proxy", "host", 3000).unwrap(),
            Rule::local(id(2), "web", "host", 3001, "localhost", 3000).unwrap(),
            Rule::local(id(3), "other", "host", 3000, "localhost", 3000)
                .unwrap()
                .with_bind_address("192.0.2.1")
                .unwrap(),
        ];
        assert_eq!(validate_rule_set(&rules), Ok(()));
    }

    #[test]
    fn wildcard_and_ipv6_unspecified_bindings_overlap_their_address_families() {
        let wildcard_rules = [
            Rule::dynamic(id(1), "proxy", "host", 3000)
                .unwrap()
                .with_bind_address("*")
                .unwrap(),
            Rule::dynamic(id(2), "proxy-v6", "host", 3000)
                .unwrap()
                .with_bind_address("2001:db8::1")
                .unwrap(),
        ];
        assert!(matches!(
            validate_rule_set(&wildcard_rules).unwrap_err()[0],
            ValidationError::DuplicateLocalListener { .. }
        ));

        let ipv6_rules = [
            Rule::dynamic(id(3), "any-v6", "host", 3001)
                .unwrap()
                .with_bind_address("::")
                .unwrap(),
            Rule::dynamic(id(4), "loopback-v6", "host", 3001)
                .unwrap()
                .with_bind_address("::1")
                .unwrap(),
        ];
        assert!(matches!(
            validate_rule_set(&ipv6_rules).unwrap_err()[0],
            ValidationError::DuplicateLocalListener { .. }
        ));
    }
}
