use std::fmt;

use super::validation::{Field, ValidationError};

/// An exact OpenSSH `Host` alias passed to `ssh` as one argument.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct HostAlias(String);

impl HostAlias {
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ValidationError::Empty {
                field: Field::SshHostAlias,
            });
        }
        if value.trim() != value {
            return Err(ValidationError::SurroundingWhitespace {
                field: Field::SshHostAlias,
            });
        }
        if value.chars().count() > 255 {
            return Err(ValidationError::TooLong {
                field: Field::SshHostAlias,
                max_characters: 255,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(ValidationError::ControlCharacter {
                field: Field::SshHostAlias,
            });
        }
        if value.chars().any(char::is_whitespace) {
            return Err(ValidationError::Whitespace {
                field: Field::SshHostAlias,
            });
        }
        if value.starts_with('-') {
            return Err(ValidationError::LooksLikeOption {
                field: Field::SshHostAlias,
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostAlias {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::HostAlias;
    use crate::domain::validation::{Field, ValidationError};

    #[test]
    fn accepts_an_exact_openssh_alias() {
        let alias = HostAlias::new("production-web_01").unwrap();
        assert_eq!(alias.as_str(), "production-web_01");
        assert_eq!(alias.to_string(), "production-web_01");
    }

    #[test]
    fn rejects_empty_surrounding_whitespace_control_whitespace_and_options() {
        let cases = [
            (
                "",
                ValidationError::Empty {
                    field: Field::SshHostAlias,
                },
            ),
            (
                " server",
                ValidationError::SurroundingWhitespace {
                    field: Field::SshHostAlias,
                },
            ),
            (
                "serv\0er",
                ValidationError::ControlCharacter {
                    field: Field::SshHostAlias,
                },
            ),
            (
                "web server",
                ValidationError::Whitespace {
                    field: Field::SshHostAlias,
                },
            ),
            (
                "-oProxyCommand=bad",
                ValidationError::LooksLikeOption {
                    field: Field::SshHostAlias,
                },
            ),
        ];

        for (value, expected) in cases {
            assert_eq!(HostAlias::new(value).unwrap_err(), expected);
        }
    }

    #[test]
    fn rejects_aliases_over_255_characters() {
        assert_eq!(
            HostAlias::new("a".repeat(256)).unwrap_err(),
            ValidationError::TooLong {
                field: Field::SshHostAlias,
                max_characters: 255,
            }
        );
    }
}
