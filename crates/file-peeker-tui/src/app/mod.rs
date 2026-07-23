mod view;

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    io,
    sync::Arc,
};

use file_peeker_client::{Client, ClientCloseSessionError, Session, SessionTarget};
use tokio::sync::mpsc;

use crate::browser_context::{BrowserContext, BrowserContextEvent, BrowserContextId};

#[derive(Debug)]
pub(crate) struct App {
    client: Arc<Client>,
    help: String,
    session_ids: HashSet<String>,
    contexts: HashMap<BrowserContextId, BrowserContext>,
    active_context_id: Option<BrowserContextId>,
    events: mpsc::Sender<BrowserContextEvent>,
}

impl App {
    pub(crate) fn new(events: mpsc::Sender<BrowserContextEvent>, help: String) -> Self {
        Self {
            client: Client::new(),
            help,
            session_ids: HashSet::new(),
            contexts: HashMap::new(),
            active_context_id: None,
            events,
        }
    }

    pub(crate) async fn start(&mut self, path: Option<&str>) -> Result<(), Box<dyn Error>> {
        let Some(path) = path else {
            return Ok(());
        };
        let session_id = self.client.start_session(SessionTarget::Local).await?;
        self.session_ids.insert(session_id.clone());
        let session = self
            .client
            .get_session(session_id.clone())
            .await
            .ok_or_else(|| io::Error::other("started Session was not retained"))?;
        let root_path = session.op_resolve_path(path).await?;
        let context_id = self.add_context(session, root_path);
        self.active_context_id = Some(context_id);
        Ok(())
    }

    fn add_context(&mut self, session: Arc<Session>, root_path: String) -> BrowserContextId {
        let context = BrowserContext::new(session, root_path, self.events.clone());
        let id = context.id();
        self.contexts.insert(id, context);
        id
    }

    pub(crate) fn update(&mut self, event: BrowserContextEvent) {
        if let Some(context) = self.contexts.get_mut(&event.context_id()) {
            context.apply(event);
        }
    }

    pub(crate) fn refresh_active_context(&mut self) {
        if let Some(context) = self.active_context_mut() {
            context.refresh();
        }
    }

    pub(crate) fn move_active_selection(&mut self, down: bool) {
        if let Some(context) = self.active_context_mut() {
            context.move_selection(down);
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
        self.active_context_id.and_then(|id| self.contexts.get(&id))
    }

    fn active_context_mut(&mut self) -> Option<&mut BrowserContext> {
        self.active_context_id
            .and_then(|id| self.contexts.get_mut(&id))
    }

    fn cancel_all_listings(&mut self) {
        for context in self.contexts.values_mut() {
            context.cancel();
        }
    }

    pub(crate) async fn shutdown(&mut self) -> Result<(), ClientCloseSessionError> {
        self.cancel_all_listings();
        let session_ids: Vec<_> = self.session_ids.drain().collect();
        let mut first_error = None;
        for session_id in session_ids {
            if let Err(error) = self.client.close_session(session_id).await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use super::App;
    use crate::EVENT_CHANNEL_CAPACITY;

    fn app() -> App {
        let (events, _receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        App::new(events, "test help".into())
    }

    #[test]
    fn new_app_owns_an_empty_client() {
        let app = app();
        assert_eq!(Arc::strong_count(&app.client), 1);
        assert!(app.session_ids.is_empty());
        assert!(app.contexts.is_empty());
        assert_eq!(app.active_context_id, None);
    }

    #[tokio::test]
    async fn starting_without_a_path_keeps_the_app_sessionless() {
        let mut app = app();
        app.start(None).await.unwrap();

        assert!(app.session_ids.is_empty());
        assert!(app.contexts.is_empty());
        assert_eq!(app.active_context_id, None);
    }

    #[tokio::test]
    async fn resolves_startup_path_before_creating_browser_context() {
        let mut app = app();
        app.start(Some(".")).await.unwrap();

        assert_eq!(
            app.active_context().unwrap().root_path(),
            std::env::current_dir().unwrap().to_str().unwrap()
        );

        app.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_without_a_session_is_harmless() {
        app().shutdown().await.unwrap();
    }
}
