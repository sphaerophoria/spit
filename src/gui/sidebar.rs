use crate::{
    app::{RepoState, ViewState},
    git::ReferenceId,
    gui::{reference_richtext, tristate_checkbox::TristateCheckbox, try_set_clipboard},
};

use clipboard::ClipboardContext;
use eframe::egui::{ScrollArea, TextEdit, Ui, Widget};

use std::{collections::BTreeSet, sync::Arc};

pub(super) enum SidebarAction {
    Checkout(ReferenceId),
    Delete(ReferenceId),
    None,
}

#[derive(Default)]
pub(super) struct Sidebar {
    repo_state: Arc<RepoState>,
    filter_text: String,
    filtered_refs: BTreeSet<ReferenceId>,
}

impl Sidebar {
    pub(super) fn new() -> Sidebar {
        Default::default()
    }

    pub(super) fn update_with_repo_state(&mut self, repo_state: Arc<RepoState>) {
        self.repo_state = repo_state;
        self.update_filters();
    }

    pub(super) fn update_filters(&mut self) {
        self.filtered_refs = self
            .repo_state
            .branches
            .iter()
            .filter_map(|x| {
                if x.id.to_string().contains(&self.filter_text) {
                    Some(x.id.clone())
                } else {
                    None
                }
            })
            .collect();
    }

    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        view_state: &ViewState,
        pending_view_state: &mut ViewState,
        clipboard: &mut ClipboardContext,
    ) -> SidebarAction {
        if TextEdit::singleline(&mut self.filter_text)
            .desired_width(ui.available_width())
            .hint_text("Branch filter")
            .show(ui)
            .response
            .changed()
        {
            self.update_filters()
        }

        let mut action = SidebarAction::None;

        ui.horizontal(|ui| {
            if ui.button("All").clicked() {
                pending_view_state
                    .selected_references
                    .extend(self.filtered_refs.iter().cloned());
            }

            if ui.button("None").clicked() {
                pending_view_state
                    .selected_references
                    .retain(|id| !self.filtered_refs.contains(id));
            }
        });

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for id in self.filtered_refs.iter() {
                    let real_state = view_state.selected_references.contains(id);
                    let mut selected = pending_view_state.selected_references.contains(id);

                    let text = reference_richtext(id, &self.repo_state);

                    let response = TristateCheckbox::new(&real_state, &mut selected, text).ui(ui);
                    if response.clicked() {
                        if selected {
                            pending_view_state.selected_references.insert(id.clone());
                        } else {
                            pending_view_state.selected_references.remove(id);
                        }
                    }
                    response.context_menu(|ui| {
                        if ui.button("Copy").clicked() {
                            try_set_clipboard(clipboard, id.to_string());
                            ui.close_menu();
                        }

                        if ui.button("Checkout").clicked() {
                            action = SidebarAction::Checkout(id.clone());
                            ui.close_menu();
                        }

                        ui.separator();

                        if ui.button("Delete").clicked() {
                            action = SidebarAction::Delete(id.clone());
                            ui.close_menu();
                        }
                    });
                }
            });

        action
    }
}
