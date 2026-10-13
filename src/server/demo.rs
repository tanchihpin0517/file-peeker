//! `demo.count`: a task with nothing behind it, so the task machinery can be
//! watched end to end before any real kind exists. It counts for a while,
//! reporting progress, and fails halfway when asked to.

use std::{io, time::Duration};

use crate::task::{AdmissionClass, Measure, Progress, TaskContext, TaskDescriptor, TaskKind};

const STEP: Duration = Duration::from_millis(100);

pub(super) struct Demo {
    pub seconds: u64,
    pub fail: bool,
}

impl TaskKind for Demo {
    type Item = ();
    type Output = ();

    /// Background, so a second demo queues behind the first on its cap of one:
    /// queueing is part of what there is to watch.
    fn describe(&self) -> TaskDescriptor {
        let mut descriptor = TaskDescriptor::new("demo.count", AdmissionClass::Background)
            .with("seconds", self.seconds.to_string());
        if self.fail {
            descriptor = descriptor.with("fail", "true");
        }
        descriptor
    }

    async fn run(self, cx: TaskContext<()>) -> io::Result<()> {
        let total = self.seconds.saturating_mul(10).max(1);
        for done in 1..=total {
            tokio::select! {
                () = tokio::time::sleep(STEP) => {}
                () = cx.cancelled() => return Err(io::ErrorKind::Interrupted.into()),
            }
            cx.progress(Progress {
                phase: Some("counting"),
                measure: Measure::Items {
                    done,
                    total: Some(total),
                },
            });
            if self.fail && done * 2 >= total {
                return Err(io::Error::other("the demo was told to fail"));
            }
        }
        Ok(())
    }
}
