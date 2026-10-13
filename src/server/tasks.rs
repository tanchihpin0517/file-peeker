//! The runtime's tasks, as the client protocol carries them.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    protocol::client_v1::{self, progress},
    task::{Measure, TaskInfo, TaskState},
};

pub(super) fn task_message(info: &TaskInfo) -> client_v1::Task {
    let (state, error, waiting_on_resource) = match &info.state {
        TaskState::Queued { waiting_on } => (
            client_v1::TaskState::Queued,
            String::new(),
            waiting_on.is_some(),
        ),
        TaskState::Running => (client_v1::TaskState::Running, String::new(), false),
        TaskState::Cancelling => (client_v1::TaskState::Cancelling, String::new(), false),
        TaskState::Succeeded => (client_v1::TaskState::Succeeded, String::new(), false),
        TaskState::Failed(error) => (client_v1::TaskState::Failed, error.to_string(), false),
        TaskState::Cancelled => (client_v1::TaskState::Cancelled, String::new(), false),
    };
    let measure = match info.progress.measure {
        Measure::Indeterminate => None,
        Measure::Items { done, total } => {
            Some(progress::Measure::Items(client_v1::Count { done, total }))
        }
        Measure::Bytes { done, total } => {
            Some(progress::Measure::Bytes(client_v1::Count { done, total }))
        }
    };
    client_v1::Task {
        id: info.id.to_string(),
        parent: info
            .parent
            .map(|parent| parent.to_string())
            .unwrap_or_default(),
        kind: info.descriptor.kind.to_owned(),
        subject: info
            .descriptor
            .subject
            .iter()
            .map(|operand| client_v1::Operand {
                name: operand.name.to_owned(),
                value: operand.value.clone(),
            })
            .collect(),
        state: state.into(),
        error,
        waiting_on_resource,
        progress: Some(client_v1::Progress {
            phase: info.progress.phase.unwrap_or_default().to_owned(),
            measure,
        }),
        submitted_unix_millis: unix_millis(info.submitted),
        settled_unix_millis: info.settled.map(unix_millis),
    }
}

fn unix_millis(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX),
    }
}
