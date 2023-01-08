use anyhow::Result;
use eframe::{
    egui::{
        self, text::LayoutJob, CentralPanel, Color32, ComboBox, Align, FontId, Galley, Layout, ScrollArea,
        TextEdit, TextFormat, TextStyle, TopBottomPanel, Ui, Visuals,
    },
    App, CreationContext,
};

use log::error;

use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(PartialEq, Eq)]
enum EditorType {
    CommitEdit,
    Unknown,
}

impl fmt::Display for EditorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditorType::CommitEdit => write!(f, "Commit Edit"),
            EditorType::Unknown => write!(f, "Unknown"),
        }
    }
}

pub struct Editor {
    filename: PathBuf,
    editor_type: EditorType,
    content: String,
    should_save: bool,
}

impl Editor {
    pub fn new(filename: &str, cc: &CreationContext<'_>) -> Result<Editor> {
        let filename: PathBuf = filename.try_into()?;
        let content = load_content(&filename)?;
        let should_save = false;
        let editor_type = detect_type(&filename);

        cc.egui_ctx.set_visuals(Visuals::dark());

        Ok(Editor {
            filename,
            editor_type,
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

                ComboBox::from_label("Editor Type")
                    .selected_text(&self.editor_type.to_string())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.editor_type,
                            EditorType::CommitEdit,
                            EditorType::CommitEdit.to_string(),
                        );
                        ui.selectable_value(
                            &mut self.editor_type,
                            EditorType::Unknown,
                            EditorType::Unknown.to_string(),
                        );
                    });
            });
        });

        TopBottomPanel::bottom("dialog").show(ctx, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Finish").clicked() {
                    self.should_save = true;
                    frame.close();
                }

                if ui.button("Cancel").clicked() {
                    frame.close();
                }
            });
        });

        CentralPanel::default().show(ctx, |ui| {
            let text_height = ui.text_style_height(&TextStyle::Monospace);
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    TextEdit::multiline(&mut self.content)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows((ui.available_height() / text_height) as usize)
                        .lock_focus(true)
                        .layouter(&mut |ui, s, wrap_width| {
                            highlight(ui, s, wrap_width, &self.editor_type)
                        })
                        .show(ui);
                });
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

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
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

fn detect_type(filename: &Path) -> EditorType {
    match filename.file_name().and_then(|s| s.to_str()) {
        Some("COMMIT_EDITMSG") => EditorType::CommitEdit,
        _ => EditorType::Unknown,
    }
}

fn default_fontid(ui: &Ui) -> FontId {
    ui.style().text_styles[&TextStyle::Monospace].clone()
}

fn default_textformat(ui: &Ui) -> TextFormat {
    TextFormat::simple(default_fontid(ui), ui.style().visuals.text_color())
}

fn layout_commit_message_line(
    job: &mut LayoutJob,
    line: &str,
    max_len: usize,
    good: &TextFormat,
    bad: &TextFormat,
) {
    let line_split_idx = max_len.min(line.len());
    job.append(&line[..line_split_idx], 0.0, good.clone());
    if line.len() > max_len {
        job.append(&line[max_len..], 0.0, bad.clone());
    }
}

fn commit_message_layout_job(ui: &Ui, s: &str) -> LayoutJob {
    let textformat = default_textformat(ui);
    let mut bad_textformat = textformat.clone();
    bad_textformat.color = Color32::LIGHT_RED;

    let mut lines = s.split_inclusive('\n');
    let mut job = LayoutJob::default();

    if let Some(first_line) = lines.next() {
        layout_commit_message_line(&mut job, first_line, 50, &textformat, &bad_textformat);
    }

    if let Some(second_line) = lines.next() {
        layout_commit_message_line(&mut job, second_line, 0, &textformat, &bad_textformat);
    }

    for line in lines {
        layout_commit_message_line(&mut job, line, 72, &textformat, &bad_textformat);
    }

    job
}

fn highlight(ui: &Ui, s: &str, wrap_width: f32, editor_type: &EditorType) -> Arc<Galley> {
    let mut layout_job = match editor_type {
        EditorType::CommitEdit => commit_message_layout_job(ui, s),
        EditorType::Unknown => LayoutJob::single_section(s.to_string(), default_textformat(ui)),
    };
    layout_job.wrap.max_width = wrap_width;
    ui.fonts().layout_job(layout_job)
}
