use anyhow::Result;
use spit::{app::App, gui::Gui};

use std::{env, path::PathBuf, sync::mpsc};

fn main() -> Result<()> {
    env_logger::init();
    let (app_response_tx, app_response_rx) = mpsc::channel();
    let (app_request_tx, app_request_rx) = mpsc::channel();

    if let Some(repo) = env::args().nth(1) {
        app_request_tx
            .send(spit::app::AppRequest::OpenRepo(PathBuf::from(repo)))
            .expect("Gui TX did not initialize correctly");
    };

    let gui = Gui::new(app_request_tx.clone(), app_response_rx)?;
    let native_options = eframe::NativeOptions::default();

    std::thread::spawn(move || {
        let mut app = App::new(app_response_tx, app_request_tx, app_request_rx)
            .expect("Failed to iniitialize app");
        app.run();
    });

    eframe::run_native(Box::new(gui), native_options);
}
