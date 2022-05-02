use anyhow::Result;
use spit::gui::Editor;

fn main() -> Result<()> {
    env_logger::init();

    let filename = match std::env::args().nth(1) {
        Some(f) => f,
        None => std::process::exit(1),
    };

    eframe::run_native(
        "Spit editor",
        Default::default(),
        Box::new(move |cc| Box::new(Editor::new(&filename, cc).unwrap())),
    );
}
