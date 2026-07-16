use ratatui::{Frame, widgets::Paragraph};

#[derive(Debug, Default)]
struct App {
    status: String,
}

impl App {
    fn render(&self, frame: &mut Frame<'_>) {
        frame.render_widget(Paragraph::new(self.status.as_str()), frame.area());
    }
}

fn main() {
    let _start_path = std::env::args_os().nth(1);
    println!("File Peeker v1 skeleton");
}

#[allow(dead_code)]
fn render_skeleton(frame: &mut Frame<'_>) {
    App {
        status: "File Peeker v1 skeleton".into(),
    }
    .render(frame);
}
