use anyhow::Result;
use spit::gui::Editor;

fn main() -> Result<()> {
    env_logger::init();

    let filename = match std::env::args().nth(1) {
        Some(f) => f,
        None => std::process::exit(1),
    };

    let gui = Editor::new(&filename)?;
    eframe::run_native(Box::new(gui), Default::default());
}
