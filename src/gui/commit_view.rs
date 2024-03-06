use crate::{
    app::RepoState,
    git::{Commit, Diff, DiffTarget, ObjectId},
    util::Cache,
};

use eframe::egui::{ScrollArea, TextEdit, TextStyle, Ui, Widget};

use spiff::widget::{self as spiff_widget, search_bar_wrapped, SearchBar, SearchBarAction};
use spiff::DiffOptions;

use super::commit_log::SelectedItem;

use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessedDiffOffset {
    file_index: usize,
    string_index: usize,
}

#[derive(PartialEq, Eq, Clone)]
pub(super) struct DiffRequest {
    pub(super) options: DiffOptions,
    pub(super) from: DiffTarget,
    pub(super) to: DiffTarget,
    pub(super) search_query: String,
}

pub(super) enum CommitViewAction {
    RequestDiff(DiffRequest),
    None,
}

#[derive(Default)]
pub(super) struct CommitView {
    repo_state: Arc<RepoState>,
    index_has_changed: bool,
    last_requested_diff: Option<DiffRequest>,
    diff_options: DiffOptions,
    diff_view: Option<spiff_widget::DiffView>,
    search_bar: SearchBar,
    search_query: String,
}

impl CommitView {
    pub(super) fn new() -> CommitView {
        Default::default()
    }

    pub(super) fn reset(&mut self) {
        self.last_requested_diff = None;
        self.diff_view = None;
    }

    pub(super) fn update_with_repo_state(&mut self, repo_state: Arc<RepoState>) {
        if self.repo_state.index != repo_state.index {
            self.index_has_changed = true;
        }
        self.repo_state = repo_state;
    }

    pub(super) fn update_diff(&mut self, diff: Diff) {
        if let Some(diff_view) = &mut self.diff_view {
            diff_view.update_data(diff.diff.processed_diffs);
        } else {
            self.diff_view = Some(spiff_widget::DiffView::new(diff.diff.processed_diffs));
        }
    }

    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        cached_commits: &Cache<ObjectId, Commit>,
        selected_commit: &SelectedItem,
    ) -> CommitViewAction {
        let force_open = match spiff_widget::show_header(&mut self.diff_options, ui) {
            spiff_widget::HeaderAction::ExpandAll => Some(true),
            spiff_widget::HeaderAction::CollapseAll => Some(false),
            _ => None,
        };

        let mut action = CommitViewAction::None;

        let selected_commit = match selected_commit {
            SelectedItem::None => {
                ui.allocate_space(ui.available_size());
                return CommitViewAction::None;
            }
            _ => selected_commit,
        };

        if let Some(diff_view) = &mut self.diff_view {
            ScrollArea::vertical().show(ui, |ui| {
                let action = search_bar_wrapped(&mut self.search_bar, ui, |ui, jump_idx| {
                    let message = gen_commit_header(selected_commit, cached_commits);
                    TextEdit::multiline(&mut message.as_str())
                        .font(TextStyle::Monospace)
                        .desired_rows(1)
                        .desired_width(ui.available_width())
                        .ui(ui);
                    diff_view.show(ui, jump_idx, force_open);
                })
                .action;
                match action {
                    SearchBarAction::UpdateSearch(s) => {
                        self.search_query = s;
                    }
                    SearchBarAction::Jump | SearchBarAction::None => (),
                }
            });
        }

        let request = construct_diff_request(
            selected_commit,
            &self.diff_options,
            cached_commits,
            &self.search_query,
            &self.repo_state,
        );

        if request != self.last_requested_diff
            || (self.index_has_changed
                && request.as_ref().map(|x| x.to.clone()) == Some(DiffTarget::Index))
        {
            self.last_requested_diff = request.clone();
            if let Some(request) = request {
                action = CommitViewAction::RequestDiff(request);
            }
            self.index_has_changed = false;
        }

        action
    }
}

fn construct_diff_request(
    selected_item: &SelectedItem,
    options: &DiffOptions,
    commit_cache: &Cache<ObjectId, Commit>,
    search_query: &str,
    repo_state: &RepoState,
) -> Option<DiffRequest> {
    let (from, to) = match selected_item {
        SelectedItem::Object(id) => {
            let commit = match commit_cache.get(id) {
                Some(v) => v,
                None => return None,
            };

            let parent = match commit.metadata.parents.first() {
                // FIXME: Choose which parent to diff to
                // FIXME: Support initial commit
                // FIXME: Support range of commits
                Some(v) => v,
                None => return None,
            };
            let from = DiffTarget::Object(parent.clone());
            let to = DiffTarget::Object(id.clone());
            (from, to)
        }
        SelectedItem::Index => {
            let from = DiffTarget::Object(repo_state.head_object_id());
            let to = DiffTarget::Index;
            (from, to)
        }
        _ => unimplemented!(),
    };

    Some(DiffRequest {
        from,
        to,
        options: options.clone(),
        search_query: search_query.to_string(),
    })
}

fn gen_commit_header(
    selected_item: &SelectedItem,
    cached_commits: &Cache<ObjectId, Commit>,
) -> String {
    match selected_item {
        SelectedItem::Index => "Staged files".to_string(),
        SelectedItem::Object(id) => gen_commit_header_for_object(id, cached_commits),
        SelectedItem::None => panic!("no selected item"),
    }
}

fn gen_commit_header_for_object(
    selected_commit: &ObjectId,
    cached_commits: &Cache<ObjectId, Commit>,
) -> String {
    cached_commits
        .get(selected_commit)
        .map(|commit| {
            format!(
                "id: {}\n\
                    author: {}\n\
                    author timestamp: {}\n\
                    committer timestamp: {}\n\
                    \n\
                    {}",
                commit.metadata.id,
                commit.author,
                commit.metadata.author_timestamp,
                commit.metadata.committer_timestamp,
                commit.message
            )
        })
        .unwrap_or_default()
}
