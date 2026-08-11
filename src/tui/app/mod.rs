mod session;
mod view;

use std::{collections::HashMap, error::Error, io, sync::Arc};

use crate::{
    Client, CopyConflict, CopyResult, EntryKind, Session, SessionShutdownError, SessionTarget,
};
use tokio::{sync::mpsc, task::JoinHandle};
use uuid::Uuid;

use self::session::{SessionRecord, SessionRecordId, SessionStatus};
use super::{
    EVENT_CHANNEL_CAPACITY,
    browser_context::{BrowserContext, BrowserContextEvent, BrowserContextId},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputMode {
    Main,
    SessionManager,
    TargetChooser,
    RemoteInput,
    NewBrowser,
    SessionCloseConfirmation,
    FileOpenConfirmation,
    TransferOverwriteConfirmation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReturnMode {
    Main,
    SessionManager,
}

#[derive(Debug)]
struct NewBrowserDialog {
    session_id: SessionRecordId,
    input: String,
    request_id: Option<Uuid>,
    error: Option<String>,
    return_mode: ReturnMode,
}

#[derive(Debug)]
enum Overlay {
    None,
    SessionManager,
    TargetChooser { selected_ssh: bool },
    RemoteInput { input: String },
    NewBrowser(NewBrowserDialog),
    CloseConfirmation { session_id: SessionRecordId },
    TransferOverwriteConfirmation(TransferRequest),
}

#[derive(Debug)]
struct BrowserTab {
    session_id: SessionRecordId,
    context: BrowserContext,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TransferSelection {
    session_id: SessionRecordId,
    path: String,
    name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TransferRequest {
    source_session_id: SessionRecordId,
    source_path: String,
    source_name: String,
    destination_session_id: SessionRecordId,
    destination_context_id: BrowserContextId,
    destination_root: String,
}

#[derive(Debug)]
struct TransferTask {
    id: Uuid,
    request: TransferRequest,
    handle: JoinHandle<()>,
}

#[derive(Debug)]
enum LifecycleEvent {
    SessionReady {
        record_id: SessionRecordId,
        session: Arc<Session>,
    },
    SessionFailed {
        record_id: SessionRecordId,
        error: String,
    },
    PathResolved {
        record_id: SessionRecordId,
        request_id: Uuid,
        result: Result<String, String>,
    },
    SessionClosed {
        record_id: SessionRecordId,
        result: Result<(), SessionShutdownError>,
    },
    TransferCompleted {
        transfer_id: Uuid,
        request: TransferRequest,
        conflict: CopyConflict,
        result: Result<CopyResult, io::Error>,
    },
}

#[derive(Debug)]
pub(crate) struct App {
    client: Arc<Client>,
    sessions: HashMap<SessionRecordId, SessionRecord>,
    session_order: Vec<SessionRecordId>,
    label_counts: HashMap<String, usize>,
    selected_session_id: Option<SessionRecordId>,
    contexts: HashMap<BrowserContextId, BrowserTab>,
    context_order: Vec<BrowserContextId>,
    active_context_id: Option<BrowserContextId>,
    overlay: Overlay,
    events: mpsc::Sender<BrowserContextEvent>,
    event_receiver: mpsc::Receiver<BrowserContextEvent>,
    lifecycle_sender: mpsc::UnboundedSender<LifecycleEvent>,
    lifecycle_receiver: mpsc::UnboundedReceiver<LifecycleEvent>,
    start_tasks: HashMap<SessionRecordId, JoinHandle<()>>,
    path_tasks: HashMap<Uuid, JoinHandle<()>>,
    close_tasks: HashMap<SessionRecordId, JoinHandle<()>>,
    transfer_selection: Option<TransferSelection>,
    transfer_task: Option<TransferTask>,
    status_message: Option<String>,
    status_error: Option<String>,
}

impl App {
    pub(crate) fn new() -> Self {
        let (events, event_receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (lifecycle_sender, lifecycle_receiver) = mpsc::unbounded_channel();
        Self {
            client: Client::new(),
            sessions: HashMap::new(),
            session_order: Vec::new(),
            label_counts: HashMap::new(),
            selected_session_id: None,
            contexts: HashMap::new(),
            context_order: Vec::new(),
            active_context_id: None,
            overlay: Overlay::SessionManager,
            events,
            event_receiver,
            lifecycle_sender,
            lifecycle_receiver,
            start_tasks: HashMap::new(),
            path_tasks: HashMap::new(),
            close_tasks: HashMap::new(),
            transfer_selection: None,
            transfer_task: None,
            status_message: None,
            status_error: None,
        }
    }

    pub(crate) async fn start(&mut self, path: Option<&str>) -> Result<(), Box<dyn Error>> {
        let Some(path) = path else {
            self.overlay = Overlay::SessionManager;
            return Ok(());
        };
        let record_id = self.insert_session_record(SessionTarget::Local, SessionStatus::Starting);
        let session = self.client.start_session(SessionTarget::Local).await?;
        if let Some(record) = self.sessions.get_mut(&record_id) {
            record.session = Some(Arc::clone(&session));
            record.status = SessionStatus::Ready;
        }
        let root_path = session.op_resolve_path(path).await?;
        let context_id = self.add_context(record_id, session, root_path);
        self.activate_context(context_id);
        self.overlay = Overlay::None;
        Ok(())
    }

    fn insert_session_record(
        &mut self,
        target: SessionTarget,
        status: SessionStatus,
    ) -> SessionRecordId {
        let base = match &target {
            SessionTarget::Local => "Local".to_owned(),
            SessionTarget::Remote { destination } => destination.clone(),
        };
        let count = self.label_counts.entry(base.clone()).or_default();
        *count += 1;
        let label = if *count == 1 {
            base
        } else {
            format!("{base}#{count}")
        };
        let id = SessionRecordId::new();
        self.sessions.insert(
            id,
            SessionRecord {
                id,
                target,
                label,
                status,
                session: None,
                last_active_context_id: None,
            },
        );
        self.session_order.push(id);
        if self.selected_session_id.is_none() {
            self.selected_session_id = Some(id);
        }
        id
    }

    fn add_context(
        &mut self,
        session_id: SessionRecordId,
        session: Arc<Session>,
        root_path: String,
    ) -> BrowserContextId {
        let context = BrowserContext::new(session, root_path, self.events.clone());
        let id = context.id();
        self.contexts.insert(
            id,
            BrowserTab {
                session_id,
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
        if let Some(session) = self.sessions.get_mut(&tab.session_id) {
            session.last_active_context_id = Some(id);
        }
    }

    pub(crate) fn pump_events(&mut self, limit: usize) {
        for _ in 0..limit {
            let Ok(event) = self.event_receiver.try_recv() else {
                break;
            };
            if let Some(tab) = self.contexts.get_mut(&event.context_id()) {
                tab.context.apply(event);
            }
        }
        while let Ok(event) = self.lifecycle_receiver.try_recv() {
            self.apply_lifecycle_event(event);
        }
    }

    fn apply_lifecycle_event(&mut self, event: LifecycleEvent) {
        match event {
            LifecycleEvent::SessionReady { record_id, session } => {
                self.start_tasks.remove(&record_id);
                let Some(record) = self.sessions.get_mut(&record_id) else {
                    return;
                };
                if !matches!(record.status, SessionStatus::Starting) {
                    return;
                }
                record.session = Some(session);
                record.status = SessionStatus::Ready;
            }
            LifecycleEvent::SessionFailed { record_id, error } => {
                self.start_tasks.remove(&record_id);
                if let Some(record) = self.sessions.get_mut(&record_id)
                    && matches!(record.status, SessionStatus::Starting)
                {
                    record.status = SessionStatus::Failed(error);
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
                        if dialog.session_id == record_id
                            && dialog.request_id == Some(request_id)
                );
                if !current {
                    return;
                }
                match result {
                    Ok(path) => {
                        let session = self
                            .sessions
                            .get(&record_id)
                            .and_then(|record| record.session.clone());
                        if let Some(session) = session {
                            let context_id = self.add_context(record_id, session, path);
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
            LifecycleEvent::SessionClosed { record_id, result } => {
                self.close_tasks.remove(&record_id);
                self.remove_session_record(record_id);
                if let Err(error) = result {
                    self.status_error = Some(format!("Error closing Session: {error}"));
                }
            }
            LifecycleEvent::TransferCompleted {
                transfer_id,
                request,
                conflict,
                result,
            } => self.apply_transfer_completed(transfer_id, request, conflict, result),
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
                    result.bytes_copied, result.destination_path
                ));
                if let Some(tab) = self.contexts.get_mut(&request.destination_context_id)
                    && tab.session_id == request.destination_session_id
                    && tab.context.root_path() == request.destination_root
                {
                    tab.context.refresh();
                }
            }
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    && conflict == CopyConflict::FailIfExists =>
            {
                self.status_message = Some(format!("{} already exists", request.source_name));
                self.overlay = Overlay::TransferOverwriteConfirmation(request);
            }
            Err(error) => {
                self.status_error = Some(format!("Error copying {}: {error}", request.source_name));
            }
        }
    }

    pub(crate) fn input_mode(&self) -> InputMode {
        if self
            .active_context()
            .is_some_and(|context| context.pending_open_path().is_some())
        {
            return InputMode::FileOpenConfirmation;
        }
        match self.overlay {
            Overlay::None => InputMode::Main,
            Overlay::SessionManager => InputMode::SessionManager,
            Overlay::TargetChooser { .. } => InputMode::TargetChooser,
            Overlay::RemoteInput { .. } => InputMode::RemoteInput,
            Overlay::NewBrowser(_) => InputMode::NewBrowser,
            Overlay::CloseConfirmation { .. } => InputMode::SessionCloseConfirmation,
            Overlay::TransferOverwriteConfirmation(_) => InputMode::TransferOverwriteConfirmation,
        }
    }

    pub(crate) fn open_session_manager(&mut self) {
        self.overlay = Overlay::SessionManager;
        self.repair_session_selection();
    }

    pub(crate) fn dismiss_overlay(&mut self) {
        self.overlay = Overlay::None;
    }

    pub(crate) fn open_add_session(&mut self) {
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
            self.overlay = Overlay::SessionManager;
            self.start_session(SessionTarget::Local);
        }
    }

    fn start_session(&mut self, target: SessionTarget) {
        let record_id = self.insert_session_record(target.clone(), SessionStatus::Starting);
        let client = Arc::clone(&self.client);
        let sender = self.lifecycle_sender.clone();
        self.start_tasks.insert(
            record_id,
            tokio::spawn(async move {
                match client.start_session(target).await {
                    Ok(session) => {
                        if sender
                            .send(LifecycleEvent::SessionReady {
                                record_id,
                                session: Arc::clone(&session),
                            })
                            .is_err()
                        {
                            let _ = client.close_session(&session.id()).await;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(LifecycleEvent::SessionFailed {
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
                self.overlay = Overlay::SessionManager;
                self.start_session(SessionTarget::Remote { destination });
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
                    ReturnMode::SessionManager => Overlay::SessionManager,
                };
            }
            Overlay::TargetChooser { .. } | Overlay::RemoteInput { .. } => {
                self.overlay = Overlay::SessionManager;
            }
            Overlay::CloseConfirmation { .. } => self.overlay = Overlay::SessionManager,
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
            session_id: tab.session_id,
            input: tab.context.root_path().to_owned(),
            request_id: None,
            error: None,
            return_mode: ReturnMode::Main,
        });
    }

    fn open_new_browser_for_session(
        &mut self,
        session_id: SessionRecordId,
        return_mode: ReturnMode,
    ) {
        let default_path = self
            .sessions
            .get(&session_id)
            .and_then(|record| record.last_active_context_id)
            .and_then(|id| self.contexts.get(&id))
            .map_or_else(|| "~".into(), |tab| tab.context.root_path().to_owned());
        self.overlay = Overlay::NewBrowser(NewBrowserDialog {
            session_id,
            input: default_path,
            request_id: None,
            error: None,
            return_mode,
        });
    }

    pub(crate) fn open_new_browser_for_selected_session(&mut self) {
        let Some(session_id) = self.selected_session_id else {
            return;
        };
        if self
            .sessions
            .get(&session_id)
            .is_some_and(|record| matches!(record.status, SessionStatus::Ready))
        {
            self.open_new_browser_for_session(session_id, ReturnMode::SessionManager);
        }
    }

    fn submit_new_browser(&mut self) {
        let Overlay::NewBrowser(dialog) = &mut self.overlay else {
            return;
        };
        let Some(session) = self
            .sessions
            .get(&dialog.session_id)
            .and_then(|record| record.session.clone())
        else {
            dialog.error = Some("Session is not ready".into());
            return;
        };
        let request_id = Uuid::new_v4();
        dialog.request_id = Some(request_id);
        dialog.error = None;
        let record_id = dialog.session_id;
        let path = dialog.input.clone();
        let sender = self.lifecycle_sender.clone();
        self.path_tasks.insert(
            request_id,
            tokio::spawn(async move {
                let result = session
                    .op_resolve_path(&path)
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

    pub(crate) fn move_session_selection(&mut self, down: bool) {
        if self.session_order.is_empty() {
            self.selected_session_id = None;
            return;
        }
        let index = self
            .selected_session_id
            .and_then(|id| {
                self.session_order
                    .iter()
                    .position(|candidate| *candidate == id)
            })
            .unwrap_or(0);
        let next = if down {
            (index + 1).min(self.session_order.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.selected_session_id = Some(self.session_order[next]);
    }

    pub(crate) fn activate_selected_session(&mut self) {
        let Some(session_id) = self.selected_session_id else {
            return;
        };
        let Some(record) = self.sessions.get(&session_id) else {
            return;
        };
        if !matches!(record.status, SessionStatus::Ready) {
            return;
        }
        if let Some(context_id) = record.last_active_context_id {
            self.activate_context(context_id);
            self.overlay = Overlay::None;
        } else {
            self.open_new_browser_for_session(session_id, ReturnMode::SessionManager);
        }
    }

    pub(crate) fn delete_selected_session(&mut self) {
        let Some(session_id) = self.selected_session_id else {
            return;
        };
        let Some(record) = self.sessions.get(&session_id) else {
            return;
        };
        match record.status {
            SessionStatus::Failed(_) if record.session.is_none() => {
                self.remove_session_record(session_id);
            }
            SessionStatus::Ready => {
                self.overlay = Overlay::CloseConfirmation { session_id };
            }
            _ => {}
        }
    }

    pub(crate) fn confirm_session_close(&mut self) {
        let Overlay::CloseConfirmation { session_id } = self.overlay else {
            return;
        };
        let session = {
            let Some(record) = self.sessions.get_mut(&session_id) else {
                self.overlay = Overlay::SessionManager;
                return;
            };
            record.status = SessionStatus::Closing;
            record.session.take()
        };
        self.remove_contexts_for_session(session_id);
        self.clear_transfer_state_for_session(session_id);
        self.overlay = Overlay::SessionManager;
        let Some(session) = session else {
            self.remove_session_record(session_id);
            return;
        };
        let client = Arc::clone(&self.client);
        let sender = self.lifecycle_sender.clone();
        let transfer = self.take_transfer_for_session(session_id);
        self.close_tasks.insert(
            session_id,
            tokio::spawn(async move {
                if let Some(task) = transfer {
                    task.handle.abort();
                    let _ = task.handle.await;
                }
                let result = client.close_session(&session.id()).await;
                let _ = sender.send(LifecycleEvent::SessionClosed {
                    record_id: session_id,
                    result,
                });
            }),
        );
    }

    fn remove_session_record(&mut self, id: SessionRecordId) {
        self.clear_transfer_state_for_session(id);
        let old_index = self
            .session_order
            .iter()
            .position(|candidate| *candidate == id);
        self.sessions.remove(&id);
        self.session_order.retain(|candidate| *candidate != id);
        if self.selected_session_id == Some(id) {
            self.selected_session_id = old_index.and_then(|index| {
                self.session_order
                    .get(index.min(self.session_order.len().saturating_sub(1)))
                    .copied()
            });
        }
        self.repair_session_selection();
    }

    fn repair_session_selection(&mut self) {
        if self
            .selected_session_id
            .is_some_and(|id| self.sessions.contains_key(&id))
        {
            return;
        }
        self.selected_session_id = self.session_order.first().copied();
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
        self.repair_last_active(tab.session_id);
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

    fn remove_contexts_for_session(&mut self, session_id: SessionRecordId) {
        let ids: Vec<_> = self
            .context_order
            .iter()
            .copied()
            .filter(|id| {
                self.contexts
                    .get(id)
                    .is_some_and(|tab| tab.session_id == session_id)
            })
            .collect();
        for id in ids {
            self.remove_context(id);
        }
    }

    fn repair_last_active(&mut self, session_id: SessionRecordId) {
        let replacement = self.context_order.iter().rev().copied().find(|id| {
            self.contexts
                .get(id)
                .is_some_and(|tab| tab.session_id == session_id)
        });
        if let Some(record) = self.sessions.get_mut(&session_id) {
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
        let session_id = tab.session_id;
        let Some(record) = self.sessions.get(&session_id) else {
            self.status_message = Some("Source Session is unavailable".into());
            return;
        };
        if !matches!(record.status, SessionStatus::Ready) || record.session.is_none() {
            self.status_message = Some("Source Session is not ready".into());
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
                selection.name
            ));
            return;
        }
        let label = record.label.clone();
        self.transfer_selection = Some(TransferSelection {
            session_id,
            path: selection.path,
            name: selection.name.clone(),
        });
        self.status_error = None;
        self.status_message = Some(format!("Yanked {} from {label}", selection.name));
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
            source_session_id: selection.session_id,
            source_path: selection.path,
            source_name: selection.name,
            destination_session_id: tab.session_id,
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
            .sessions
            .get(&request.source_session_id)
            .filter(|record| matches!(record.status, SessionStatus::Ready))
            .and_then(|record| record.session.clone());
        let destination = self
            .sessions
            .get(&request.destination_session_id)
            .filter(|record| matches!(record.status, SessionStatus::Ready))
            .and_then(|record| record.session.clone());
        let (Some(source), Some(destination)) = (source, destination) else {
            self.status_message = Some("Source or destination Session is not ready".into());
            return;
        };
        let transfer_id = Uuid::new_v4();
        let sender = self.lifecycle_sender.clone();
        let event_request = request.clone();
        let source_path = request.source_path.clone();
        let destination_root = request.destination_root.clone();
        let source_name = request.source_name.clone();
        let handle = tokio::spawn(async move {
            let result = destination
                .op_copy_file_from(
                    &source,
                    &source_path,
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
        self.status_message = Some(format!("Copying {}…", request.source_name));
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

    fn clear_transfer_state_for_session(&mut self, session_id: SessionRecordId) {
        if self
            .transfer_selection
            .as_ref()
            .is_some_and(|selection| selection.session_id == session_id)
        {
            self.transfer_selection = None;
        }
        if matches!(
            &self.overlay,
            Overlay::TransferOverwriteConfirmation(request)
                if request.source_session_id == session_id
                    || request.destination_session_id == session_id
        ) {
            self.overlay = Overlay::None;
        }
    }

    fn take_transfer_for_session(&mut self, session_id: SessionRecordId) -> Option<TransferTask> {
        let uses_session = self.transfer_task.as_ref().is_some_and(|task| {
            task.request.source_session_id == session_id
                || task.request.destination_session_id == session_id
        });
        uses_session.then(|| self.transfer_task.take()).flatten()
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

    pub(crate) fn activate_active_selection(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.activate_selected();
        }
    }

    pub(crate) fn confirm_active_open(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.confirm_open();
        }
    }

    pub(crate) fn cancel_active_open_confirmation(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.cancel_open_confirmation();
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

    fn active_context(&self) -> Option<&BrowserContext> {
        self.active_context_id
            .and_then(|id| self.contexts.get(&id))
            .map(|tab| &tab.context)
    }

    fn active_context_mut(&mut self) -> Option<&mut BrowserContext> {
        self.active_context_id
            .and_then(|id| self.contexts.get_mut(&id))
            .map(|tab| &mut tab.context)
    }

    pub(crate) async fn shutdown(&mut self) -> Result<(), SessionShutdownError> {
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

    use crate::SessionTarget;

    use super::{App, InputMode, SessionStatus};
    use crate::tui::browser_context::ListingStatus;

    #[test]
    fn duplicate_labels_are_stable_and_ordered() {
        let mut app = App::new();
        let first = app.insert_session_record(SessionTarget::Local, SessionStatus::Starting);
        let second = app.insert_session_record(SessionTarget::Local, SessionStatus::Starting);
        assert_eq!(app.sessions[&first].label, "Local");
        assert_eq!(app.sessions[&second].label, "Local#2");
        app.remove_session_record(first);
        assert_eq!(app.sessions[&second].label, "Local#2");
    }

    #[tokio::test]
    async fn starting_without_a_path_opens_session_manager() {
        let mut app = App::new();
        app.start(None).await.unwrap();
        assert_eq!(app.input_mode(), InputMode::SessionManager);
        assert!(app.sessions.is_empty());
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
    async fn switching_wraps_and_closing_a_context_leaves_its_session() {
        let mut app = App::new();
        app.start(Some(".")).await.unwrap();
        let session_id = app.session_order[0];
        let session = app.sessions[&session_id].session.clone().unwrap();
        let second = app.add_context(session_id, session, "/".into());
        app.activate_context(second);
        let first = app.context_order[0];
        app.switch_context(true);
        assert_eq!(app.active_context_id, Some(first));
        app.switch_context(false);
        assert_eq!(app.active_context_id, Some(second));
        app.close_active_context();
        assert_eq!(app.active_context_id, Some(first));
        assert!(app.sessions.contains_key(&session_id));
        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_without_a_session_is_harmless() {
        App::new().shutdown().await.unwrap();
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
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                app.pump_events(super::EVENT_CHANNEL_CAPACITY);
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

        app.yank_active_selection();
        assert_eq!(
            app.transfer_selection
                .as_ref()
                .map(|selection| selection.name.as_str()),
            Some("payload.txt")
        );
        let source_context_id = app.active_context_id.unwrap();
        let session_id = app.contexts[&source_context_id].session_id;
        let session = app.sessions[&session_id].session.clone().unwrap();
        let destination_context_id = app.add_context(
            session_id,
            session,
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

    async fn wait_for_transfer(app: &mut App) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                app.pump_events(super::EVENT_CHANNEL_CAPACITY);
                if app.transfer_task.is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        app.pump_events(super::EVENT_CHANNEL_CAPACITY);
    }
}
