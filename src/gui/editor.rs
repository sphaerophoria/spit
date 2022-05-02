use anyhow::Result;
use eframe::{
    egui::{self, CentralPanel, Layout, TextEdit, TextStyle, TopBottomPanel, Visuals},
    App, CreationContext,
};

use log::error;

use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub struct Editor {
    filename: PathBuf,
    content: String,
    should_save: bool,
}

impl Editor {
    pub fn new(filename: &str, cc: &CreationContext<'_>) -> Result<Editor> {
        let filename: PathBuf = filename.try_into()?;
        let content = load_content(&filename)?;
        let should_save = false;

        cc.egui_ctx.set_visuals(Visuals::dark());

        Ok(Editor {
            filename,
            content,
            should_save,
        })
    }
}

impl App for Editor {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let mut reload = false;

        TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Reload").clicked() {
                    reload = true;
                }
            });
        });

        TopBottomPanel::bottom("dialog").show(ctx, |ui| {
            ui.with_layout(Layout::right_to_left(), |ui| {
                if ui.button("Finish").clicked() {
                    self.should_save = true;
                    frame.quit();
                }

                if ui.button("Cancel").clicked() {
                    std::process::exit(1);
                }
            });
        });

        CentralPanel::default().show(ctx, |ui| {
            let text_height = ui.text_style_height(&TextStyle::Monospace);
            TextEdit::multiline(&mut self.content)
                .code_editor()
                .desired_width(f32::INFINITY)
                .desired_rows((ui.available_height() / text_height) as usize)
                .lock_focus(true)
                .show(ui);
        });

        if reload {
            match load_content(&self.filename) {
                Ok(v) => {
                    self.content = v;
                }
                Err(_e) => {
                    error!("Failed to reload file");
                }
            }
        }
    }

    fn on_exit(&mut self, _gl: &eframe::glow::Context) {
        if self.should_save {
            let mut f = match OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&self.filename)
            {
                Ok(v) => v,
                Err(_e) => {
                    error!("Failed to open file to save");
                    std::process::exit(1);
                }
            };

            if let Err(_e) = write!(f, "{}", &self.content) {
                error!("Failed to save");
            }
        } else {
            std::process::exit(1);
        }
    }
}

fn load_content(filename: &Path) -> Result<String> {
    let mut content = String::new();
    File::open(filename)?.read_to_string(&mut content)?;

    Ok(content)
}
