//! The public API socket: one JSON request per line, one response per line.

use std::sync::Arc;

use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    net::{UnixListener, UnixStream},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing::{debug, info, warn};

use super::{ServerInfo, demo::Demo};
use crate::{
    protocol::api::{
        DemoParams, ERROR_INVALID_REQUEST, ERROR_UNKNOWN_METHOD, METHOD_STATUS, METHOD_STOP,
        METHOD_TASK_DEMO, Request, Response, Submitted,
    },
    task::Runtime,
};

/// Accepts API connections until `shutdown`, then waits for the open ones to
/// finish the request they are on.
pub(super) async fn serve(
    listener: UnixListener,
    info: Arc<ServerInfo>,
    runtime: Runtime,
    shutdown: CancellationToken,
) {
    let connections = TaskTracker::new();
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(connection(
                        stream,
                        Arc::clone(&info),
                        runtime.clone(),
                        shutdown.clone(),
                    ));
                }
                Err(error) => warn!(%error, "cannot accept an API connection"),
            },
        }
    }
    connections.close();
    connections.wait().await;
}

async fn connection(
    stream: UnixStream,
    info: Arc<ServerInfo>,
    runtime: Runtime,
    shutdown: CancellationToken,
) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    loop {
        let line = tokio::select! {
            () = shutdown.cancelled() => return,
            line = lines.next_line() => line,
        };
        let line = match line {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(error) => {
                debug!(%error, "API connection closed");
                return;
            }
        };
        let (response, stop) = respond(&line, &info, &runtime);
        let mut encoded = match serde_json::to_vec(&response) {
            Ok(encoded) => encoded,
            Err(error) => {
                warn!(%error, "cannot encode an API response");
                return;
            }
        };
        encoded.push(b'\n');
        if write.write_all(&encoded).await.is_err() {
            return;
        }
        // Stop only once the caller has its answer, so `server stop` can tell
        // the request arrived from the socket going away afterwards.
        if stop {
            info!("stop requested through the API");
            shutdown.cancel();
            return;
        }
    }
}

/// Answers one line, and says whether it asked the server to stop.
fn respond(line: &str, info: &ServerInfo, runtime: &Runtime) -> (Response, bool) {
    let request: Request = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            return (
                Response::err(None, ERROR_INVALID_REQUEST, error.to_string()),
                false,
            );
        }
    };
    match request.method.as_str() {
        METHOD_STATUS => match serde_json::to_value(info.status()) {
            Ok(status) => (Response::ok(request.id, status), false),
            Err(error) => (
                Response::err(request.id, ERROR_INVALID_REQUEST, error.to_string()),
                false,
            ),
        },
        METHOD_TASK_DEMO => (submit_demo(request.id, request.params, runtime), false),
        METHOD_STOP => (
            Response::ok(request.id, Value::Object(serde_json::Map::new())),
            true,
        ),
        other => (
            Response::err(
                request.id,
                ERROR_UNKNOWN_METHOD,
                format!("no method named `{other}`"),
            ),
            false,
        ),
    }
}

fn submit_demo(id: Option<u64>, params: Value, runtime: &Runtime) -> Response {
    let params = if params.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        params
    };
    let params: DemoParams = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(error) => return Response::err(id, ERROR_INVALID_REQUEST, error.to_string()),
    };
    let handle = runtime.submit(Demo {
        seconds: params.seconds,
        fail: params.fail,
    });
    let submitted = Submitted {
        id: handle.id().to_string(),
    };
    // Dropped, the handle detaches: the task runs on in the runtime, and its
    // outcome is in the registry for whoever watches.
    drop(handle);
    match serde_json::to_value(submitted) {
        Ok(result) => Response::ok(id, result),
        Err(error) => Response::err(id, ERROR_INVALID_REQUEST, error.to_string()),
    }
}
