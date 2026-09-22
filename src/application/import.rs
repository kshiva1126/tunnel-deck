//! Side-effect-free preview of forwarding directives reported by `ssh -G`.

use std::collections::{HashMap, HashSet};

use crate::domain::{
    host::HostAlias,
    rule::{BindAddress, DestinationHost, Forwarding, Rule, RuleId},
    validation::bind_addresses_overlap,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ImportForwarding {
    Local {
        bind_address: String,
        bind_port: u16,
        destination_host: String,
        destination_port: u16,
    },
    Remote {
        bind_address: String,
        bind_port: u16,
        destination_host: String,
        destination_port: u16,
    },
    Dynamic {
        bind_address: String,
        bind_port: u16,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectiveKind {
    Local,
    Remote,
    Dynamic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportClassification {
    Supported,
    DuplicateDirective {
        first_index: usize,
    },
    DuplicateRule {
        rule_id: RuleId,
        running: bool,
    },
    Conflict {
        rule_id: Option<RuleId>,
        running: bool,
    },
    Unsupported {
        reason: UnsupportedReason,
    },
    Invalid {
        reason: InvalidReason,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedReason {
    UnixSocket,
    RemoteDynamic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidReason {
    Syntax,
    BindAddress,
    DestinationHost,
    Port,
    NonUtf8Output,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportCandidate {
    pub kind: DirectiveKind,
    pub forwarding: Option<ImportForwarding>,
    pub classification: ImportClassification,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportPreview {
    pub ssh_host_alias: String,
    pub candidates: Vec<ImportCandidate>,
}

pub fn preview_effective_forwards(
    alias: &str,
    output: &[u8],
    existing_rules: &[Rule],
    running_rule_ids: &[RuleId],
) -> Result<ImportPreview, InvalidReason> {
    HostAlias::new(alias).map_err(|_| InvalidReason::Syntax)?;
    let text = std::str::from_utf8(output).map_err(|_| InvalidReason::NonUtf8Output)?;
    let running: HashSet<_> = running_rule_ids.iter().copied().collect();
    let local_default_bind = effective_local_default_bind(text);
    let mut seen = HashMap::<ImportForwarding, usize>::new();
    let mut accepted = Vec::<ImportForwarding>::new();
    let mut candidates = Vec::new();

    for line in text.lines() {
        let Some((keyword, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let kind = match keyword.to_ascii_lowercase().as_str() {
            "localforward" => DirectiveKind::Local,
            "remoteforward" => DirectiveKind::Remote,
            "dynamicforward" => DirectiveKind::Dynamic,
            _ => continue,
        };
        let parsed = parse_forward(kind, value.trim(), local_default_bind);
        let (forwarding, classification) = match parsed {
            Err(ParseFailure::Unsupported(reason)) => {
                (None, ImportClassification::Unsupported { reason })
            }
            Err(ParseFailure::Invalid(reason)) => (None, ImportClassification::Invalid { reason }),
            Ok(forwarding) => {
                let classification = classify(
                    alias,
                    &forwarding,
                    &seen,
                    &accepted,
                    existing_rules,
                    &running,
                );
                if matches!(classification, ImportClassification::Supported) {
                    accepted.push(forwarding.clone());
                }
                seen.entry(forwarding.clone()).or_insert(candidates.len());
                (Some(forwarding), classification)
            }
        };
        candidates.push(ImportCandidate {
            kind,
            forwarding,
            classification,
        });
    }
    Ok(ImportPreview {
        ssh_host_alias: alias.to_owned(),
        candidates,
    })
}

fn classify(
    alias: &str,
    forwarding: &ImportForwarding,
    seen: &HashMap<ImportForwarding, usize>,
    accepted: &[ImportForwarding],
    existing: &[Rule],
    running: &HashSet<RuleId>,
) -> ImportClassification {
    if let Some(first_index) = seen.get(forwarding).copied() {
        return ImportClassification::DuplicateDirective { first_index };
    }
    for rule in existing
        .iter()
        .filter(|rule| rule.ssh_host_alias().as_str() == alias)
    {
        if forwarding_matches_rule(forwarding, rule) {
            return ImportClassification::DuplicateRule {
                rule_id: rule.id(),
                running: running.contains(&rule.id()),
            };
        }
    }
    if accepted
        .iter()
        .any(|other| forwards_conflict(forwarding, other))
    {
        return ImportClassification::Conflict {
            rule_id: None,
            running: false,
        };
    }
    for rule in existing {
        if forwarding_conflicts_rule(alias, forwarding, rule) {
            return ImportClassification::Conflict {
                rule_id: Some(rule.id()),
                running: running.contains(&rule.id()),
            };
        }
    }
    ImportClassification::Supported
}

fn effective_local_default_bind(output: &str) -> &'static str {
    if output.lines().any(|line| {
        line.split_once(char::is_whitespace)
            .is_some_and(|(key, value)| {
                key.eq_ignore_ascii_case("gatewayports") && value.trim() == "yes"
            })
    }) {
        "*"
    } else {
        "localhost"
    }
}

fn parse_forward(
    kind: DirectiveKind,
    value: &str,
    local_default_bind: &str,
) -> Result<ImportForwarding, ParseFailure> {
    let fields: Vec<_> = value.split_whitespace().collect();
    if fields.iter().any(|field| looks_like_socket(field)) {
        return Err(ParseFailure::Unsupported(UnsupportedReason::UnixSocket));
    }
    match (kind, fields.as_slice()) {
        (DirectiveKind::Local, [listen, destination]) => {
            let (bind_address, bind_port) = parse_listener(listen, local_default_bind)?;
            let (destination_host, destination_port) = parse_destination(destination)?;
            Ok(ImportForwarding::Local {
                bind_address,
                bind_port,
                destination_host,
                destination_port,
            })
        }
        (DirectiveKind::Remote, [listen, destination]) => {
            let (bind_address, bind_port) = parse_listener(listen, "localhost")?;
            let (destination_host, destination_port) = parse_destination(destination)?;
            Ok(ImportForwarding::Remote {
                bind_address,
                bind_port,
                destination_host,
                destination_port,
            })
        }
        (DirectiveKind::Remote, [listen]) => {
            parse_listener(listen, "localhost")?;
            Err(ParseFailure::Unsupported(UnsupportedReason::RemoteDynamic))
        }
        (DirectiveKind::Dynamic, [listen]) => {
            let (bind_address, bind_port) = parse_listener(listen, local_default_bind)?;
            Ok(ImportForwarding::Dynamic {
                bind_address,
                bind_port,
            })
        }
        _ => Err(ParseFailure::Invalid(InvalidReason::Syntax)),
    }
}

fn looks_like_socket(value: &str) -> bool {
    value.contains('/')
}

fn parse_listener(value: &str, default_bind: &str) -> Result<(String, u16), ParseFailure> {
    if value.chars().all(|character| character.is_ascii_digit()) {
        return Ok((default_bind.to_owned(), parse_port(value)?));
    }
    let (address, port) = split_endpoint(value)?;
    BindAddress::new(address.clone())
        .map_err(|_| ParseFailure::Invalid(InvalidReason::BindAddress))?;
    Ok((address, port))
}

fn parse_destination(value: &str) -> Result<(String, u16), ParseFailure> {
    let (host, port) = split_endpoint(value)?;
    DestinationHost::new(host.clone())
        .map_err(|_| ParseFailure::Invalid(InvalidReason::DestinationHost))?;
    Ok((host, port))
}

fn split_endpoint(value: &str) -> Result<(String, u16), ParseFailure> {
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, rest) = rest
            .split_once(']')
            .ok_or(ParseFailure::Invalid(InvalidReason::Syntax))?;
        let port = rest
            .strip_prefix(':')
            .ok_or(ParseFailure::Invalid(InvalidReason::Syntax))?;
        (host, port)
    } else {
        value
            .rsplit_once(':')
            .ok_or(ParseFailure::Invalid(InvalidReason::Syntax))?
    };
    if host.is_empty() {
        return Err(ParseFailure::Invalid(InvalidReason::Syntax));
    }
    Ok((host.to_owned(), parse_port(port)?))
}

fn parse_port(value: &str) -> Result<u16, ParseFailure> {
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or(ParseFailure::Invalid(InvalidReason::Port))
}

fn forwarding_matches_rule(forwarding: &ImportForwarding, rule: &Rule) -> bool {
    match (forwarding, rule.forwarding()) {
        (
            ImportForwarding::Local {
                bind_address,
                bind_port,
                destination_host,
                destination_port,
            },
            Forwarding::Local(rule),
        ) => {
            bind_address == rule.bind_address().as_str()
                && *bind_port == rule.bind_port().get()
                && destination_host == rule.destination_host().as_str()
                && *destination_port == rule.destination_port().get()
        }
        (
            ImportForwarding::Remote {
                bind_address,
                bind_port,
                destination_host,
                destination_port,
            },
            Forwarding::Remote(rule),
        ) => {
            bind_address == rule.bind_address().as_str()
                && *bind_port == rule.bind_port().get()
                && destination_host == rule.destination_host().as_str()
                && *destination_port == rule.destination_port().get()
        }
        (
            ImportForwarding::Dynamic {
                bind_address,
                bind_port,
            },
            Forwarding::Dynamic(rule),
        ) => bind_address == rule.bind_address().as_str() && *bind_port == rule.bind_port().get(),
        _ => false,
    }
}

fn forwarding_conflicts_rule(alias: &str, forwarding: &ImportForwarding, rule: &Rule) -> bool {
    match forwarding {
        ImportForwarding::Local { .. } | ImportForwarding::Dynamic { .. } => {
            rule.local_listener().is_some_and(|(address, port)| {
                listener(forwarding).is_some_and(|candidate| {
                    candidate.1 == port.get() && bind_addresses_overlap(&candidate.0, address)
                })
            })
        }
        ImportForwarding::Remote {
            bind_address,
            bind_port,
            ..
        } => {
            rule.ssh_host_alias().as_str() == alias
                && matches!(rule.forwarding(), Forwarding::Remote(remote)
                if remote.bind_port().get() == *bind_port
                    && BindAddress::new(bind_address.clone()).is_ok_and(|candidate|
                        bind_addresses_overlap(&candidate, remote.bind_address())))
        }
    }
}

fn forwards_conflict(left: &ImportForwarding, right: &ImportForwarding) -> bool {
    match (left, right) {
        (ImportForwarding::Remote { .. }, ImportForwarding::Remote { .. }) => {
            listeners_conflict(left, right)
        }
        (ImportForwarding::Remote { .. }, _) | (_, ImportForwarding::Remote { .. }) => false,
        _ => listeners_conflict(left, right),
    }
}

fn listeners_conflict(left: &ImportForwarding, right: &ImportForwarding) -> bool {
    let (Some((left_address, left_port)), Some((right_address, right_port))) =
        (listener(left), listener(right))
    else {
        return false;
    };
    left_port == right_port && bind_addresses_overlap(&left_address, &right_address)
}

fn listener(forwarding: &ImportForwarding) -> Option<(BindAddress, u16)> {
    let (address, port) = match forwarding {
        ImportForwarding::Local {
            bind_address,
            bind_port,
            ..
        }
        | ImportForwarding::Remote {
            bind_address,
            bind_port,
            ..
        }
        | ImportForwarding::Dynamic {
            bind_address,
            bind_port,
        } => (bind_address, *bind_port),
    };
    BindAddress::new(address.clone())
        .ok()
        .map(|address| (address, port))
}

enum ParseFailure {
    Unsupported(UnsupportedReason),
    Invalid(InvalidReason),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use uuid::Uuid;

    use super::*;

    fn id(value: u128) -> RuleId {
        RuleId::from_uuid(Uuid::from_u128(value))
    }

    #[test]
    fn fixture_classifies_all_forms_without_mutating_snapshots() {
        let output = fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/ssh_effective_forwards.txt"
        ))
        .unwrap();
        let existing = vec![
            Rule::dynamic(id(1), "existing", "sample", 1080)
                .unwrap()
                .with_bind_address("::1")
                .unwrap(),
            Rule::local(id(2), "occupied", "other-host", 3001, "localhost", 9)
                .unwrap()
                .with_bind_address("::")
                .unwrap(),
        ];
        let before = existing.clone();
        let preview = preview_effective_forwards("sample", &output, &existing, &[id(1)]).unwrap();

        assert_eq!(existing, before);
        assert_eq!(preview.candidates.len(), 12);
        assert!(matches!(
            preview.candidates[0].classification,
            ImportClassification::Supported
        ));
        assert!(matches!(
            preview.candidates[1].classification,
            ImportClassification::Supported
        ));
        assert!(matches!(
            preview.candidates[2].classification,
            ImportClassification::DuplicateRule { running: true, .. }
        ));
        assert_eq!(
            preview.candidates[3].classification,
            ImportClassification::DuplicateDirective { first_index: 0 }
        );
        assert!(matches!(
            preview.candidates[4].classification,
            ImportClassification::Conflict { rule_id: None, .. }
        ));
        assert!(matches!(
            preview.candidates[5].classification,
            ImportClassification::Conflict {
                rule_id: Some(_),
                ..
            }
        ));
        assert!(matches!(
            preview.candidates[6].classification,
            ImportClassification::Unsupported {
                reason: UnsupportedReason::UnixSocket
            }
        ));
        assert!(matches!(
            preview.candidates[7].classification,
            ImportClassification::Unsupported {
                reason: UnsupportedReason::RemoteDynamic
            }
        ));
        assert!(matches!(
            preview.candidates[8].classification,
            ImportClassification::Invalid {
                reason: InvalidReason::Port
            }
        ));
        assert!(matches!(
            preview.candidates[9].classification,
            ImportClassification::Invalid {
                reason: InvalidReason::BindAddress
            }
        ));
        assert!(matches!(
            preview.candidates[10].classification,
            ImportClassification::Invalid {
                reason: InvalidReason::DestinationHost
            }
        ));
        assert!(matches!(
            preview.candidates[11].classification,
            ImportClassification::Invalid {
                reason: InvalidReason::Syntax
            }
        ));
    }

    #[test]
    fn omitted_bind_and_ipv4_ipv6_are_normalized_as_typed_candidates() {
        let preview = preview_effective_forwards(
            "sample",
            b"localforward 3000 [::1]:80\nremoteforward [0.0.0.0]:9000 [example.com]:90\n",
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(
            preview.candidates[0].forwarding,
            Some(ImportForwarding::Local {
                bind_address: "localhost".to_owned(),
                bind_port: 3000,
                destination_host: "::1".to_owned(),
                destination_port: 80,
            })
        );
        assert!(matches!(
            preview.candidates[1].forwarding,
            Some(ImportForwarding::Remote { ref bind_address, .. }) if bind_address == "0.0.0.0"
        ));
    }

    #[test]
    fn non_utf8_effective_output_is_a_typed_error() {
        assert_eq!(
            preview_effective_forwards("sample", &[0xff], &[], &[]),
            Err(InvalidReason::NonUtf8Output)
        );
    }

    #[test]
    fn gateway_ports_changes_only_omitted_local_listener_defaults() {
        let preview = preview_effective_forwards(
            "sample",
            b"gatewayports yes\nlocalforward 3000 [::1]:80\ndynamicforward 1080\nremoteforward 9000 [::1]:90\n",
            &[],
            &[],
        )
        .unwrap();
        assert!(matches!(
            preview.candidates[0].forwarding,
            Some(ImportForwarding::Local { ref bind_address, .. }) if bind_address == "*"
        ));
        assert!(matches!(
            preview.candidates[1].forwarding,
            Some(ImportForwarding::Dynamic { ref bind_address, .. }) if bind_address == "*"
        ));
        assert!(matches!(
            preview.candidates[2].forwarding,
            Some(ImportForwarding::Remote { ref bind_address, .. }) if bind_address == "localhost"
        ));
    }

    #[test]
    fn relative_unix_socket_paths_are_unsupported() {
        let preview =
            preview_effective_forwards("sample", b"localforward 3000 ./service.sock\n", &[], &[])
                .unwrap();
        assert!(matches!(
            preview.candidates[0].classification,
            ImportClassification::Unsupported {
                reason: UnsupportedReason::UnixSocket
            }
        ));
    }
}
