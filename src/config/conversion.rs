use super::{ConfigV1, ConfigV2, Rule, SCHEMA_VERSION, Settings};
use crate::domain::{
    rule::{Forwarding, Rule as DomainRule, RuleId},
    validation::{ValidationError, validate_rule_set},
};

impl TryFrom<Rule> for DomainRule {
    type Error = ValidationError;
    fn try_from(value: Rule) -> Result<Self, Self::Error> {
        let (rule, bind, auto, reconnect) = match value {
            Rule::Local {
                id,
                name,
                ssh_host_alias,
                bind_address,
                bind_port,
                destination_host,
                destination_port,
                auto_start,
                reconnect,
            } => (
                Self::local(
                    RuleId::from_uuid(id),
                    name,
                    ssh_host_alias,
                    bind_port,
                    destination_host,
                    destination_port,
                )?,
                bind_address,
                auto_start,
                reconnect,
            ),
            Rule::Remote {
                id,
                name,
                ssh_host_alias,
                bind_address,
                bind_port,
                destination_host,
                destination_port,
                auto_start,
                reconnect,
            } => (
                Self::remote(
                    RuleId::from_uuid(id),
                    name,
                    ssh_host_alias,
                    bind_port,
                    destination_host,
                    destination_port,
                )?,
                bind_address,
                auto_start,
                reconnect,
            ),
            Rule::Dynamic {
                id,
                name,
                ssh_host_alias,
                bind_address,
                bind_port,
                auto_start,
                reconnect,
            } => (
                Self::dynamic(RuleId::from_uuid(id), name, ssh_host_alias, bind_port)?,
                bind_address,
                auto_start,
                reconnect,
            ),
        };
        Ok(rule.with_bind_address(bind)?.with_policy(auto, reconnect))
    }
}

impl From<&DomainRule> for Rule {
    fn from(rule: &DomainRule) -> Self {
        let id = rule.id().as_uuid();
        let name = rule.name().as_str().to_owned();
        let ssh_host_alias = rule.ssh_host_alias().as_str().to_owned();
        let auto_start = rule.auto_start();
        let reconnect = rule.reconnect();
        match rule.forwarding() {
            Forwarding::Local(v) => Self::Local {
                id,
                name,
                ssh_host_alias,
                auto_start,
                reconnect,
                bind_address: v.bind_address().to_string(),
                bind_port: v.bind_port().get(),
                destination_host: v.destination_host().to_string(),
                destination_port: v.destination_port().get(),
            },
            Forwarding::Remote(v) => Self::Remote {
                id,
                name,
                ssh_host_alias,
                auto_start,
                reconnect,
                bind_address: v.bind_address().to_string(),
                bind_port: v.bind_port().get(),
                destination_host: v.destination_host().to_string(),
                destination_port: v.destination_port().get(),
            },
            Forwarding::Dynamic(v) => Self::Dynamic {
                id,
                name,
                ssh_host_alias,
                auto_start,
                reconnect,
                bind_address: v.bind_address().to_string(),
                bind_port: v.bind_port().get(),
            },
        }
    }
}

impl ConfigV1 {
    pub fn into_domain(self) -> Result<Vec<DomainRule>, super::StoreError> {
        if self.schema_version != 1 {
            return Err(super::StoreError::UnsupportedSchema(self.schema_version));
        }
        let rules = self
            .rules
            .into_iter()
            .map(DomainRule::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        validate_rule_set(&rules).map_err(super::StoreError::InvalidRules)?;
        Ok(rules)
    }
}

impl ConfigV2 {
    pub fn into_domain(self) -> Result<(Vec<DomainRule>, Settings), super::StoreError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(super::StoreError::UnsupportedSchema(self.schema_version));
        }
        let rules = self
            .rules
            .into_iter()
            .map(DomainRule::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        validate_rule_set(&rules).map_err(super::StoreError::InvalidRules)?;
        Ok((rules, self.settings))
    }

    pub fn from_domain(
        rules: &[DomainRule],
        settings: Settings,
    ) -> Result<Self, super::StoreError> {
        validate_rule_set(rules).map_err(super::StoreError::InvalidRules)?;
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            rules: rules.iter().map(Rule::from).collect(),
            settings,
        })
    }
}
