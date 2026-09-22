//! Version 1 newline-delimited JSON message contract.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u32,
    pub request_id: Uuid,
    pub operation: Operation,
    pub payload: Value,
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

#[cfg(test)]
mod tests {
    use serde::{Serialize, de::DeserializeOwned};

    use super::{Event, PROTOCOL_VERSION, Request, Response};

    fn assert_json_fixture<T>(fixture: &str)
    where
        T: DeserializeOwned + Serialize + PartialEq + std::fmt::Debug,
    {
        let frame = fixture.strip_suffix('\n').unwrap_or(fixture);
        assert!(!frame.contains('\n'), "wire fixture must be one frame");
        let value: T = serde_json::from_str(frame).expect("valid fixture");
        let encoded = serde_json::to_string(&value).expect("serialize message");
        let expected: serde_json::Value = serde_json::from_str(frame).expect("JSON fixture");
        let actual: serde_json::Value = serde_json::from_str(&encoded).expect("encoded JSON");
        assert_eq!(actual, expected, "serialized wire shape changed");
        let reparsed: T = serde_json::from_str(&encoded).expect("round-trip message");
        assert_eq!(reparsed, value);
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
            serde_json::from_str(include_str!("../../tests/fixtures/ipc_request_v1.json"))
                .expect("valid request fixture");
        assert_eq!(request.protocol_version, PROTOCOL_VERSION);
    }

    #[test]
    fn unknown_request_fields_are_rejected() {
        let frame = r#"{"protocol_version":1,"request_id":"24699e94-5c3f-4d69-a400-d3f08102787c","operation":"status","payload":{},"extra":true}"#;
        assert!(serde_json::from_str::<Request>(frame).is_err());
    }
}
