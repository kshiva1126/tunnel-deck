//! Daemon-owned desired configuration and idempotent runtime intent.

use crate::{
    config::{ConfigStore, ConfigV1, Rule as WireRule},
    daemon::{
        lifecycle::RequestHandler,
        process::{self, ManagedAttempt},
    },
    domain::{rule::Rule, validation::validate_rule_set},
    ipc::{self, ErrorCode, Event, EventHub, Operation, Request, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::File,
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
};
use uuid::Uuid;

struct State {
    store: ConfigStore,
    rules: Vec<Rule>,
    running: HashMap<Uuid, Attempt>,
}
enum Attempt {
    Starting(Uuid),
    Active(ManagedAttempt),
}
pub struct DaemonManager {
    state: Mutex<State>,
    events: EventHub,
    process: Option<ProcessSettings>,
}

struct ProcessSettings {
    executable: PathBuf,
    ssh: PathBuf,
    runtime: PathBuf,
    lock: File,
}

impl DaemonManager {
    pub fn open(config_directory: &Path) -> Result<Self, crate::config::StoreError> {
        let store = ConfigStore::open(config_directory)?;
        let rules = store.load()?.unwrap_or_default();
        Ok(Self {
            state: Mutex::new(State {
                store,
                rules,
                running: HashMap::new(),
            }),
            events: EventHub::default(),
            process: None,
        })
    }

    pub fn open_managed(
        config_directory: &Path,
        runtime: PathBuf,
        executable: PathBuf,
        ssh: PathBuf,
        lock: File,
    ) -> Result<Self, crate::config::StoreError> {
        let mut manager = Self::open(config_directory)?;
        manager.process = Some(ProcessSettings {
            executable,
            ssh,
            runtime,
            lock,
        });
        Ok(manager)
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
                if let Some(existing) = next
                    .iter_mut()
                    .find(|existing| existing.id().as_uuid() == id)
                {
                    if state.running.contains_key(&id) {
                        return Err((
                            ErrorCode::Conflict,
                            "an active rule cannot be edited".to_owned(),
                        ));
                    }
                    *existing = rule;
                } else {
                    next.push(rule);
                }
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
                let id = rule_id(&request.payload, &state)?;
                if state.running.contains_key(&id) {
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
                Ok(json!({"rule_id": id, "removed": true}))
            }
            Operation::ForwardStart => {
                unreachable!("start is dispatched without holding the state lock")
            }
            Operation::ForwardStop => {
                unreachable!("stop is dispatched without holding the state lock")
            }
            Operation::Status => {
                let mut exited = Vec::new();
                for (id, attempt) in &mut state.running {
                    if let Attempt::Active(attempt) = attempt {
                        if attempt.has_exited().map_err(internal)? {
                            exited.push(*id);
                        }
                    }
                }
                for id in exited {
                    state.running.remove(&id);
                    self.events.publish("rule_failed", json!({"rule_id": id}));
                }
                let active = state
                    .running
                    .values()
                    .filter(|value| matches!(value, Attempt::Active(_)))
                    .count();
                let starting = state.running.len() - active;
                let states: Vec<_> = state
                    .rules
                    .iter()
                    .map(|rule| {
                        let id = rule.id().as_uuid();
                        let status = match state.running.get(&id) {
                            Some(Attempt::Starting(_)) => "starting",
                            Some(Attempt::Active(_)) => "active",
                            None => "stopped",
                        };
                        json!({"rule_id": id, "name": rule.name().as_str(), "state": status})
                    })
                    .collect();
                Ok(
                    json!({"rules": state.rules.len(), "active": active, "starting": starting, "forwards": states}),
                )
            }
            Operation::Subscribe => Ok(json!({"subscribed": true})),
            _ => Err((
                ErrorCode::Unavailable,
                "operation is not handled by the daemon yet".to_owned(),
            )),
        }
    }

    fn start(&self, request: &Request) -> Result<Value, (ErrorCode, String)> {
        let attempt_id = Uuid::new_v4();
        let rule = {
            let mut state = self.state.lock().map_err(|_| {
                (
                    ErrorCode::Internal,
                    "daemon state is unavailable".to_owned(),
                )
            })?;
            let id = rule_id(&request.payload, &state)?;
            let rule = state
                .rules
                .iter()
                .find(|rule| rule.id().as_uuid() == id)
                .cloned()
                .ok_or((ErrorCode::NotFound, "rule was not found".to_owned()))?;
            if state.running.contains_key(&id) {
                return Ok(json!({"rule_id": id, "start_requested": true, "changed": false}));
            }
            check_local_port(&rule)?;
            // The attempt ID is a cancellable Starting marker. No process call is made
            // while the daemon state lock is held, so stop can remove it.
            state.running.insert(id, Attempt::Starting(attempt_id));
            (id, rule)
        };
        let (id, rule) = rule;
        let attempt = if let Some(settings) = &self.process {
            match process::start_attempt(
                &settings.executable,
                &settings.ssh,
                &settings.runtime,
                &settings.lock,
                &rule,
            ) {
                Ok(attempt) => Some(attempt),
                Err(error) => {
                    let mut state = self.state.lock().map_err(|_| {
                        (
                            ErrorCode::Internal,
                            "daemon state is unavailable".to_owned(),
                        )
                    })?;
                    if matches!(state.running.get(&id), Some(Attempt::Starting(current)) if *current == attempt_id)
                    {
                        state.running.remove(&id);
                    }
                    return Err((ErrorCode::Unavailable, error.to_string()));
                }
            }
        } else {
            None
        };
        let mut state = self.state.lock().map_err(|_| {
            (
                ErrorCode::Internal,
                "daemon state is unavailable".to_owned(),
            )
        })?;
        if !matches!(state.running.get(&id), Some(Attempt::Starting(current)) if *current == attempt_id)
        {
            drop(state);
            if let Some(attempt) = attempt {
                attempt.stop().map_err(internal)?;
            }
            return Ok(json!({"rule_id": id, "start_requested": false, "changed": true}));
        }
        state.running.insert(
            id,
            match attempt {
                Some(attempt) => Attempt::Active(attempt),
                None => Attempt::Starting(attempt_id),
            },
        );
        drop(state);
        self.events.publish("rule_started", json!({"rule_id": id}));
        Ok(json!({"rule_id": id, "start_requested": true, "changed": true}))
    }

    fn stop(&self, request: &Request) -> Result<Value, (ErrorCode, String)> {
        let attempt = {
            let mut state = self.state.lock().map_err(|_| {
                (
                    ErrorCode::Internal,
                    "daemon state is unavailable".to_owned(),
                )
            })?;
            let id = rule_id(&request.payload, &state)?;
            if !state.rules.iter().any(|rule| rule.id().as_uuid() == id) {
                return Err((ErrorCode::NotFound, "rule was not found".to_owned()));
            }
            (id, state.running.remove(&id))
        };
        let (id, attempt) = attempt;
        let changed = attempt.is_some();
        if let Some(Attempt::Active(attempt)) = attempt {
            attempt
                .stop()
                .map_err(|error| (ErrorCode::Internal, error.to_string()))?;
        }
        if changed {
            self.events.publish("rule_stopped", json!({"rule_id": id}));
        }
        Ok(json!({"rule_id": id, "start_requested": false, "changed": changed}))
    }
}

impl RequestHandler for DaemonManager {
    fn handle(&self, request: &Request) -> Response {
        let result = match request.operation {
            Operation::ForwardStart => self.start(request),
            Operation::ForwardStop => self.stop(request),
            _ => self.dispatch(request),
        };
        match result {
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

fn check_local_port(rule: &Rule) -> Result<(), (ErrorCode, String)> {
    let Some((address, port)) = rule.local_listener() else {
        return Ok(());
    };
    let address = if address.as_str() == "*" {
        "0.0.0.0"
    } else {
        address.as_str()
    };
    match std::net::TcpListener::bind((address, port.get())) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => Err((
            ErrorCode::Conflict,
            format!("local listener {address}:{} is already in use", port.get()),
        )),
        Err(error) => Err((
            ErrorCode::Unavailable,
            format!("local listener cannot be checked: {error}"),
        )),
    }
}
fn rule_id(value: &Value, state: &State) -> Result<Uuid, (ErrorCode, String)> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Selector {
        Id { rule_id: Uuid },
        Name { rule_name: String },
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Id {
        rule_id: Uuid,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Name {
        rule_name: String,
    }
    let selector = serde_json::from_value::<Id>(value.clone())
        .map(|v| Selector::Id { rule_id: v.rule_id })
        .or_else(|_| {
            serde_json::from_value::<Name>(value.clone()).map(|v| Selector::Name {
                rule_name: v.rule_name,
            })
        })
        .map_err(invalid)?;
    match selector {
        Selector::Id { rule_id } => Ok(rule_id),
        Selector::Name { rule_name } => state
            .rules
            .iter()
            .find(|rule| rule.name().as_str() == rule_name)
            .map(|rule| rule.id().as_uuid())
            .ok_or((ErrorCode::NotFound, "rule was not found".to_owned())),
    }
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

    #[test]
    fn stopped_rule_can_be_edited_by_stable_id_but_active_rule_cannot() {
        let root = private_tempdir();
        let manager = DaemonManager::open(root.path()).unwrap();
        let id = Uuid::new_v4();
        let rule = |name: &str| json!({"kind":"dynamic","id":id,"name":name,"ssh_host_alias":"host","bind_address":"127.0.0.1","bind_port":1080,"auto_start":false,"reconnect":false});
        result(manager.handle(&request(Operation::ForwardAdd, rule("proxy"))));
        result(manager.handle(&request(Operation::ForwardAdd, rule("renamed"))));
        let listed = result(manager.handle(&request(Operation::ForwardList, json!({}))));
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["name"], "renamed");

        result(manager.handle(&request(Operation::ForwardStart, json!({"rule_id":id}))));
        let error = failure(manager.handle(&request(Operation::ForwardAdd, rule("not-saved"))));
        assert_eq!(error.code, ErrorCode::Conflict);
        assert!(error.message.contains("cannot be edited"));
    }

    #[test]
    fn exact_name_controls_a_rule_and_every_result_contains_its_uuid() {
        let root = private_tempdir();
        let manager = DaemonManager::open(root.path()).unwrap();
        let id = Uuid::new_v4();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let rule = json!({"kind":"local","id":id,"name":"web app","ssh_host_alias":"host","bind_address":"127.0.0.1","bind_port":port,"destination_host":"127.0.0.1","destination_port":3000,"auto_start":false,"reconnect":false});
        assert_eq!(
            result(manager.handle(&request(Operation::ForwardAdd, rule)))["rule_id"],
            id.to_string()
        );
        let selector = json!({"rule_name":"web app"});
        let started = result(manager.handle(&request(Operation::ForwardStart, selector.clone())));
        assert_eq!(started["rule_id"], id.to_string());
        assert_eq!(started["changed"], true);
        let repeated = result(manager.handle(&request(Operation::ForwardStart, selector.clone())));
        assert_eq!(repeated["rule_id"], id.to_string());
        assert_eq!(repeated["changed"], false);

        let removal = failure(manager.handle(&request(Operation::ForwardRemove, selector.clone())));
        assert_eq!(removal.code, ErrorCode::Conflict);
        let stopped = result(manager.handle(&request(Operation::ForwardStop, selector.clone())));
        assert_eq!(stopped["rule_id"], id.to_string());
        assert_eq!(stopped["changed"], true);
        let repeated = result(manager.handle(&request(Operation::ForwardStop, selector.clone())));
        assert_eq!(repeated["changed"], false);
        let removed = result(manager.handle(&request(Operation::ForwardRemove, selector)));
        assert_eq!(removed["rule_id"], id.to_string());
        assert_eq!(removed["removed"], true);
    }

    #[test]
    fn occupied_explicit_local_port_is_a_structured_conflict() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let root = private_tempdir();
        let manager = DaemonManager::open(root.path()).unwrap();
        let id = Uuid::new_v4();
        let rule = json!({"kind":"local","id":id,"name":"web","ssh_host_alias":"host","bind_address":"127.0.0.1","bind_port":port,"destination_host":"127.0.0.1","destination_port":3000,"auto_start":false,"reconnect":false});
        result(manager.handle(&request(Operation::ForwardAdd, rule)));

        let error =
            failure(manager.handle(&request(Operation::ForwardStart, json!({"rule_id":id}))));
        assert_eq!(error.code, ErrorCode::Conflict);
        assert!(error.message.contains("already in use"));
        assert_eq!(
            result(manager.handle(&request(Operation::Status, json!({}))))["active"],
            0
        );
    }
}
