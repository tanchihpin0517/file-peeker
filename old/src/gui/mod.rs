//! The `file-peeker-gui` binary: an egui/eframe front end over
//! [`Client`](crate::Client) and its connections.
//!
//! It starts on one browser rooted at the local user's home directory. Extra
//! browsers arrive by dragging a tab, which `egui_dock` handles, so the layout
//! is the user's rather than something this module lays out.

mod app;
mod dock;
mod glyph;
mod theme;

use std::{error::Error, sync::Arc};

use clap::Parser;

use crate::{Client, ServerLocation, host_path::HostPath};

#[derive(Debug, Eq, Parser, PartialEq)]
#[command(
    name = "file-peeker-gui",
    version,
    about = "Browse a directory with File Peeker"
)]
struct Cli {
    /// Directory to browse. Defaults to the local user's home directory.
    path: Option<String>,
}

/// Runs the `file-peeker-gui` desktop front end.
///
/// # Errors
///
/// Returns an error if the local connection fails to open, the starting path
/// cannot be resolved, or the window cannot be created.
///
/// # Panics
///
/// Panics if the Tokio runtime cannot be built.
pub fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    // The UI thread is synchronous, so the runtime is entered around it rather
    // than wrapping it: `tokio::spawn` from a frame then lands on a worker, and
    // the filesystem walk runs in parallel with the rendering.
    let runtime = tokio::runtime::Runtime::new().expect("a Tokio runtime should start");
    let _guard = runtime.enter();

    let client = Client::new();
    let (connection, root) = runtime.block_on(async {
        let connection = client.open(ServerLocation::Embedded).await?;
        // The server resolves the path, which is what expands `~` and makes the
        // default the *server's* idea of home rather than this process's.
        // Argv is text a person typed, so it is escaped on its way to the host.
        let root = client
            .op_resolve_path(
                &connection,
                &HostPath::from_literal(cli.path.as_deref().unwrap_or("~")),
            )
            .await?;
        Ok::<_, std::io::Error>((connection, root))
    })?;

    let viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([720.0, 480.0])
        .with_title("File Peeker")
        // The unified top bar: the OS keeps the window buttons, the app draws
        // everything else, so the toolbar and the title share one surface.
        .with_fullsize_content_view(true)
        .with_titlebar_shown(false)
        .with_title_shown(false)
        .with_titlebar_buttons_shown(false);

    let ui_client = Arc::clone(&client);
    let result = eframe::run_native(
        "File Peeker",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(move |cx| {
            theme::install_fonts(&cx.egui_ctx);
            Ok(Box::new(app::App::new(ui_client, connection, root)))
        }),
    );

    // The window is gone; the server it was talking to should follow before the
    // runtime is dropped out from under it.
    let shutdown = runtime.block_on(client.close_all());
    result?;
    shutdown.map_err(Into::into)
}
