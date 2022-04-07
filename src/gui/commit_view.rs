use crate::{
    git::{Commit, Diff, DiffContent, DiffFileHeader, ObjectId},
    util::Cache,
};

use eframe::egui::{
    text::LayoutJob, CollapsingHeader, Color32, Galley, ScrollArea, TextEdit, TextFormat,
    TextStyle, Ui, Widget,
};

use std::{collections::BTreeMap, sync::Arc};

#[derive(PartialEq, Eq, Clone)]
pub(super) struct DiffRequest {
    pub(super) ignore_whitespace: bool,
    pub(super) from: ObjectId,
    pub(super) to: ObjectId,
}

pub(super) enum CommitViewAction {
    RequestDiff(DiffRequest),
    None,
}

#[derive(Default)]
pub(super) struct CommitView {
    diff_ignore_whitespace: bool,
    last_requested_diff: Option<DiffRequest>,
    current_diff: Option<Diff>,
}

impl CommitView {
    pub(super) fn new() -> CommitView {
        Default::default()
    }

    pub(super) fn reset(&mut self) {
        self.last_requested_diff = None;
        self.current_diff = None;
    }

    pub(super) fn update_diff(&mut self, diff: Diff) {
        self.current_diff = Some(diff);
    }

    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        cached_commits: &Cache<ObjectId, Commit>,
        selected_commit: Option<&ObjectId>,
    ) -> CommitViewAction {
        let mut force_expanded_state = None;

        ui.allocate_space(ui.style().spacing.item_spacing);

        // Always show header even if there's nothing to show
        ui.horizontal(|ui| {
            if ui.button("Expand all").clicked() {
                force_expanded_state = Some(true);
            }
            if ui.button("Collapse all").clicked() {
                force_expanded_state = Some(false);
            }
            ui.checkbox(&mut self.diff_ignore_whitespace, "Ignore whitespace");
        });

        ui.allocate_space(ui.style().spacing.item_spacing);

        let selected_commit = match selected_commit {
            Some(v) => v,
            None => {
                ui.allocate_space(ui.available_size());
                return CommitViewAction::None;
            }
        };

        ScrollArea::vertical()
            .id_source("commit_view")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let message = gen_commit_header(selected_commit, cached_commits);
                TextEdit::multiline(&mut message.as_str())
                    .font(TextStyle::Monospace)
                    .desired_width(ui.available_width())
                    .ui(ui);

                if let Some(diff) = &self.current_diff {
                    if &diff.to == selected_commit {
                        add_diff_hunks_to_ui(ui, &diff.items, force_expanded_state);
                    }
                }
            });

        let request =
            construct_diff_request(selected_commit, self.diff_ignore_whitespace, cached_commits);

        let mut action = CommitViewAction::None;

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
    ignore_whitespace: bool,
    commit_cache: &Cache<ObjectId, Commit>,
) -> Option<DiffRequest> {
    let commit = match commit_cache.get(selected_commit) {
        Some(v) => v,
        None => return None,
    };

    let parent = match commit.metadata.parents.get(0) {
        // FIXME: Choose which parent to diff to
        // FIXME: Support initial commit
        Some(v) => v,
        None => return None,
    };

    Some(DiffRequest {
        from: parent.clone(),
        to: selected_commit.clone(),
        ignore_whitespace,
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
                    timestamp: {}\n\
                    \n\
                    {}",
                commit.metadata.id, commit.author, commit.metadata.timestamp, commit.message
            )
        })
        .unwrap_or_else(String::new)
}

fn diff_view_layouter(ui: &Ui, s: &str, wrap_width: f32) -> Arc<Galley> {
    // NOTE: no caching here, I think our layout is cheap enough that it doesn't
    // matter, but we have to keep an eye on it

    let mut job = LayoutJob {
        wrap_width,
        ..Default::default()
    };

    let default_color = ui.visuals().text_color();
    let font = ui.style().text_styles[&TextStyle::Monospace].clone();
    for line in s.split_inclusive('\n') {
        let color = match line.as_bytes()[0] {
            b'+' => Color32::LIGHT_GREEN,
            b'-' => Color32::LIGHT_RED,
            _ => default_color,
        };
        job.append(
            line,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
    }
    ui.fonts().layout_job(job)
}

fn add_diff_hunks_to_ui(
    ui: &mut Ui,
    diff_items: &BTreeMap<DiffFileHeader, DiffContent>,
    force_state: Option<bool>,
) {
    for (file, content) in diff_items {
        CollapsingHeader::new(file.to_string())
            .open(force_state)
            .show(ui, |ui| match content {
                DiffContent::Patch(hunks) => {
                    for (hunk, content) in hunks {
                        let mut message = hunk.to_string();
                        message.push_str(
                            std::str::from_utf8(content)
                                .unwrap_or("Patch content is not valid utf8"),
                        );

                        TextEdit::multiline(&mut message.as_str())
                            .font(TextStyle::Monospace)
                            .desired_width(ui.available_width())
                            .layouter(&mut diff_view_layouter)
                            .ui(ui);
                    }
                }
                DiffContent::Binary => {
                    TextEdit::multiline(&mut "Binary content changed")
                        .font(TextStyle::Monospace)
                        .desired_width(ui.available_width())
                        .ui(ui);
                }
            });
    }
}
