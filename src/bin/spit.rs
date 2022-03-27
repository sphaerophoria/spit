use spit::{app::App, gui::Gui};

use std::{env, path::PathBuf, sync::mpsc};

fn main() {
    env_logger::init();
    let (app_tx, gui_rx) = mpsc::channel();
    let (gui_tx, app_rx) = mpsc::channel();

    if let Some(repo) = env::args().nth(1) {
        gui_tx
            .send(spit::app::AppRequest::OpenRepo(PathBuf::from(repo)))
            .expect("Gui TX did not initialize correctly");
    };

    let gui = Gui::new(gui_tx, gui_rx);
    let native_options = eframe::NativeOptions::default();

    std::thread::spawn(move || {
        let mut app = App::new(app_tx, app_rx);
        app.run();
    });

    eframe::run_native(Box::new(gui), native_options);
}
