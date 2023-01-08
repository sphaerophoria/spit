use crate::{
    git::{Commit, Diff, ObjectId},
    util::Cache,
};

use eframe::egui::{TextEdit, TextStyle, Ui, Widget};

use spiff::widget::{self as spiff_widget, DiffViewAction};
use spiff::DiffOptions;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessedDiffOffset {
    file_index: usize,
    string_index: usize,
}

#[derive(PartialEq, Eq, Clone)]
pub(super) struct DiffRequest {
    pub(super) options: DiffOptions,
    pub(super) from: ObjectId,
    pub(super) to: ObjectId,
    pub(super) search_query: String,
}

pub(super) enum CommitViewAction {
    RequestDiff(DiffRequest),
    None,
}

#[derive(Default)]
pub(super) struct CommitView {
    last_requested_diff: Option<DiffRequest>,
    diff_options: DiffOptions,
    diff_view: Option<spiff_widget::DiffView>,
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

    pub(super) fn update_diff(&mut self, diff: Diff) {
        if let Some(diff_view) = &mut self.diff_view {
            diff_view.update_data(diff.diff);
        } else {
            self.diff_view = Some(spiff_widget::DiffView::new(diff.diff));
        }
    }

    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        cached_commits: &Cache<ObjectId, Commit>,
        selected_commit: Option<&ObjectId>,
    ) -> CommitViewAction {
        let force_open = match spiff_widget::show_header(&mut self.diff_options, ui) {
            spiff_widget::HeaderAction::ExpandAll => Some(true),
            spiff_widget::HeaderAction::CollapseAll => Some(false),
            _ => None,
        };

        let mut action = CommitViewAction::None;

        let selected_commit = match selected_commit {
            Some(v) => v,
            None => {
                ui.allocate_space(ui.available_size());
                return CommitViewAction::None;
            }
        };

        if let Some(diff_view) = &mut self.diff_view {
            match diff_view.show_with_additional_content(ui, force_open, |ui| {
                let message = gen_commit_header(selected_commit, cached_commits);
                TextEdit::multiline(&mut message.as_str())
                    .font(TextStyle::Monospace)
                    .desired_width(ui.available_width())
                    .ui(ui);
            }) {
                DiffViewAction::UpdateSearch(s) => {
                    self.search_query = s;
                }
                DiffViewAction::None => (),
            }
        }

        let request = construct_diff_request(
            selected_commit,
            &self.diff_options,
            cached_commits,
            &self.search_query,
        );
        if request != self.last_requested_diff {
            self.last_requested_diff = request.clone();
            if let Some(request) = request {
                action = CommitViewAction::RequestDiff(request);
            }
        }

        action
    }
}

fn construct_diff_request(
    selected_commit: &ObjectId,
    options: &DiffOptions,
    commit_cache: &Cache<ObjectId, Commit>,
    search_query: &str,
) -> Option<DiffRequest> {
    let commit = match commit_cache.get(selected_commit) {
        Some(v) => v,
        None => return None,
    };

    let parent = match commit.metadata.parents.get(0) {
        // FIXME: Choose which parent to diff to
        // FIXME: Support initial commit
        // FIXME: Support range of commits
        Some(v) => v,
        None => return None,
    };

    Some(DiffRequest {
        from: parent.clone(),
        to: selected_commit.clone(),
        options: options.clone(),
        search_query: search_query.to_string(),
    })
}

fn gen_commit_header(
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
        .unwrap_or_else(String::new)
}
