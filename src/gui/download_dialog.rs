use eframe::egui::{self, ScrollArea, TextEdit, Window};

use crate::{app::RemoteState, git::RemoteRef};

pub(crate) struct DownloadDialog {
    open: bool,
    remote_state: RemoteState,
    filter_text: String,
    filtered_remote_refs: Vec<RemoteRef>,
}

impl DownloadDialog {
    pub(crate) fn new() -> DownloadDialog {
        DownloadDialog {
            open: false,
            remote_state: Default::default(),
            filter_text: Default::default(),
            filtered_remote_refs: Default::default(),
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = DownloadDialog::new();
    }

    pub(crate) fn update_remote_state(&mut self, remote_state: RemoteState) {
        self.remote_state = remote_state;

        self.update_filters();
    }

    pub(crate) fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Option<RemoteRef> {
        if !self.open {
            return None;
        }

        let mut ret = None;

        let mut next_open = self.open;

        Window::new("Download references")
            .collapsible(false)
            .open(&mut next_open)
            .show(ctx, |ui| {
                if TextEdit::singleline(&mut self.filter_text)
                    .desired_width(ui.available_width())
                    .hint_text("Filter")
                    .show(ui)
                    .response
                    .changed()
                {
                    self.update_filters()
                }

                let row_height = ui.spacing().interact_size.y;

                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(
                        ui,
                        row_height,
                        self.filtered_remote_refs.len(),
                        |ui, row_range| {
                            for r in &self.filtered_remote_refs[row_range] {
                                ui.horizontal(|ui| {
                                    ui.label(ref_to_display_string(r));
                                    if ui.button("Download").clicked() {
                                        ret = Some(r.clone());
                                    }
                                });
                            }
                        },
                    );
            });

        self.open = next_open;

        ret
    }

    fn update_filters(&mut self) {
        self.filtered_remote_refs = self
            .remote_state
            .references
            .iter()
            .filter_map(
                |x| match ref_to_display_string(x).contains(&self.filter_text) {
                    true => Some(x.clone()),
                    false => None,
                },
            )
            .collect();
    }
}

fn ref_to_display_string(r: &RemoteRef) -> String {
    format!("{}/{}", r.remote, r.ref_name)
}
