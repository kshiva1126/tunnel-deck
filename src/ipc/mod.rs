//! Version 1 newline-delimited JSON IPC contract and bounded Unix transport.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::UnixStream,
    },
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use thiserror::Error;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
pub const EVENT_BUFFER_CAPACITY: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u32,
    pub request_id: Uuid,
    pub operation: Operation,
    pub payload: Value,
}
impl Request {
    pub fn new(operation: Operation, payload: Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            operation,
            payload,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    HostList,
    HostShow,
    HostTest,
    ForwardList,
    ForwardAdd,
    ForwardRemove,
    ForwardStart,
    ForwardStop,
    Status,
    Subscribe,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Success(SuccessResponse),
    Failure(ErrorResponse),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuccessResponse {
    pub protocol_version: u32,
    pub request_id: Uuid,
    pub result: Value,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    pub protocol_version: u32,
    pub request_id: Uuid,
    pub error: ProtocolError,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    MessageTooLarge,
    UnsupportedVersion,
    NotFound,
    Conflict,
    Unavailable,
    Internal,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub protocol_version: u32,
    pub sequence: u64,
    pub event: String,
    pub payload: Value,
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("IPC I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("IPC message exceeds 1 MiB or is not newline terminated")]
    MessageTooLarge,
    #[error("IPC peer disconnected")]
    Disconnected,
    #[error("invalid IPC message: {0}")]
    InvalidMessage(#[from] serde_json::Error),
    #[error("IPC response request ID did not match")]
    RequestIdMismatch,
    #[error("IPC protocol version mismatch (expected 1, received {0})")]
    VersionMismatch(u32),
    #[error("IPC subscription acknowledgement was invalid")]
    InvalidSubscriptionAcknowledgement,
    #[error("IPC subscription was rejected: {0:?}")]
    SubscriptionRejected(ErrorCode),
}

pub fn read_frame<T: serde::de::DeserializeOwned>(
    reader: &mut impl BufRead,
) -> Result<T, TransportError> {
    let mut bytes = Vec::new();
    let read = reader
        .take((MAX_MESSAGE_BYTES + 2) as u64)
        .read_until(b'\n', &mut bytes)?;
    if read == 0 {
        return Err(TransportError::Disconnected);
    }
    if bytes.last() != Some(&b'\n') || bytes.len() - 1 > MAX_MESSAGE_BYTES {
        return Err(TransportError::MessageTooLarge);
    }
    bytes.pop();
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<(), TransportError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(TransportError::MessageTooLarge);
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

pub struct Client {
    reader: BufReader<UnixStream>,
    timeout: Duration,
}
impl Client {
    pub fn connect(path: &std::path::Path, timeout: Duration) -> Result<Self, TransportError> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != crate::platform::private_fs::current_uid()
            || metadata.permissions().mode() & 0o7777 != 0o600
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe IPC socket ownership, type, or permissions",
            )
            .into());
        }
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(Self {
            reader: BufReader::new(stream),
            timeout,
        })
    }
    pub fn call(&mut self, request: &Request) -> Result<Response, TransportError> {
        self.reader.get_mut().set_read_timeout(Some(self.timeout))?;
        write_frame(self.reader.get_mut(), request)?;
        let response: Response = read_frame(&mut self.reader)?;
        let (version, id) = match &response {
            Response::Success(v) => (v.protocol_version, v.request_id),
            Response::Failure(v) => (v.protocol_version, v.request_id),
        };
        if version != PROTOCOL_VERSION {
            return Err(TransportError::VersionMismatch(version));
        }
        if id != request.request_id {
            return Err(TransportError::RequestIdMismatch);
        }
        Ok(response)
    }

    pub fn subscribe(mut self) -> Result<Subscription, TransportError> {
        let request = Request::new(Operation::Subscribe, Value::Object(Default::default()));
        match self.call(&request)? {
            Response::Success(value)
                if value.result.get("subscribed").and_then(Value::as_bool) == Some(true) =>
            {
                Ok(Subscription {
                    reader: self.reader,
                })
            }
            Response::Success(_) => Err(TransportError::InvalidSubscriptionAcknowledgement),
            Response::Failure(value) => Err(TransportError::SubscriptionRejected(value.error.code)),
        }
    }
}

pub struct Subscription {
    reader: BufReader<UnixStream>,
}
impl Subscription {
    pub fn read_event(&mut self) -> Result<Event, TransportError> {
        let event: Event = read_frame(&mut self.reader)?;
        if event.protocol_version != PROTOCOL_VERSION {
            return Err(TransportError::VersionMismatch(event.protocol_version));
        }
        Ok(event)
    }
}

#[derive(Clone, Default)]
pub struct EventHub {
    inner: Arc<Mutex<EventHubState>>,
}
#[derive(Default)]
struct EventHubState {
    sequence: u64,
    subscribers: Vec<mpsc::SyncSender<Event>>,
}
impl EventHub {
    pub fn subscribe(&self) -> mpsc::Receiver<Event> {
        let (sender, receiver) = mpsc::sync_channel(EVENT_BUFFER_CAPACITY);
        self.inner
            .lock()
            .expect("event hub poisoned")
            .subscribers
            .push(sender);
        receiver
    }
    pub fn publish(&self, event: impl Into<String>, payload: Value) -> Event {
        let mut state = self.inner.lock().expect("event hub poisoned");
        state.sequence = state
            .sequence
            .checked_add(1)
            .expect("event sequence exhausted");
        let message = Event {
            protocol_version: PROTOCOL_VERSION,
            sequence: state.sequence,
            event: event.into(),
            payload,
        };
        state
            .subscribers
            .retain(|sender| sender.try_send(message.clone()).is_ok());
        message
    }
}
pub fn success(request_id: Uuid, result: Value) -> Response {
    Response::Success(SuccessResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        result,
    })
}
pub fn failure(request_id: Uuid, code: ErrorCode, message: impl Into<String>) -> Response {
    Response::Failure(ErrorResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        error: ProtocolError {
            code,
            message: message.into(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Serialize, de::DeserializeOwned};
    fn assert_json_fixture<T>(fixture: &str)
    where
        T: DeserializeOwned + Serialize + PartialEq + std::fmt::Debug,
    {
        let frame = fixture.strip_suffix('\n').unwrap_or(fixture);
        assert!(!frame.contains('\n'));
        let value: T = serde_json::from_str(frame).unwrap();
        let encoded = serde_json::to_string(&value).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&encoded).unwrap(),
            serde_json::from_str::<Value>(frame).unwrap()
        );
        assert_eq!(serde_json::from_str::<T>(&encoded).unwrap(), value);
    }
    #[test]
    fn version_one_fixtures_round_trip() {
        assert_json_fixture::<Request>(include_str!("../../tests/fixtures/ipc_request_v1.json"));
        assert_json_fixture::<Response>(include_str!("../../tests/fixtures/ipc_response_v1.json"));
        assert_json_fixture::<Response>(include_str!("../../tests/fixtures/ipc_error_v1.json"));
        assert_json_fixture::<Event>(include_str!("../../tests/fixtures/ipc_event_v1.json"));
    }
    #[test]
    fn fixtures_declare_current_protocol() {
        let request: Request =
            serde_json::from_str(include_str!("../../tests/fixtures/ipc_request_v1.json")).unwrap();
        assert_eq!(request.protocol_version, PROTOCOL_VERSION);
    }
    #[test]
    fn unknown_request_fields_are_rejected() {
        assert!(serde_json::from_str::<Request>(r#"{"protocol_version":1,"request_id":"24699e94-5c3f-4d69-a400-d3f08102787c","operation":"status","payload":{},"extra":true}"#).is_err());
    }
    #[test]
    fn framing_rejects_malformed_truncated_and_oversized_messages() {
        assert!(matches!(
            read_frame::<Request>(&mut BufReader::new(&b"{}\n"[..])),
            Err(TransportError::InvalidMessage(_))
        ));
        assert!(matches!(
            read_frame::<Value>(&mut BufReader::new(&b"{}"[..])),
            Err(TransportError::MessageTooLarge)
        ));
        let oversized = vec![b'x'; MAX_MESSAGE_BYTES + 2];
        assert!(matches!(
            read_frame::<Value>(&mut BufReader::new(&oversized[..])),
            Err(TransportError::MessageTooLarge)
        ));
    }
    #[test]
    fn event_sequences_increase_and_lagging_subscribers_are_dropped() {
        let hub = EventHub::default();
        let receiver = hub.subscribe();
        for n in 1..=(EVENT_BUFFER_CAPACITY + 2) {
            assert_eq!(hub.publish("changed", Value::Null).sequence, n as u64);
        }
        assert_eq!(receiver.iter().count(), EVENT_BUFFER_CAPACITY);
    }

    #[test]
    fn client_times_out_when_server_does_not_respond() {
        use std::{
            fs,
            os::unix::{fs::PermissionsExt, net::UnixListener},
            thread,
        };
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.path().join("socket");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let task = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(100));
        });
        let mut client = Client::connect(&socket, Duration::from_millis(20)).unwrap();
        assert!(
            matches!(client.call(&Request::new(Operation::Status, Value::Null)), Err(TransportError::Io(error)) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut))
        );
        task.join().unwrap();
    }

    #[test]
    fn subscription_preserves_buffered_events_after_acknowledgement() {
        use std::{os::unix::net::UnixStream, thread};

        let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
        let task = thread::spawn(move || {
            let reader_stream = server_stream.try_clone().unwrap();
            let request: Request = read_frame(&mut BufReader::new(reader_stream)).unwrap();
            assert_eq!(request.operation, Operation::Subscribe);
            write_frame(
                &mut server_stream,
                &success(request.request_id, serde_json::json!({"subscribed": true})),
            )
            .unwrap();
            write_frame(
                &mut server_stream,
                &Event {
                    protocol_version: PROTOCOL_VERSION,
                    sequence: 7,
                    event: "changed".to_owned(),
                    payload: Value::Null,
                },
            )
            .unwrap();
        });
        let client = Client {
            reader: BufReader::new(client_stream),
            timeout: DEFAULT_TIMEOUT,
        };
        let mut subscription = client.subscribe().unwrap();
        assert_eq!(subscription.read_event().unwrap().sequence, 7);
        task.join().unwrap();
    }

    #[test]
    fn subscription_rejects_a_mismatched_acknowledgement() {
        use std::{os::unix::net::UnixStream, thread};

        let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
        let task = thread::spawn(move || {
            let reader_stream = server_stream.try_clone().unwrap();
            let _: Request = read_frame(&mut BufReader::new(reader_stream)).unwrap();
            write_frame(
                &mut server_stream,
                &success(Uuid::new_v4(), serde_json::json!({"subscribed": true})),
            )
            .unwrap();
        });
        let client = Client {
            reader: BufReader::new(client_stream),
            timeout: DEFAULT_TIMEOUT,
        };
        assert!(matches!(
            client.subscribe(),
            Err(TransportError::RequestIdMismatch)
        ));
        task.join().unwrap();
    }
}
