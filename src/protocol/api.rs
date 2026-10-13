//! The public API's wire shapes: one JSON object per line in each direction.
//!
//! ```text
//! -> {"id":1,"method":"server.status"}
//! <- {"id":1,"result":{"version":"0.0.0","pid":4242,...}}
//! -> {"id":2,"method":"server.nope"}
//! <- {"id":2,"error":{"code":"unknown_method","message":"..."}}
//! ```
//!
//! A response carries the request's `id`, so a caller may pipeline. A line that
//! is not a request at all is answered with `"id": null`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The API's generation, separate from the client protocol's version so the
/// private protocol can change while this one stays stable.
pub(crate) const API_GENERATION: u32 = 1;

pub(crate) const METHOD_STATUS: &str = "server.status";
pub(crate) const METHOD_STOP: &str = "server.stop";
/// Starts a `demo.count` task: `{"seconds": 5, "fail": false}`, both optional.
/// Answers `{"id": "<task id>"}` at once; the task runs on.
pub(crate) const METHOD_TASK_DEMO: &str = "task.demo";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Request {
    pub id: Option<u64>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Response {
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl Response {
    pub(crate) fn ok(id: Option<u64>, result: Value) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub(crate) fn err(id: Option<u64>, code: &str, message: impl Into<String>) -> Self {
        Self {
            id,
            result: None,
            error: Some(ApiError {
                code: code.to_owned(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ApiError {
    pub code: String,
    pub message: String,
}

pub(crate) const ERROR_INVALID_REQUEST: &str = "invalid_request";
pub(crate) const ERROR_UNKNOWN_METHOD: &str = "unknown_method";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct DemoParams {
    #[serde(default = "DemoParams::default_seconds")]
    pub seconds: u64,
    #[serde(default)]
    pub fail: bool,
}

impl DemoParams {
    fn default_seconds() -> u64 {
        5
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Submitted {
    pub id: String,
}

/// `server.status`'s result.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ServerStatus {
    pub version: String,
    pub pid: u32,
    pub client_protocol: u32,
    pub api_generation: u32,
    pub started_unix_seconds: u64,
    pub home: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_without_params_omits_them() {
        let request = Request {
            id: Some(1),
            method: METHOD_STATUS.to_owned(),
            params: Value::Null,
        };
        let line = serde_json::to_string(&request).unwrap();
        assert_eq!(line, r#"{"id":1,"method":"server.status"}"#);
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), request);
    }

    #[test]
    fn a_response_carries_either_result_or_error() {
        let ok = serde_json::to_string(&Response::ok(Some(1), Value::Bool(true))).unwrap();
        assert_eq!(ok, r#"{"id":1,"result":true}"#);
        let err = serde_json::to_string(&Response::err(None, ERROR_UNKNOWN_METHOD, "no")).unwrap();
        assert_eq!(
            err,
            r#"{"id":null,"error":{"code":"unknown_method","message":"no"}}"#
        );
    }
}
