mod connection;
mod view;

use std::{collections::HashMap, error::Error, io, sync::Arc, time::Duration};

use crate::{
    Client, ConnectionId, CopyConflict, CopyResult, EntryKind, ServerLocation, host_path::HostPath,
};
use tokio::{sync::mpsc, task::JoinHandle};
use uuid::Uuid;

use self::connection::{ConnectionRecord, ConnectionRecordId, ConnectionStatus};
use super::{
    EVENT_CHANNEL_CAPACITY,
    browser_context::{BrowserContext, BrowserContextEvent, BrowserContextId},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputMode {
    Main,
    /// The filter prompt is open over the active browser. It is not an overlay:
    /// the query belongs to the Browser Context whose rows it narrows, and this
    /// mode is derived from it.
    Filter,
    ConnectionManager,
    TargetChooser,
    RemoteInput,
    NewBrowser,
    ConnectionCloseConfirmation,
    TransferOverwriteConfirmation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReturnMode {
    Main,
    ConnectionManager,
}

#[derive(Debug)]
struct NewBrowserDialog {
    connection_id: ConnectionRecordId,
    input: String,
    request_id: Option<Uuid>,
    error: Option<String>,
    return_mode: ReturnMode,
}

#[derive(Debug)]
enum Overlay {
    None,
    ConnectionManager,
    TargetChooser { selected_ssh: bool },
    RemoteInput { input: String },
    NewBrowser(NewBrowserDialog),
    CloseConfirmation { connection_id: ConnectionRecordId },
    TransferOverwriteConfirmation(TransferRequest),
}

#[derive(Debug)]
struct BrowserTab {
    connection_id: ConnectionRecordId,
    context: BrowserContext,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TransferSelection {
    connection_id: ConnectionRecordId,
    path: HostPath,
    name: HostPath,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TransferRequest {
    source_connection_id: ConnectionRecordId,
    source_path: HostPath,
    source_name: HostPath,
    destination_connection_id: ConnectionRecordId,
    destination_context_id: BrowserContextId,
    destination_root: HostPath,
}

#[derive(Debug)]
struct TransferTask {
    id: Uuid,
    request: TransferRequest,
    handle: JoinHandle<()>,
}

/// How long a quit key waits for the press that confirms it.
const QUIT_CONFIRMATION: Duration = Duration::from_secs(2);

/// A quit key waiting to be confirmed, and the timer that gives up on it.
///
/// The timer is a tracked task reporting through the lifecycle channel rather than a
/// deadline the event loop selects over, because the loop deliberately owns no timer:
/// it draws when something happens, and an expiring confirmation is something
/// happening. Its ID is what makes a timer from an earlier press harmless.
#[derive(Debug)]
struct PendingQuit {
    id: Uuid,
    timer: JoinHandle<()>,
}

#[derive(Debug)]
enum LifecycleEvent {
    ConnectionReady {
        record_id: ConnectionRecordId,
        connection: ConnectionId,
    },
    ConnectionFailed {
        record_id: ConnectionRecordId,
        error: String,
    },
    PathResolved {
        record_id: ConnectionRecordId,
        request_id: Uuid,
        result: Result<HostPath, String>,
    },
    ConnectionClosed {
        record_id: ConnectionRecordId,
        result: io::Result<()>,
    },
    TransferCompleted {
        transfer_id: Uuid,
        request: TransferRequest,
        conflict: CopyConflict,
        result: Result<CopyResult, io::Error>,
    },
    QuitUnconfirmed {
        id: Uuid,
    },
}

#[derive(Debug)]
pub(crate) struct App {
    client: Arc<Client>,
    connections: HashMap<ConnectionRecordId, ConnectionRecord>,
    connection_order: Vec<ConnectionRecordId>,
    label_counts: HashMap<String, usize>,
    selected_connection_id: Option<ConnectionRecordId>,
    contexts: HashMap<BrowserContextId, BrowserTab>,
    context_order: Vec<BrowserContextId>,
    active_context_id: Option<BrowserContextId>,
    overlay: Overlay,
    events: mpsc::Sender<BrowserContextEvent>,
    event_receiver: mpsc::Receiver<BrowserContextEvent>,
    lifecycle_sender: mpsc::UnboundedSender<LifecycleEvent>,
    lifecycle_receiver: mpsc::UnboundedReceiver<LifecycleEvent>,
    start_tasks: HashMap<ConnectionRecordId, JoinHandle<()>>,
    path_tasks: HashMap<Uuid, JoinHandle<()>>,
    close_tasks: HashMap<ConnectionRecordId, JoinHandle<()>>,
    transfer_selection: Option<TransferSelection>,
    transfer_task: Option<TransferTask>,
    pending_quit: Option<PendingQuit>,
    status_message: Option<String>,
    status_error: Option<String>,
}

impl App {
    pub(crate) fn new() -> Self {
        let (events, event_receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (lifecycle_sender, lifecycle_receiver) = mpsc::unbounded_channel();
        Self {
            client: Client::new(),
            connections: HashMap::new(),
            connection_order: Vec::new(),
            label_counts: HashMap::new(),
            selected_connection_id: None,
            contexts: HashMap::new(),
            context_order: Vec::new(),
            active_context_id: None,
            overlay: Overlay::ConnectionManager,
            events,
            event_receiver,
            lifecycle_sender,
            lifecycle_receiver,
            start_tasks: HashMap::new(),
            path_tasks: HashMap::new(),
            close_tasks: HashMap::new(),
            transfer_selection: None,
            transfer_task: None,
            pending_quit: None,
            status_message: None,
            status_error: None,
        }
    }

    pub(crate) async fn start(&mut self, path: Option<&str>) -> Result<(), Box<dyn Error>> {
        let Some(path) = path else {
            self.overlay = Overlay::ConnectionManager;
            return Ok(());
        };
        let record_id =
            self.insert_connection_record(ServerLocation::Embedded, ConnectionStatus::Starting);
        let connection = self.client.open(ServerLocation::Embedded).await?;
        if let Some(record) = self.connections.get_mut(&record_id) {
            record.connection = Some(connection);
            record.status = ConnectionStatus::Ready;
        }
        let root_path = self
            .client
            .op_resolve_path(&connection, &HostPath::from_literal(path))
            .await?;
        let context_id = self.add_context(record_id, connection, root_path);
        self.activate_context(context_id);
        self.overlay = Overlay::None;
        Ok(())
    }

    fn insert_connection_record(
        &mut self,
        target: ServerLocation,
        status: ConnectionStatus,
    ) -> ConnectionRecordId {
        let base = match &target {
            ServerLocation::Embedded => "Local".to_owned(),
            ServerLocation::Ssh { destination, .. } => destination.clone(),
        };
        let count = self.label_counts.entry(base.clone()).or_default();
        *count += 1;
        let label = if *count == 1 {
            base
        } else {
            format!("{base}#{count}")
        };
        let id = ConnectionRecordId::new();
        self.connections.insert(
            id,
            ConnectionRecord {
                id,
                target,
                label,
                status,
                connection: None,
                last_active_context_id: None,
            },
        );
        self.connection_order.push(id);
        if self.selected_connection_id.is_none() {
            self.selected_connection_id = Some(id);
        }
        id
    }

    fn add_context(
        &mut self,
        connection_id: ConnectionRecordId,
        connection: ConnectionId,
        root_path: HostPath,
    ) -> BrowserContextId {
        let context = BrowserContext::new(
            Arc::clone(&self.client),
            connection,
            root_path,
            self.events.clone(),
        );
        let id = context.id();
        self.contexts.insert(
            id,
            BrowserTab {
                connection_id,
                context,
            },
        );
        self.context_order.push(id);
        id
    }

    fn activate_context(&mut self, id: BrowserContextId) {
        let Some(tab) = self.contexts.get(&id) else {
            return;
        };
        self.active_context_id = Some(id);
        if let Some(connection) = self.connections.get_mut(&tab.connection_id) {
            connection.last_active_context_id = Some(id);
        }
    }

    /// Applies every event already queued, without waiting for more.
    pub(crate) fn pump_events(&mut self) {
        while let Ok(event) = self.event_receiver.try_recv() {
            self.apply_browser_event(event);
        }
        while let Ok(event) = self.lifecycle_receiver.try_recv() {
            self.apply_lifecycle_event(event);
        }
    }

    /// Waits for the next event, then keeps collecting for `budget` before
    /// returning, so one redraw covers a frame's worth of a streaming listing
    /// instead of one redraw per entry.
    ///
    /// Cancel safe: an event this returns early from is applied, and any event
    /// still queued stays queued for the next call.
    pub(crate) async fn next_events(&mut self, budget: Duration) {
        tokio::select! {
            event = self.event_receiver.recv() => {
                if let Some(event) = event {
                    self.apply_browser_event(event);
                }
            }
            event = self.lifecycle_receiver.recv() => {
                if let Some(event) = event {
                    self.apply_lifecycle_event(event);
                }
            }
        }
        tokio::time::sleep(budget).await;
        self.pump_events();
    }

    fn apply_browser_event(&mut self, event: BrowserContextEvent) {
        if let Some(tab) = self.contexts.get_mut(&event.context_id()) {
            tab.context.apply(event);
        }
    }

    fn apply_lifecycle_event(&mut self, event: LifecycleEvent) {
        match event {
            LifecycleEvent::ConnectionReady {
                record_id,
                connection,
            } => {
                self.start_tasks.remove(&record_id);
                let Some(record) = self.connections.get_mut(&record_id) else {
                    return;
                };
                if !matches!(record.status, ConnectionStatus::Starting) {
                    return;
                }
                record.connection = Some(connection);
                record.status = ConnectionStatus::Ready;
            }
            LifecycleEvent::ConnectionFailed { record_id, error } => {
                self.start_tasks.remove(&record_id);
                if let Some(record) = self.connections.get_mut(&record_id)
                    && matches!(record.status, ConnectionStatus::Starting)
                {
                    record.status = ConnectionStatus::Failed(error);
                }
            }
            LifecycleEvent::PathResolved {
                record_id,
                request_id,
                result,
            } => {
                self.path_tasks.remove(&request_id);
                let current = matches!(
                    &self.overlay,
                    Overlay::NewBrowser(dialog)
                        if dialog.connection_id == record_id
                            && dialog.request_id == Some(request_id)
                );
                if !current {
                    return;
                }
                match result {
                    Ok(path) => {
                        let connection = self
                            .connections
                            .get(&record_id)
                            .and_then(|record| record.connection);
                        if let Some(connection) = connection {
                            let context_id = self.add_context(record_id, connection, path);
                            self.activate_context(context_id);
                            self.overlay = Overlay::None;
                        }
                    }
                    Err(error) => {
                        if let Overlay::NewBrowser(dialog) = &mut self.overlay {
                            dialog.request_id = None;
                            dialog.error = Some(error);
                        }
                    }
                }
            }
            LifecycleEvent::ConnectionClosed { record_id, result } => {
                self.close_tasks.remove(&record_id);
                self.remove_connection_record(record_id);
                if let Err(error) = result {
                    self.status_error = Some(format!("Error closing connection: {error}"));
                }
            }
            LifecycleEvent::TransferCompleted {
                transfer_id,
                request,
                conflict,
                result,
            } => self.apply_transfer_completed(transfer_id, request, conflict, result),
            // A timer from a press that has already been answered says nothing about
            // the one waiting now.
            LifecycleEvent::QuitUnconfirmed { id } => {
                if self
                    .pending_quit
                    .as_ref()
                    .is_some_and(|pending| pending.id == id)
                {
                    self.pending_quit = None;
                }
            }
        }
    }

    fn apply_transfer_completed(
        &mut self,
        transfer_id: Uuid,
        request: TransferRequest,
        conflict: CopyConflict,
        result: Result<CopyResult, io::Error>,
    ) {
        if self
            .transfer_task
            .as_ref()
            .is_none_or(|task| task.id != transfer_id)
        {
            return;
        }
        self.transfer_task = None;
        match result {
            Ok(result) => {
                self.status_error = None;
                self.status_message = Some(format!(
                    "Copied {} bytes to {}",
                    result.bytes_copied,
                    result.destination_path.display()
                ));
                if let Some(tab) = self.contexts.get_mut(&request.destination_context_id)
                    && tab.connection_id == request.destination_connection_id
                    && *tab.context.root_path() == request.destination_root
                {
                    tab.context.refresh();
                }
            }
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    && conflict == CopyConflict::FailIfExists =>
            {
                self.status_message =
                    Some(format!("{} already exists", request.source_name.display()));
                self.overlay = Overlay::TransferOverwriteConfirmation(request);
            }
            Err(error) => {
                self.status_error = Some(format!(
                    "Error copying {}: {error}",
                    request.source_name.display()
                ));
            }
        }
    }

    pub(crate) fn input_mode(&self) -> InputMode {
        match self.overlay {
            Overlay::None
                if self
                    .active_context()
                    .is_some_and(|context| context.filter().is_some()) =>
            {
                InputMode::Filter
            }
            Overlay::None => InputMode::Main,
            Overlay::ConnectionManager => InputMode::ConnectionManager,
            Overlay::TargetChooser { .. } => InputMode::TargetChooser,
            Overlay::RemoteInput { .. } => InputMode::RemoteInput,
            Overlay::NewBrowser(_) => InputMode::NewBrowser,
            Overlay::CloseConfirmation { .. } => InputMode::ConnectionCloseConfirmation,
            Overlay::TransferOverwriteConfirmation(_) => InputMode::TransferOverwriteConfirmation,
        }
    }

    pub(crate) fn open_connection_manager(&mut self) {
        self.overlay = Overlay::ConnectionManager;
        self.repair_connection_selection();
    }

    pub(crate) fn dismiss_overlay(&mut self) {
        self.overlay = Overlay::None;
    }

    pub(crate) fn open_add_connection(&mut self) {
        self.overlay = Overlay::TargetChooser {
            selected_ssh: false,
        };
    }

    pub(crate) fn move_target_selection(&mut self, down: bool) {
        if let Overlay::TargetChooser { selected_ssh } = &mut self.overlay {
            *selected_ssh = down;
        }
    }

    pub(crate) fn submit_target_choice(&mut self) {
        let Overlay::TargetChooser { selected_ssh } = self.overlay else {
            return;
        };
        if selected_ssh {
            self.overlay = Overlay::RemoteInput {
                input: String::new(),
            };
        } else {
            self.overlay = Overlay::ConnectionManager;
            self.start_connection(ServerLocation::Embedded);
        }
    }

    fn start_connection(&mut self, target: ServerLocation) {
        let record_id = self.insert_connection_record(target.clone(), ConnectionStatus::Starting);
        let client = Arc::clone(&self.client);
        let sender = self.lifecycle_sender.clone();
        self.start_tasks.insert(
            record_id,
            tokio::spawn(async move {
                match client.open(target).await {
                    Ok(connection) => {
                        if sender
                            .send(LifecycleEvent::ConnectionReady {
                                record_id,
                                connection,
                            })
                            .is_err()
                        {
                            let _ = client.close(&connection).await;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(LifecycleEvent::ConnectionFailed {
                            record_id,
                            error: error.to_string(),
                        });
                    }
                }
            }),
        );
    }

    pub(crate) fn input_character(&mut self, character: char) {
        match &mut self.overlay {
            Overlay::RemoteInput { input } => input.push(character),
            Overlay::NewBrowser(dialog) if dialog.request_id.is_none() => {
                dialog.input.push(character);
                dialog.error = None;
            }
            _ => {}
        }
    }

    pub(crate) fn input_backspace(&mut self) {
        match &mut self.overlay {
            Overlay::RemoteInput { input } => {
                input.pop();
            }
            Overlay::NewBrowser(dialog) if dialog.request_id.is_none() => {
                dialog.input.pop();
                dialog.error = None;
            }
            _ => {}
        }
    }

    pub(crate) fn submit_text_input(&mut self) {
        match &self.overlay {
            Overlay::RemoteInput { input } if !input.trim().is_empty() => {
                let destination = input.clone();
                self.overlay = Overlay::ConnectionManager;
                self.start_connection(ServerLocation::Ssh {
                    destination,
                    force_install: false,
                });
            }
            Overlay::NewBrowser(dialog)
                if dialog.request_id.is_none() && !dialog.input.trim().is_empty() =>
            {
                self.submit_new_browser();
            }
            _ => {}
        }
    }

    pub(crate) fn cancel_dialog(&mut self) {
        match &self.overlay {
            Overlay::NewBrowser(dialog) => {
                if let Some(request_id) = dialog.request_id
                    && let Some(task) = self.path_tasks.remove(&request_id)
                {
                    task.abort();
                }
                self.overlay = match dialog.return_mode {
                    ReturnMode::Main => Overlay::None,
                    ReturnMode::ConnectionManager => Overlay::ConnectionManager,
                };
            }
            Overlay::TargetChooser { .. } | Overlay::RemoteInput { .. } => {
                self.overlay = Overlay::ConnectionManager;
            }
            Overlay::CloseConfirmation { .. } => self.overlay = Overlay::ConnectionManager,
            _ => self.overlay = Overlay::None,
        }
    }

    pub(crate) fn open_new_browser_for_active(&mut self) {
        let Some(active_id) = self.active_context_id else {
            return;
        };
        let Some(tab) = self.contexts.get(&active_id) else {
            return;
        };
        self.overlay = Overlay::NewBrowser(NewBrowserDialog {
            connection_id: tab.connection_id,
            input: tab.context.root_path().display().into_owned(),
            request_id: None,
            error: None,
            return_mode: ReturnMode::Main,
        });
    }

    fn open_new_browser_for_connection(
        &mut self,
        connection_id: ConnectionRecordId,
        return_mode: ReturnMode,
    ) {
        let default_path = self
            .connections
            .get(&connection_id)
            .and_then(|record| record.last_active_context_id)
            .and_then(|id| self.contexts.get(&id))
            .map_or_else(
                || "~".to_owned(),
                |tab| tab.context.root_path().display().into_owned(),
            );
        self.overlay = Overlay::NewBrowser(NewBrowserDialog {
            connection_id,
            input: default_path,
            request_id: None,
            error: None,
            return_mode,
        });
    }

    pub(crate) fn open_new_browser_for_selected_connection(&mut self) {
        let Some(connection_id) = self.selected_connection_id else {
            return;
        };
        if self
            .connections
            .get(&connection_id)
            .is_some_and(|record| matches!(record.status, ConnectionStatus::Ready))
        {
            self.open_new_browser_for_connection(connection_id, ReturnMode::ConnectionManager);
        }
    }

    fn submit_new_browser(&mut self) {
        let Overlay::NewBrowser(dialog) = &mut self.overlay else {
            return;
        };
        let Some(connection) = self
            .connections
            .get(&dialog.connection_id)
            .and_then(|record| record.connection)
        else {
            dialog.error = Some("Connection is not ready".into());
            return;
        };
        let request_id = Uuid::new_v4();
        dialog.request_id = Some(request_id);
        dialog.error = None;
        let record_id = dialog.connection_id;
        let path = dialog.input.clone();
        let client = Arc::clone(&self.client);
        let sender = self.lifecycle_sender.clone();
        self.path_tasks.insert(
            request_id,
            tokio::spawn(async move {
                // The dialog holds text a person typed, so it is escaped here on
                // its way to the host.
                let result = client
                    .op_resolve_path(&connection, &HostPath::from_literal(&path))
                    .await
                    .map_err(|error| error.to_string());
                let _ = sender.send(LifecycleEvent::PathResolved {
                    record_id,
                    request_id,
                    result,
                });
            }),
        );
    }

    pub(crate) fn move_connection_selection(&mut self, down: bool) {
        if self.connection_order.is_empty() {
            self.selected_connection_id = None;
            return;
        }
        let index = self
            .selected_connection_id
            .and_then(|id| {
                self.connection_order
                    .iter()
                    .position(|candidate| *candidate == id)
            })
            .unwrap_or(0);
        let next = if down {
            (index + 1).min(self.connection_order.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.selected_connection_id = Some(self.connection_order[next]);
    }

    pub(crate) fn activate_selected_connection(&mut self) {
        let Some(connection_id) = self.selected_connection_id else {
            return;
        };
        let Some(record) = self.connections.get(&connection_id) else {
            return;
        };
        if !matches!(record.status, ConnectionStatus::Ready) {
            return;
        }
        if let Some(context_id) = record.last_active_context_id {
            self.activate_context(context_id);
            self.overlay = Overlay::None;
        } else {
            self.open_new_browser_for_connection(connection_id, ReturnMode::ConnectionManager);
        }
    }

    pub(crate) fn delete_selected_connection(&mut self) {
        let Some(connection_id) = self.selected_connection_id else {
            return;
        };
        let Some(record) = self.connections.get(&connection_id) else {
            return;
        };
        match record.status {
            ConnectionStatus::Failed(_) if record.connection.is_none() => {
                self.remove_connection_record(connection_id);
            }
            ConnectionStatus::Ready => {
                self.overlay = Overlay::CloseConfirmation { connection_id };
            }
            _ => {}
        }
    }

    pub(crate) fn confirm_connection_close(&mut self) {
        let Overlay::CloseConfirmation { connection_id } = self.overlay else {
            return;
        };
        let connection = {
            let Some(record) = self.connections.get_mut(&connection_id) else {
                self.overlay = Overlay::ConnectionManager;
                return;
            };
            record.status = ConnectionStatus::Closing;
            record.connection.take()
        };
        self.remove_contexts_for_connection(connection_id);
        self.clear_transfer_state_for_connection(connection_id);
        self.overlay = Overlay::ConnectionManager;
        let Some(connection) = connection else {
            self.remove_connection_record(connection_id);
            return;
        };
        let client = Arc::clone(&self.client);
        let sender = self.lifecycle_sender.clone();
        let transfer = self.take_transfer_for_connection(connection_id);
        self.close_tasks.insert(
            connection_id,
            tokio::spawn(async move {
                if let Some(task) = transfer {
                    task.handle.abort();
                    let _ = task.handle.await;
                }
                let result = client.close(&connection).await;
                let _ = sender.send(LifecycleEvent::ConnectionClosed {
                    record_id: connection_id,
                    result,
                });
            }),
        );
    }

    fn remove_connection_record(&mut self, id: ConnectionRecordId) {
        self.clear_transfer_state_for_connection(id);
        let old_index = self
            .connection_order
            .iter()
            .position(|candidate| *candidate == id);
        self.connections.remove(&id);
        self.connection_order.retain(|candidate| *candidate != id);
        if self.selected_connection_id == Some(id) {
            self.selected_connection_id = old_index.and_then(|index| {
                self.connection_order
                    .get(index.min(self.connection_order.len().saturating_sub(1)))
                    .copied()
            });
        }
        self.repair_connection_selection();
    }

    fn repair_connection_selection(&mut self) {
        if self
            .selected_connection_id
            .is_some_and(|id| self.connections.contains_key(&id))
        {
            return;
        }
        self.selected_connection_id = self.connection_order.first().copied();
    }

    pub(crate) fn switch_context(&mut self, next: bool) {
        if self.context_order.len() < 2 {
            return;
        }
        let index = self
            .active_context_id
            .and_then(|id| {
                self.context_order
                    .iter()
                    .position(|candidate| *candidate == id)
            })
            .unwrap_or(0);
        let index = if next {
            (index + 1) % self.context_order.len()
        } else if index == 0 {
            self.context_order.len() - 1
        } else {
            index - 1
        };
        self.activate_context(self.context_order[index]);
    }

    pub(crate) fn close_active_context(&mut self) {
        if let Some(id) = self.active_context_id {
            self.remove_context(id);
        }
    }

    fn remove_context(&mut self, id: BrowserContextId) {
        let Some(index) = self
            .context_order
            .iter()
            .position(|candidate| *candidate == id)
        else {
            return;
        };
        let Some(tab) = self.contexts.remove(&id) else {
            return;
        };
        self.context_order.remove(index);
        self.repair_last_active(tab.connection_id);
        if self.active_context_id == Some(id) {
            self.active_context_id = self
                .context_order
                .get(index.min(self.context_order.len().saturating_sub(1)))
                .copied();
            if let Some(active) = self.active_context_id {
                self.activate_context(active);
            }
        }
    }

    fn remove_contexts_for_connection(&mut self, connection_id: ConnectionRecordId) {
        let ids: Vec<_> = self
            .context_order
            .iter()
            .copied()
            .filter(|id| {
                self.contexts
                    .get(id)
                    .is_some_and(|tab| tab.connection_id == connection_id)
            })
            .collect();
        for id in ids {
            self.remove_context(id);
        }
    }

    fn repair_last_active(&mut self, connection_id: ConnectionRecordId) {
        let replacement = self.context_order.iter().rev().copied().find(|id| {
            self.contexts
                .get(id)
                .is_some_and(|tab| tab.connection_id == connection_id)
        });
        if let Some(record) = self.connections.get_mut(&connection_id) {
            record.last_active_context_id = replacement;
        }
    }

    pub(crate) fn refresh_active_context(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.refresh();
        }
    }

    pub(crate) fn yank_active_selection(&mut self) {
        let Some(context_id) = self.active_context_id else {
            self.status_message = Some("No active browser to copy from".into());
            return;
        };
        let Some(tab) = self.contexts.get(&context_id) else {
            self.status_message = Some("No active browser to copy from".into());
            return;
        };
        let connection_id = tab.connection_id;
        let Some(record) = self.connections.get(&connection_id) else {
            self.status_message = Some("Source connection is unavailable".into());
            return;
        };
        if !matches!(record.status, ConnectionStatus::Ready) || record.connection.is_none() {
            self.status_message = Some("Source connection is not ready".into());
            return;
        }
        let Some(selection) = tab.context.selected_entry() else {
            self.status_message = Some(match tab.context.status() {
                crate::tui::browser_context::ListingStatus::Loading => {
                    "Cannot copy while the browser is loading".into()
                }
                _ => "No file is selected".into(),
            });
            return;
        };
        if selection.kind != EntryKind::File {
            self.status_message = Some(format!(
                "Cannot copy {}: only regular files are supported",
                selection.name.display()
            ));
            return;
        }
        let label = record.label.clone();
        self.transfer_selection = Some(TransferSelection {
            connection_id,
            path: selection.path,
            name: selection.name.clone(),
        });
        self.status_error = None;
        self.status_message = Some(format!("Yanked {} from {label}", selection.name.display()));
    }

    pub(crate) fn paste_into_active_context(&mut self) {
        let Some(selection) = self.transfer_selection.clone() else {
            self.status_message = Some("Nothing has been copied".into());
            return;
        };
        let Some(context_id) = self.active_context_id else {
            self.status_message = Some("No active destination browser".into());
            return;
        };
        let Some(tab) = self.contexts.get(&context_id) else {
            self.status_message = Some("No active destination browser".into());
            return;
        };
        let request = TransferRequest {
            source_connection_id: selection.connection_id,
            source_path: selection.path,
            source_name: selection.name,
            destination_connection_id: tab.connection_id,
            destination_context_id: context_id,
            destination_root: tab.context.root_path().to_owned(),
        };
        self.start_transfer(request, CopyConflict::FailIfExists);
    }

    fn start_transfer(&mut self, request: TransferRequest, conflict: CopyConflict) {
        if self.transfer_task.is_some() {
            self.status_message = Some("A file transfer is already running".into());
            return;
        }
        let source = self
            .connections
            .get(&request.source_connection_id)
            .filter(|record| matches!(record.status, ConnectionStatus::Ready))
            .and_then(|record| record.connection);
        let destination = self
            .connections
            .get(&request.destination_connection_id)
            .filter(|record| matches!(record.status, ConnectionStatus::Ready))
            .and_then(|record| record.connection);
        let (Some(source), Some(destination)) = (source, destination) else {
            self.status_message = Some("Source or destination connection is not ready".into());
            return;
        };
        let transfer_id = Uuid::new_v4();
        let client = Arc::clone(&self.client);
        let sender = self.lifecycle_sender.clone();
        let event_request = request.clone();
        let source_path = request.source_path.clone();
        let destination_root = request.destination_root.clone();
        let source_name = request.source_name.clone();
        let handle = tokio::spawn(async move {
            let result = client
                .op_copy_file(
                    &source,
                    &source_path,
                    &destination,
                    &destination_root,
                    &source_name,
                    conflict,
                )
                .await;
            let _ = sender.send(LifecycleEvent::TransferCompleted {
                transfer_id,
                request: event_request,
                conflict,
                result,
            });
        });
        self.status_error = None;
        self.status_message = Some(format!("Copying {}…", request.source_name.display()));
        self.transfer_task = Some(TransferTask {
            id: transfer_id,
            request,
            handle,
        });
    }

    pub(crate) fn confirm_transfer_overwrite(&mut self) {
        let Overlay::TransferOverwriteConfirmation(request) =
            std::mem::replace(&mut self.overlay, Overlay::None)
        else {
            return;
        };
        self.start_transfer(request, CopyConflict::Replace);
    }

    fn clear_transfer_state_for_connection(&mut self, connection_id: ConnectionRecordId) {
        if self
            .transfer_selection
            .as_ref()
            .is_some_and(|selection| selection.connection_id == connection_id)
        {
            self.transfer_selection = None;
        }
        if matches!(
            &self.overlay,
            Overlay::TransferOverwriteConfirmation(request)
                if request.source_connection_id == connection_id
                    || request.destination_connection_id == connection_id
        ) {
            self.overlay = Overlay::None;
        }
    }

    fn take_transfer_for_connection(
        &mut self,
        connection_id: ConnectionRecordId,
    ) -> Option<TransferTask> {
        let uses_connection = self.transfer_task.as_ref().is_some_and(|task| {
            task.request.source_connection_id == connection_id
                || task.request.destination_connection_id == connection_id
        });
        uses_connection.then(|| self.transfer_task.take()).flatten()
    }

    pub(crate) fn move_active_selection(&mut self, down: bool) {
        if let Some(context) = self.active_context_mut() {
            context.move_selection(down);
        }
    }

    pub(crate) fn scroll_active_half_page(&mut self, down: bool) {
        if let Some(context) = self.active_context_mut() {
            context.scroll_half_page(down);
        }
    }

    /// Takes a press of a quit key, reporting whether it quits.
    ///
    /// The first press only arms a confirmation, so the keys that leave the browser
    /// take two presses; the second takes it. A press that arrives after the
    /// confirmation has lapsed is a first press again rather than the second of a
    /// pair, which is what makes leaving deliberate rather than a matter of timing.
    pub(crate) fn press_quit(&mut self) -> bool {
        if let Some(pending) = self.pending_quit.take() {
            pending.timer.abort();
            return true;
        }
        let id = Uuid::new_v4();
        let sender = self.lifecycle_sender.clone();
        self.pending_quit = Some(PendingQuit {
            id,
            timer: tokio::spawn(async move {
                tokio::time::sleep(QUIT_CONFIRMATION).await;
                let _ = sender.send(LifecycleEvent::QuitUnconfirmed { id });
            }),
        });
        false
    }

    /// Whether a quit key is waiting for the press that confirms it.
    pub(crate) fn quit_pending(&self) -> bool {
        self.pending_quit.is_some()
    }

    /// Stands a waiting confirmation down, which is what any key that is not another
    /// quit press does: a press confirms the press before it, not one buried under a
    /// burst of navigation.
    pub(crate) fn cancel_quit(&mut self) {
        if let Some(pending) = self.pending_quit.take() {
            pending.timer.abort();
        }
    }

    pub(crate) fn open_filter(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.begin_filter();
        }
    }

    pub(crate) fn extend_filter(&mut self, character: char) {
        if let Some(context) = self.active_context_mut() {
            context.extend_filter(character);
        }
    }

    pub(crate) fn shorten_filter(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.shorten_filter();
        }
    }

    pub(crate) fn complete_filter(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.complete_filter();
        }
    }

    pub(crate) fn descend_filter(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.descend_filter();
        }
    }

    pub(crate) fn clear_filter_entry(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.clear_filter_entry();
        }
    }

    pub(crate) fn submit_filter(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.submit_filter();
        }
    }

    pub(crate) fn abandon_filter(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.abandon_filter();
        }
    }

    pub(crate) fn activate_active_selection(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.activate_selected();
        }
    }

    pub(crate) fn enter_active_selection(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.enter_selected();
        }
    }

    pub(crate) fn leave_active_directory(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.leave();
        }
    }

    pub(crate) fn active_context(&self) -> Option<&BrowserContext> {
        self.active_context_id
            .and_then(|id| self.contexts.get(&id))
            .map(|tab| &tab.context)
    }

    fn active_context_mut(&mut self) -> Option<&mut BrowserContext> {
        self.active_context_id
            .and_then(|id| self.contexts.get_mut(&id))
            .map(|tab| &mut tab.context)
    }

    pub(crate) async fn shutdown(&mut self) -> io::Result<()> {
        if let Some(pending) = self.pending_quit.take() {
            pending.timer.abort();
            let _ = pending.timer.await;
        }
        if let Some(task) = self.transfer_task.take() {
            task.handle.abort();
            let _ = task.handle.await;
        }
        for tab in self.contexts.values_mut() {
            tab.context.cancel();
        }
        self.contexts.clear();
        self.context_order.clear();
        self.active_context_id = None;

        for (_, task) in self.path_tasks.drain() {
            task.abort();
            let _ = task.await;
        }
        for (_, task) in self.start_tasks.drain() {
            task.abort();
            let _ = task.await;
        }
        while let Ok(event) = self.lifecycle_receiver.try_recv() {
            self.apply_lifecycle_event(event);
        }

        let close_tasks: Vec<_> = self.close_tasks.drain().map(|(_, task)| task).collect();
        for task in close_tasks {
            let _ = task.await;
        }
        while let Ok(event) = self.lifecycle_receiver.try_recv() {
            self.apply_lifecycle_event(event);
        }

        self.client.close_all().await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::ServerLocation;

    use super::{App, ConnectionStatus, InputMode, LifecycleEvent};
    use crate::tui::browser_context::{BrowserContext, ListingStatus};

    #[test]
    fn duplicate_labels_are_stable_and_ordered() {
        let mut app = App::new();
        let first =
            app.insert_connection_record(ServerLocation::Embedded, ConnectionStatus::Starting);
        let second =
            app.insert_connection_record(ServerLocation::Embedded, ConnectionStatus::Starting);
        assert_eq!(app.connections[&first].label, "Local");
        assert_eq!(app.connections[&second].label, "Local#2");
        app.remove_connection_record(first);
        assert_eq!(app.connections[&second].label, "Local#2");
    }

    #[tokio::test]
    async fn starting_without_a_path_opens_connection_manager() {
        let mut app = App::new();
        app.start(None).await.unwrap();
        assert_eq!(app.input_mode(), InputMode::ConnectionManager);
        assert!(app.connections.is_empty());
        assert!(app.contexts.is_empty());
    }

    #[tokio::test]
    async fn resolves_startup_path_before_creating_browser_context() {
        let mut app = App::new();
        app.start(Some(".")).await.unwrap();
        assert_eq!(
            app.active_context().unwrap().root_path(),
            std::env::current_dir().unwrap().to_str().unwrap()
        );
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn switching_wraps_and_closing_a_context_leaves_its_connection() {
        let mut app = App::new();
        app.start(Some(".")).await.unwrap();
        let connection_id = app.connection_order[0];
        let connection = app.connections[&connection_id].connection.unwrap();
        let second = app.add_context(connection_id, connection, "/".into());
        app.activate_context(second);
        let first = app.context_order[0];
        app.switch_context(true);
        assert_eq!(app.active_context_id, Some(first));
        app.switch_context(false);
        assert_eq!(app.active_context_id, Some(second));
        app.close_active_context();
        assert_eq!(app.active_context_id, Some(first));
        assert!(app.connections.contains_key(&connection_id));
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_without_a_connection_is_harmless() {
        App::new().shutdown().await.unwrap();
    }

    /// The filter prompt is not an overlay. Its query belongs to the Browser
    /// Context whose rows it narrows -- switching tabs must not carry it across --
    /// so the input mode is read back off the active context rather than stored
    /// beside it.
    #[tokio::test]
    async fn filter_mode_is_derived_from_the_active_context() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["alpha.txt", "beta.txt"] {
            tokio::fs::write(directory.path().join(name), b"")
                .await
                .unwrap();
        }
        let mut app = App::new();
        app.start(Some(directory.path().to_str().unwrap()))
            .await
            .unwrap();
        wait_for_listing(&mut app).await;

        assert_eq!(app.input_mode(), InputMode::Main);
        app.open_filter();
        assert_eq!(app.input_mode(), InputMode::Filter);

        app.extend_filter('b');
        assert_eq!(
            app.active_context()
                .and_then(BrowserContext::selected_entry)
                .map(|entry| entry.name),
            Some("beta.txt".into())
        );

        // The first Enter spells out what was narrowed to; the second takes it.
        app.submit_filter();
        assert_eq!(
            app.active_context().and_then(BrowserContext::filter),
            Some("beta.txt")
        );
        assert_eq!(app.input_mode(), InputMode::Filter);

        app.submit_filter();
        assert_eq!(app.input_mode(), InputMode::Main);

        app.shutdown().await.unwrap();
    }

    /// A confirmation that is never answered lapses, and the press after that is a
    /// first press again rather than the second of a pair.
    #[tokio::test]
    async fn an_unanswered_quit_confirmation_lapses() {
        let mut app = App::new();

        assert!(!app.press_quit());
        let lapsed = app.pending_quit.as_ref().expect("armed").id;

        app.lifecycle_sender
            .send(LifecycleEvent::QuitUnconfirmed { id: lapsed })
            .unwrap();
        app.pump_events();
        assert!(!app.quit_pending(), "the timer stood it down");

        assert!(!app.press_quit(), "so this press arms rather than quitting");
        app.lifecycle_sender
            .send(LifecycleEvent::QuitUnconfirmed { id: lapsed })
            .unwrap();
        app.pump_events();
        assert!(
            app.quit_pending(),
            "a timer from the earlier press says nothing about this one"
        );
        assert!(app.press_quit());

        app.shutdown().await.unwrap();
    }

    async fn wait_for_listing(app: &mut App) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                app.pump_events();
                if app
                    .active_context()
                    .is_some_and(|context| context.status() == &ListingStatus::Complete)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn yank_paste_and_overwrite_use_the_tracked_transfer_workflow() {
        let source_dir = tempfile::tempdir().unwrap();
        let destination_dir = tempfile::tempdir().unwrap();
        tokio::fs::write(source_dir.path().join("payload.txt"), b"payload")
            .await
            .unwrap();
        let mut app = App::new();
        app.start(Some(source_dir.path().to_str().unwrap()))
            .await
            .unwrap();
        wait_for_listing(&mut app).await;

        app.yank_active_selection();
        assert_eq!(
            app.transfer_selection
                .as_ref()
                .map(|selection| selection.name.as_escaped()),
            Some("payload.txt")
        );
        let source_context_id = app.active_context_id.unwrap();
        let connection_id = app.contexts[&source_context_id].connection_id;
        let connection = app.connections[&connection_id].connection.unwrap();
        let destination_context_id = app.add_context(
            connection_id,
            connection,
            destination_dir.path().to_str().unwrap().into(),
        );
        app.activate_context(destination_context_id);

        app.paste_into_active_context();
        wait_for_transfer(&mut app).await;
        assert_eq!(
            tokio::fs::read(destination_dir.path().join("payload.txt"))
                .await
                .unwrap(),
            b"payload"
        );
        assert!(app.transfer_selection.is_some());

        app.paste_into_active_context();
        wait_for_transfer(&mut app).await;
        assert_eq!(app.input_mode(), InputMode::TransferOverwriteConfirmation);
        app.confirm_transfer_overwrite();
        wait_for_transfer(&mut app).await;
        assert_eq!(app.input_mode(), InputMode::Main);

        app.shutdown().await.unwrap();
    }

    /// A directory the server streams faster than a frame must land in a couple
    /// of redraws, not one per handful of entries. With events metered out at a
    /// fixed count per loop iteration this needed `entries / limit` wakes, which
    /// is what made a listing visibly fill in a chunk at a time.
    #[tokio::test]
    async fn a_whole_directory_lands_within_a_few_redraws() {
        const ENTRIES: usize = 500;

        let directory = tempfile::tempdir().unwrap();
        for index in 0..ENTRIES {
            tokio::fs::write(directory.path().join(format!("entry-{index}")), b"")
                .await
                .unwrap();
        }
        let mut app = App::new();
        app.start(Some(directory.path().to_str().unwrap()))
            .await
            .unwrap();

        let mut wakes = 0_usize;
        tokio::time::timeout(Duration::from_secs(10), async {
            while app
                .active_context()
                .is_some_and(|context| context.status() != &ListingStatus::Complete)
            {
                app.next_events(super::super::FRAME_BUDGET).await;
                wakes += 1;
            }
        })
        .await
        .unwrap();

        assert_eq!(app.active_context().unwrap().rows().len(), ENTRIES);
        assert!(wakes <= 4, "{ENTRIES} entries took {wakes} redraws to land");

        app.shutdown().await.unwrap();
    }

    async fn wait_for_transfer(app: &mut App) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                app.pump_events();
                if app.transfer_task.is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        app.pump_events();
    }
}
