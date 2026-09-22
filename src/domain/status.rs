use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AttemptId(Uuid);

impl AttemptId {
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

impl Default for AttemptId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeState {
    Stopped,
    Starting { attempt_id: AttemptId },
    Active { attempt_id: AttemptId },
    Reconnecting { failed_attempt_id: AttemptId },
    Stopping { attempt_id: Option<AttemptId> },
    Failed { attempt_id: AttemptId },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEvent {
    StartRequested {
        attempt_id: AttemptId,
    },
    ForwardingAccepted {
        attempt_id: AttemptId,
    },
    AttemptFailed {
        attempt_id: AttemptId,
        retry: bool,
    },
    ProcessExited {
        attempt_id: AttemptId,
        retry: bool,
    },
    RetryDue {
        failed_attempt_id: AttemptId,
        next_attempt_id: AttemptId,
    },
    StopRequested,
    CleanupComplete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transition {
    pub from: RuntimeState,
    pub to: RuntimeState,
    pub changed: bool,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TransitionError {
    #[error("event {event:?} is invalid while state is {state:?}")]
    Invalid {
        state: RuntimeState,
        event: RuntimeEvent,
    },
    #[error("event belongs to stale attempt {actual:?}; current attempt is {expected:?}")]
    StaleAttempt {
        expected: AttemptId,
        actual: AttemptId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeStateMachine {
    state: RuntimeState,
}

impl RuntimeStateMachine {
    pub const fn new() -> Self {
        Self {
            state: RuntimeState::Stopped,
        }
    }

    pub const fn state(&self) -> RuntimeState {
        self.state
    }

    pub fn apply(&mut self, event: RuntimeEvent) -> Result<Transition, TransitionError> {
        let from = self.state;
        let to = match (from, event) {
            (
                RuntimeState::Stopped | RuntimeState::Failed { .. },
                RuntimeEvent::StartRequested { attempt_id },
            ) => RuntimeState::Starting { attempt_id },
            (
                RuntimeState::Starting {
                    attempt_id: expected,
                },
                RuntimeEvent::ForwardingAccepted { attempt_id },
            ) => {
                ensure_attempt(expected, attempt_id)?;
                RuntimeState::Active { attempt_id }
            }
            (
                RuntimeState::Starting {
                    attempt_id: expected,
                },
                RuntimeEvent::AttemptFailed { attempt_id, retry },
            ) => {
                ensure_attempt(expected, attempt_id)?;
                failed_state(attempt_id, retry)
            }
            (
                RuntimeState::Active {
                    attempt_id: expected,
                },
                RuntimeEvent::ProcessExited { attempt_id, retry },
            ) => {
                ensure_attempt(expected, attempt_id)?;
                failed_state(attempt_id, retry)
            }
            (
                RuntimeState::Reconnecting {
                    failed_attempt_id: expected,
                },
                RuntimeEvent::RetryDue {
                    failed_attempt_id,
                    next_attempt_id,
                },
            ) => {
                ensure_attempt(expected, failed_attempt_id)?;
                RuntimeState::Starting {
                    attempt_id: next_attempt_id,
                }
            }
            (RuntimeState::Stopped, RuntimeEvent::StopRequested)
            | (RuntimeState::Stopping { .. }, RuntimeEvent::StopRequested) => from,
            (RuntimeState::Starting { attempt_id }, RuntimeEvent::StopRequested)
            | (RuntimeState::Active { attempt_id }, RuntimeEvent::StopRequested)
            | (RuntimeState::Failed { attempt_id }, RuntimeEvent::StopRequested) => {
                RuntimeState::Stopping {
                    attempt_id: Some(attempt_id),
                }
            }
            (RuntimeState::Reconnecting { failed_attempt_id }, RuntimeEvent::StopRequested) => {
                RuntimeState::Stopping {
                    attempt_id: Some(failed_attempt_id),
                }
            }
            (RuntimeState::Stopping { .. }, RuntimeEvent::CleanupComplete) => RuntimeState::Stopped,
            _ => return Err(TransitionError::Invalid { state: from, event }),
        };
        self.state = to;
        Ok(Transition {
            from,
            to,
            changed: from != to,
        })
    }
}

impl Default for RuntimeStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

fn ensure_attempt(expected: AttemptId, actual: AttemptId) -> Result<(), TransitionError> {
    if expected == actual {
        Ok(())
    } else {
        Err(TransitionError::StaleAttempt { expected, actual })
    }
}

const fn failed_state(attempt_id: AttemptId, retry: bool) -> RuntimeState {
    if retry {
        RuntimeState::Reconnecting {
            failed_attempt_id: attempt_id,
        }
    } else {
        RuntimeState::Failed { attempt_id }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{AttemptId, RuntimeEvent, RuntimeState, RuntimeStateMachine, TransitionError};

    fn attempt(number: u128) -> AttemptId {
        AttemptId::from_uuid(Uuid::from_u128(number))
    }

    #[test]
    fn successful_attempt_reaches_active_then_stops() {
        let mut machine = RuntimeStateMachine::new();
        machine
            .apply(RuntimeEvent::StartRequested {
                attempt_id: attempt(1),
            })
            .unwrap();
        machine
            .apply(RuntimeEvent::ForwardingAccepted {
                attempt_id: attempt(1),
            })
            .unwrap();
        assert_eq!(
            machine.state(),
            RuntimeState::Active {
                attempt_id: attempt(1)
            }
        );
        machine.apply(RuntimeEvent::StopRequested).unwrap();
        machine.apply(RuntimeEvent::CleanupComplete).unwrap();
        assert_eq!(machine.state(), RuntimeState::Stopped);
    }

    #[test]
    fn failed_attempt_retries_with_a_new_attempt() {
        let mut machine = RuntimeStateMachine::new();
        machine
            .apply(RuntimeEvent::StartRequested {
                attempt_id: attempt(1),
            })
            .unwrap();
        machine
            .apply(RuntimeEvent::AttemptFailed {
                attempt_id: attempt(1),
                retry: true,
            })
            .unwrap();
        machine
            .apply(RuntimeEvent::RetryDue {
                failed_attempt_id: attempt(1),
                next_attempt_id: attempt(2),
            })
            .unwrap();
        assert_eq!(
            machine.state(),
            RuntimeState::Starting {
                attempt_id: attempt(2)
            }
        );
    }

    #[test]
    fn process_exit_without_retry_enters_failed() {
        let mut machine = active_machine();
        machine
            .apply(RuntimeEvent::ProcessExited {
                attempt_id: attempt(1),
                retry: false,
            })
            .unwrap();
        assert_eq!(
            machine.state(),
            RuntimeState::Failed {
                attempt_id: attempt(1)
            }
        );
    }

    #[test]
    fn stale_attempt_event_is_rejected_without_mutation() {
        let mut machine = RuntimeStateMachine::new();
        machine
            .apply(RuntimeEvent::StartRequested {
                attempt_id: attempt(2),
            })
            .unwrap();
        let before = machine.state();
        let error = machine
            .apply(RuntimeEvent::ForwardingAccepted {
                attempt_id: attempt(1),
            })
            .unwrap_err();
        assert!(matches!(error, TransitionError::StaleAttempt { .. }));
        assert_eq!(machine.state(), before);
    }

    #[test]
    fn stop_is_idempotent_while_stopped_or_stopping() {
        let mut machine = RuntimeStateMachine::new();
        let transition = machine.apply(RuntimeEvent::StopRequested).unwrap();
        assert!(!transition.changed);

        machine
            .apply(RuntimeEvent::StartRequested {
                attempt_id: attempt(1),
            })
            .unwrap();
        machine.apply(RuntimeEvent::StopRequested).unwrap();
        let transition = machine.apply(RuntimeEvent::StopRequested).unwrap();
        assert!(!transition.changed);
    }

    #[test]
    fn invalid_event_is_rejected_without_mutation() {
        let mut machine = RuntimeStateMachine::new();
        let error = machine.apply(RuntimeEvent::CleanupComplete).unwrap_err();
        assert!(matches!(error, TransitionError::Invalid { .. }));
        assert_eq!(machine.state(), RuntimeState::Stopped);
    }

    #[test]
    fn stop_is_available_while_reconnecting_or_failed() {
        for retry in [true, false] {
            let mut machine = RuntimeStateMachine::new();
            machine
                .apply(RuntimeEvent::StartRequested {
                    attempt_id: attempt(1),
                })
                .unwrap();
            machine
                .apply(RuntimeEvent::AttemptFailed {
                    attempt_id: attempt(1),
                    retry,
                })
                .unwrap();
            machine.apply(RuntimeEvent::StopRequested).unwrap();
            assert_eq!(
                machine.state(),
                RuntimeState::Stopping {
                    attempt_id: Some(attempt(1))
                }
            );
        }
    }

    fn active_machine() -> RuntimeStateMachine {
        let mut machine = RuntimeStateMachine::new();
        machine
            .apply(RuntimeEvent::StartRequested {
                attempt_id: attempt(1),
            })
            .unwrap();
        machine
            .apply(RuntimeEvent::ForwardingAccepted {
                attempt_id: attempt(1),
            })
            .unwrap();
        machine
    }
}
