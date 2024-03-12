use crate::{
    app::{DiffRequest, RepoState},
    git::{Commit, Diff, DiffMetadata, DiffTarget, ObjectId},
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

pub(super) enum CommitViewAction {
    RequestDiff(Vec<DiffRequest>),
    None,
}

#[derive(Default)]
pub(super) struct CommitView {
    repo_state: Arc<RepoState>,
    index_has_changed: bool,
    workdir_has_changed: bool,
    last_requested_diff: Vec<DiffRequest>,
    last_received_diff: Vec<DiffMetadata>,
    diff_options: DiffOptions,
    diff_views: Vec<spiff_widget::DiffView>,
    search_bar: SearchBar,
    search_query: String,
}

impl CommitView {
    pub(super) fn new() -> CommitView {
        Default::default()
    }

    pub(super) fn reset(&mut self) {
        self.last_requested_diff = Vec::new();
        self.diff_views = Vec::new();
    }

    pub(super) fn notify_workdir_updated(&mut self) {
        self.workdir_has_changed = true;
    }

    pub(super) fn update_with_repo_state(&mut self, repo_state: Arc<RepoState>) {
        if self.repo_state.index != repo_state.index {
            self.index_has_changed = true;
        }
        self.repo_state = repo_state;
    }

    pub(super) fn update_diffs(&mut self, diffs: Vec<Diff>) {
        self.diff_views.clear();
        self.last_received_diff.clear();

        for diff in diffs {
            self.diff_views
                .push(spiff_widget::DiffView::new(diff.diff.processed_diffs));
            self.last_received_diff.push(diff.metadata);
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

        if !self.diff_views.is_empty() {
            let headers = gen_commit_headers(selected_commit, cached_commits);

            let num_diff_views = self.diff_views.len();

            if num_diff_views == 1 {
                let action = search_bar_wrapped(&mut self.search_bar, ui, |ui, jump_idx| {
                    render_diffs(ui, jump_idx, &headers, &mut self.diff_views, force_open);
                })
                .action;

                match action {
                    SearchBarAction::UpdateSearch(s) => {
                        self.search_query = s;
                    }
                    SearchBarAction::Jump | SearchBarAction::None => (),
                }
            } else {
                render_diffs(ui, None, &headers, &mut self.diff_views, force_open);
            }
        } else {
            ui.allocate_space(ui.available_size());
        }

        let requests = construct_diff_requests(
            selected_commit,
            &self.diff_options,
            cached_commits,
            &self.search_query,
            &self.repo_state,
        );

        let update_needed_from_index_change = || {
            if !self.index_has_changed {
                return false;
            }

            if requests.iter().any(|x| x.to == DiffTarget::Index) {
                return true;
            }

            if requests.iter().any(|x| x.from == DiffTarget::Index) {
                return true;
            }

            false
        };

        let update_needed_from_workdir_change = || {
            self.workdir_has_changed
                && requests
                    .iter()
                    .any(|x| x.to == DiffTarget::WorkingDirModified)
        };

        if requests != self.last_requested_diff
            || update_needed_from_index_change()
            || update_needed_from_workdir_change()
        {
            self.last_requested_diff = requests.clone();
            if !received_diffs_match_request_targets(&requests, &self.last_received_diff) {
                self.diff_views = Vec::new();
            }

            if !requests.is_empty() {
                action = CommitViewAction::RequestDiff(requests);
            }
            self.index_has_changed = false;
            self.workdir_has_changed = false;
        }

        action
    }
}

fn render_diffs(
    ui: &mut Ui,
    jump_idx: Option<(usize, usize)>,
    headers: &[String],
    diff_views: &mut [spiff_widget::DiffView],
    force_open: Option<bool>,
) {
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for (view, header) in diff_views.iter_mut().zip(headers) {
            TextEdit::multiline(&mut header.as_str())
                .font(TextStyle::Monospace)
                .desired_rows(1)
                .desired_width(ui.available_width())
                .ui(ui);

            view.show(ui, jump_idx, force_open);
        }
    });
}

fn received_diffs_match_request_targets(req: &[DiffRequest], received: &[DiffMetadata]) -> bool {
    if req.len() != received.len() {
        return false;
    }

    for (req_item, response_item) in req.iter().zip(received) {
        if req_item.from != response_item.from {
            return false;
        }

        if req_item.to != response_item.to {
            return false;
        }
    }

    true
}

fn construct_diff_requests(
    selected_item: &SelectedItem,
    options: &DiffOptions,
    commit_cache: &Cache<ObjectId, Commit>,
    search_query: &str,
    repo_state: &RepoState,
) -> Vec<DiffRequest> {
    struct Pair {
        from: DiffTarget,
        to: DiffTarget,
    }

    let pairs = match selected_item {
        SelectedItem::Object(id) => {
            let commit = match commit_cache.get(id) {
                Some(v) => v,
                None => return Vec::new(),
            };

            let parent = match commit.metadata.parents.first() {
                // FIXME: Choose which parent to diff to
                // FIXME: Support initial commit
                // FIXME: Support range of commits
                Some(v) => v,
                None => return Vec::new(),
            };
            let from = DiffTarget::Object(parent.clone());
            let to = DiffTarget::Object(id.clone());
            vec![Pair { from, to }]
        }
        SelectedItem::Index => {
            vec![
                Pair {
                    from: DiffTarget::Object(repo_state.head_object_id()),
                    to: DiffTarget::Index,
                },
                Pair {
                    from: DiffTarget::Index,
                    to: DiffTarget::WorkingDirModified,
                },
                Pair {
                    from: DiffTarget::Index,
                    to: DiffTarget::WorkingDirUntracked,
                },
            ]
        }
        _ => unimplemented!(),
    };

    pairs
        .into_iter()
        .map(|p| DiffRequest {
            from: p.from,
            to: p.to,
            options: options.clone(),
            search_query: search_query.to_string(),
        })
        .collect()
}

fn gen_commit_headers(
    selected_item: &SelectedItem,
    cached_commits: &Cache<ObjectId, Commit>,
) -> Vec<String> {
    match selected_item {
        SelectedItem::Index => {
            vec![
                "Staged files".to_string(),
                "Modified files".to_string(),
                "Untracked files".to_string(),
            ]
        }
        SelectedItem::Object(id) => vec![gen_commit_header_for_object(id, cached_commits)],
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
