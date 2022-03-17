use personal_git_gui::{app::App, gui::Gui};

use std::sync::mpsc::{self};

fn main() {
    env_logger::init();
    let (app_tx, gui_rx) = mpsc::channel();
    let (gui_tx, app_rx) = mpsc::channel();

    let gui = Gui::new(gui_tx, gui_rx);
    let native_options = eframe::NativeOptions::default();

    std::thread::spawn(move || {
        let mut app = App::new(app_tx, app_rx);
        app.run();
    });

    eframe::run_native(Box::new(gui), native_options);
}
