//! Daemon-owned desired configuration and idempotent runtime intent.

use crate::{
    config::{ConfigStore, ConfigV1, Rule as WireRule},
    daemon::lifecycle::RequestHandler,
    domain::{rule::Rule, validation::validate_rule_set},
    ipc::{self, ErrorCode, Event, EventHub, Operation, Request, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::Path,
    sync::{Mutex, mpsc},
};
use uuid::Uuid;

struct State {
    store: ConfigStore,
    rules: Vec<Rule>,
    running: HashSet<Uuid>,
}
pub struct DaemonManager {
    state: Mutex<State>,
    events: EventHub,
}

impl DaemonManager {
    pub fn open(config_directory: &Path) -> Result<Self, crate::config::StoreError> {
        let store = ConfigStore::open(config_directory)?;
        let rules = store.load()?.unwrap_or_default();
        Ok(Self {
            state: Mutex::new(State {
                store,
                rules,
                running: HashSet::new(),
            }),
            events: EventHub::default(),
        })
    }

    fn dispatch(&self, request: &Request) -> Result<Value, (ErrorCode, String)> {
        let mut state = self.state.lock().map_err(|_| {
            (
                ErrorCode::Internal,
                "daemon state is unavailable".to_owned(),
            )
        })?;
        match request.operation {
            Operation::ForwardList => Ok(serde_json::to_value(
                ConfigV1::from_domain(&state.rules).map_err(internal)?.rules,
            )
            .map_err(internal)?),
            Operation::ForwardAdd => {
                let wire: WireRule =
                    serde_json::from_value(request.payload.clone()).map_err(invalid)?;
                let rule = Rule::try_from(wire)
                    .map_err(|error| (ErrorCode::InvalidRequest, error.to_string()))?;
                let id = rule.id().as_uuid();
                let mut next = state.rules.clone();
                next.push(rule);
                validate_rule_set(&next).map_err(|errors| {
                    let error = crate::config::StoreError::InvalidRules(errors);
                    (ErrorCode::Conflict, error.to_string())
                })?;
                state.store.save(&next).map_err(internal)?;
                state.rules = next;
                self.events
                    .publish("configuration_changed", json!({"rule_id": id}));
                Ok(json!({"rule_id": id}))
            }
            Operation::ForwardRemove => {
                let id = rule_id(&request.payload)?;
                if state.running.contains(&id) {
                    return Err((
                        ErrorCode::Conflict,
                        "an active rule cannot be removed".to_owned(),
                    ));
                }
                let before = state.rules.len();
                let next: Vec<_> = state
                    .rules
                    .iter()
                    .filter(|rule| rule.id().as_uuid() != id)
                    .cloned()
                    .collect();
                if next.len() == before {
                    return Err((ErrorCode::NotFound, "rule was not found".to_owned()));
                }
                state.store.save(&next).map_err(internal)?;
                state.rules = next;
                self.events
                    .publish("configuration_changed", json!({"rule_id": id}));
                Ok(json!({"removed": true}))
            }
            Operation::ForwardStart => {
                let id = rule_id(&request.payload)?;
                if !state.rules.iter().any(|rule| rule.id().as_uuid() == id) {
                    return Err((ErrorCode::NotFound, "rule was not found".to_owned()));
                }
                let changed = state.running.insert(id);
                if changed {
                    self.events.publish("rule_started", json!({"rule_id": id}));
                }
                Ok(json!({"start_requested": true, "changed": changed}))
            }
            Operation::ForwardStop => {
                let id = rule_id(&request.payload)?;
                if !state.rules.iter().any(|rule| rule.id().as_uuid() == id) {
                    return Err((ErrorCode::NotFound, "rule was not found".to_owned()));
                }
                let changed = state.running.remove(&id);
                if changed {
                    self.events.publish("rule_stopped", json!({"rule_id": id}));
                }
                Ok(json!({"start_requested": false, "changed": changed}))
            }
            Operation::Status => {
                Ok(json!({"rules": state.rules.len(), "start_requested": state.running.len()}))
            }
            Operation::Subscribe => Ok(json!({"subscribed": true})),
            _ => Err((
                ErrorCode::Unavailable,
                "operation is not handled by the daemon yet".to_owned(),
            )),
        }
    }
}

impl RequestHandler for DaemonManager {
    fn handle(&self, request: &Request) -> Response {
        match self.dispatch(request) {
            Ok(value) => ipc::success(request.request_id, value),
            Err((code, message)) => ipc::failure(request.request_id, code, message),
        }
    }
    fn subscribe(&self) -> Option<mpsc::Receiver<Event>> {
        Some(self.events.subscribe())
    }
}

fn internal(error: impl std::fmt::Display) -> (ErrorCode, String) {
    (ErrorCode::Internal, error.to_string())
}
fn invalid(error: impl std::fmt::Display) -> (ErrorCode, String) {
    (ErrorCode::InvalidRequest, error.to_string())
}
fn rule_id(value: &Value) -> Result<Uuid, (ErrorCode, String)> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Id {
        rule_id: Uuid,
    }
    serde_json::from_value::<Id>(value.clone())
        .map(|value| value.rule_id)
        .map_err(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{PROTOCOL_VERSION, Response};
    fn private_tempdir() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
    fn request(operation: Operation, payload: Value) -> Request {
        Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            operation,
            payload,
        }
    }
    fn result(response: Response) -> Value {
        match response {
            Response::Success(value) => value.result,
            Response::Failure(value) => panic!("unexpected error: {:?}", value.error),
        }
    }

    fn failure(response: Response) -> crate::ipc::ProtocolError {
        match response {
            Response::Failure(value) => value.error,
            Response::Success(value) => panic!("unexpected success: {:?}", value.result),
        }
    }
    #[test]
    fn mutations_are_persisted_and_start_stop_are_idempotent() {
        let root = private_tempdir();
        let manager = DaemonManager::open(root.path()).unwrap();
        let events = manager.subscribe().unwrap();
        let id = Uuid::new_v4();
        let rule = json!({"kind":"dynamic","id":id,"name":"proxy","ssh_host_alias":"host","bind_address":"127.0.0.1","bind_port":1080,"auto_start":false,"reconnect":false});
        result(manager.handle(&request(Operation::ForwardAdd, rule)));
        assert_eq!(
            result(manager.handle(&request(Operation::ForwardStart, json!({"rule_id":id}))))["changed"],
            true
        );
        assert_eq!(
            result(manager.handle(&request(Operation::ForwardStart, json!({"rule_id":id}))))["changed"],
            false
        );
        assert_eq!(
            result(manager.handle(&request(Operation::ForwardStop, json!({"rule_id":id}))))["changed"],
            true
        );
        assert_eq!(
            result(manager.handle(&request(Operation::ForwardStop, json!({"rule_id":id}))))["changed"],
            false
        );
        let emitted: Vec<_> = events.try_iter().collect();
        assert_eq!(
            emitted
                .iter()
                .map(|event| event.event.as_str())
                .collect::<Vec<_>>(),
            ["configuration_changed", "rule_started", "rule_stopped"]
        );
        assert_eq!(
            emitted
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        drop(manager);
        assert_eq!(
            DaemonManager::open(root.path())
                .unwrap()
                .state
                .lock()
                .unwrap()
                .rules
                .len(),
            1
        );
    }

    #[test]
    fn conflicting_rule_set_is_reported_without_persistence() {
        let root = private_tempdir();
        let manager = DaemonManager::open(root.path()).unwrap();
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let rule = |id| json!({"kind":"dynamic","id":id,"name":"proxy","ssh_host_alias":"host","bind_address":"127.0.0.1","bind_port":1080,"auto_start":false,"reconnect":false});
        result(manager.handle(&request(Operation::ForwardAdd, rule(first_id))));

        let error = failure(manager.handle(&request(Operation::ForwardAdd, rule(second_id))));
        assert_eq!(error.code, ErrorCode::Conflict);
        assert!(error.message.contains("DuplicateName"));

        drop(manager);
        let reopened = DaemonManager::open(root.path()).unwrap();
        assert_eq!(reopened.state.lock().unwrap().rules.len(), 1);
    }
}
