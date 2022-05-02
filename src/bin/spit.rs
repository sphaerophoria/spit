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

    let native_options = eframe::NativeOptions {
        maximized: true,
        ..Default::default()
    };

    std::thread::spawn({
        let app_request_tx = app_request_tx.clone();
        move || {
            let mut app = App::new(app_response_tx, app_request_tx, app_request_rx)
                .expect("Failed to iniitialize app");
            app.run();
        }
    });

    eframe::run_native(
        "Spit",
        native_options,
        Box::new(move |cc| Box::new(Gui::new(app_request_tx, app_response_rx, cc).unwrap())),
    );
}
