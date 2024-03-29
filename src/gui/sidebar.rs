use crate::{
    app::{RemoteState, RepoState, ViewState},
    git::{Reference, ReferenceId, SortType},
    gui::{reference_richtext, tristate_checkbox::TristateCheckbox, try_set_clipboard},
};

use clipboard::ClipboardContext;
use eframe::egui::{ComboBox, ScrollArea, TextEdit, Ui, Widget};

use std::{collections::BTreeSet, sync::Arc};

pub(super) enum SidebarAction {
    Checkout(ReferenceId),
    Delete(ReferenceId),
    None,
}

#[derive(Default)]
pub(super) struct Sidebar {
    repo_state: Arc<RepoState>,
    remote_state: RemoteState,
    filter_text: String,
    filtered_refs: BTreeSet<ReferenceId>,
}

impl Sidebar {
    pub(super) fn new() -> Sidebar {
        Default::default()
    }

    pub(super) fn update_with_repo_state(&mut self, repo_state: Arc<RepoState>) {
        self.repo_state = repo_state;
        if self.repo_state.repo != self.remote_state.repo {
            self.remote_state = Default::default();
        }
        self.update_filters();
    }

    pub(super) fn update_filters(&mut self) {
        self.filtered_refs =
            filter_references(&self.filter_text, &self.repo_state.references).collect();
    }

    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        view_state: &ViewState,
        pending_view_state: &mut ViewState,
        clipboard: &mut ClipboardContext,
    ) -> SidebarAction {
        ComboBox::from_label("Sort Type")
            .selected_text(sort_type_label(&pending_view_state.sort_type))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut pending_view_state.sort_type,
                    SortType::AuthorTimestamp,
                    sort_type_label(&SortType::AuthorTimestamp),
                );
                ui.selectable_value(
                    &mut pending_view_state.sort_type,
                    SortType::CommitterTimestamp,
                    sort_type_label(&SortType::CommitterTimestamp),
                );
            });

        ui.separator();

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

            if ui.button("Clear filter").clicked() {
                self.filter_text = String::new();
                self.update_filters()
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

fn filter_references<'a>(
    filter: &'a str,
    references: &'a [Reference],
) -> impl Iterator<Item = ReferenceId> + 'a {
    references.iter().filter_map(move |x| {
        if x.id.to_string().contains(filter) {
            Some(x.id.clone())
        } else {
            None
        }
    })
}

fn sort_type_label(sort_type: &SortType) -> &str {
    match sort_type {
        SortType::CommitterTimestamp => "Committer",
        SortType::AuthorTimestamp => "Author",
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_branch_filtering() {
        let branches = [
            Reference {
                id: ReferenceId::Symbolic("HEAD".into()),
                head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap(),
            },
            Reference {
                id: ReferenceId::LocalBranch("local_branch".into()),
                head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap(),
            },
            Reference {
                id: ReferenceId::RemoteBranch("origin/remote_branch".into()),
                head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap(),
            },
        ];

        assert_eq!(filter_references("test", &branches).next(), None);
        assert_eq!(
            filter_references("HE", &branches).collect::<Vec<_>>(),
            vec![ReferenceId::Symbolic("HEAD".into())]
        );
        assert_eq!(
            filter_references("_", &branches).collect::<Vec<_>>(),
            vec![
                ReferenceId::LocalBranch("local_branch".into()),
                ReferenceId::RemoteBranch("origin/remote_branch".into())
            ]
        );
        assert_eq!(
            filter_references("llocal_branch", &branches).collect::<Vec<_>>(),
            vec![]
        );
    }
}
