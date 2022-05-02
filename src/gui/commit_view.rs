use crate::{
    git::{Commit, Diff, DiffContent, DiffFileHeader, DiffMetadata, ObjectId},
    gui::{tristate_checkbox::TristateCheckbox, SearchAction, SearchBar},
    util::Cache,
};

use anyhow::{Error, Result};
use log::error;

use eframe::{
    egui::{
        text::LayoutJob, Align, CollapsingHeader, Color32, ScrollArea, TextEdit, TextFormat,
        TextStyle, Ui, Widget,
    },
    epaint::text::{cursor::CCursor, TextWrapping},
};

use std::{fmt::Write, hash::Hash};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessedDiffOffset {
    file_index: usize,
    string_index: usize,
}

type FileDiffStrings = Vec<(DiffFileHeader, String)>;

struct ProcessedDiff {
    metadata: DiffMetadata,
    file_diff_strings: FileDiffStrings,
}

impl TryFrom<Diff> for ProcessedDiff {
    type Error = Error;
    fn try_from(diff: Diff) -> Result<ProcessedDiff> {
        let Diff { metadata, items } = diff;

        let ordered_hunks = items
            .into_iter()
            .map(|(file, hunk)| Ok((file, diff_content_to_string(&hunk)?)))
            .collect::<Result<Vec<_>>>()?;

        Ok(ProcessedDiff {
            metadata,
            file_diff_strings: ordered_hunks,
        })
    }
}

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

pub(super) struct CommitView {
    diff_ignore_whitespace: bool,
    last_requested_diff: Option<DiffRequest>,
    current_diff: Option<ProcessedDiff>,
    // NOTE: We only store the hash of the string to avoid storing the diff string multiple times.
    // Technically there's a chance of hash collision here, but given that we are only caching a
    // single layout job I'd like to see a hash collision before doing something better
    layout_cache: Cache<u64, LayoutJob>,
    search_text: String,
    search_result_offsets: Vec<ProcessedDiffOffset>,
    search_result_offset_index: usize,
}

impl Default for CommitView {
    fn default() -> CommitView {
        CommitView {
            diff_ignore_whitespace: Default::default(),
            last_requested_diff: Default::default(),
            current_diff: Default::default(),
            layout_cache: Cache::new(1),
            search_text: Default::default(),
            search_result_offsets: Default::default(),
            search_result_offset_index: Default::default(),
        }
    }
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
        match ProcessedDiff::try_from(diff) {
            Ok(d) => {
                self.layout_cache.set_size(d.file_diff_strings.len());
                self.current_diff = Some(d);
                self.update_search_results();
            }
            Err(e) => {
                error!("Failed to render diff: {}", e);
                self.current_diff = None;
            }
        }
    }

    fn decrement_offset_index(&mut self) {
        if self.search_result_offset_index == 0 {
            self.search_result_offset_index = self.search_result_offsets.len().saturating_sub(1);
        } else {
            self.search_result_offset_index -= 1;
        }
    }

    fn increment_offset_index(&mut self) {
        if self.search_result_offset_index >= self.search_result_offsets.len().saturating_sub(1) {
            self.search_result_offset_index = 0;
        } else {
            self.search_result_offset_index += 1;
        }
    }

    fn update_search_results(&mut self) {
        self.search_result_offset_index = 0;
        if let Some(diff) = &self.current_diff {
            self.search_result_offsets = find_in_diff(&diff.file_diff_strings, &self.search_text);
        }
    }

    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        cached_commits: &Cache<ObjectId, Commit>,
        selected_commit: Option<&ObjectId>,
    ) -> CommitViewAction {
        let mut force_expanded_state = None;
        let mut jump_to_selected_highlight = false;

        ui.allocate_space(ui.style().spacing.item_spacing);

        // Always show header even if there's nothing to show
        ui.horizontal(|ui| {
            if ui.button("Expand all").clicked() {
                force_expanded_state = Some(true);
            }
            if ui.button("Collapse all").clicked() {
                force_expanded_state = Some(false);
            }

            TristateCheckbox::new(
                self.current_diff
                    .as_ref()
                    .map(|d| &d.metadata.ignore_whitespace)
                    .unwrap_or(&self.diff_ignore_whitespace.clone()),
                &mut self.diff_ignore_whitespace,
                "Ignore whitespace",
            )
            .ui(ui);

            let search_action = SearchBar::new(&mut self.search_text)
                .desired_width(300.0)
                .show(ui);

            match search_action {
                SearchAction::Changed => {
                    self.update_search_results();
                    if !self.search_text.is_empty() {
                        jump_to_selected_highlight = true;
                    }
                }
                SearchAction::Prev => {
                    self.decrement_offset_index();
                    jump_to_selected_highlight = true;
                }
                SearchAction::Next => {
                    self.increment_offset_index();
                    jump_to_selected_highlight = true;
                }
                SearchAction::None => (),
            }
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
                    if &diff.metadata.to == selected_commit {
                        add_diff_hunks_to_ui(
                            ui,
                            &mut self.layout_cache,
                            diff,
                            force_expanded_state,
                            self.search_text.len(),
                            &self.search_result_offsets,
                            self.search_result_offsets
                                .get(self.search_result_offset_index),
                            jump_to_selected_highlight,
                        );
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

fn extract_line_highlight_positions(
    line_pos: usize,
    line_len: usize,
    highlight_positions: &mut Vec<usize>,
) -> Vec<usize> {
    let mut line_highlight_positions = Vec::new();
    let mut to_pop = 0;
    for pos in highlight_positions.iter().rev() {
        if line_pos + line_len < *pos {
            break;
        }
        line_highlight_positions.push(*pos);
        to_pop += 1;
    }

    for _ in 0..to_pop {
        highlight_positions.pop();
    }

    line_highlight_positions
}

fn diff_view_layouter(
    ui: &Ui,
    cache: &mut Cache<u64, LayoutJob>,
    s: &str,
    wrap_width: f32,
    selected_highlight_pos: Option<usize>,
    mut highlight_positions: Vec<usize>,
    highlight_len: usize,
) -> LayoutJob {
    let hash = eframe::egui::util::hash(s);

    if let Some(job) = cache.get(&hash) {
        return job.clone();
    }

    let wrap = TextWrapping {
        max_width: wrap_width,
        ..Default::default()
    };
    let mut job = LayoutJob {
        wrap,
        ..Default::default()
    };

    highlight_positions.sort_by(|a, b| b.cmp(a));

    let default_color = ui.visuals().text_color();
    let font = ui.style().text_styles[&TextStyle::Monospace].clone();
    let mut processed_chars = 0;
    for line in s.split_inclusive('\n') {
        let line_highlight_positions =
            extract_line_highlight_positions(processed_chars, line.len(), &mut highlight_positions);

        let line_color = match line.as_bytes()[0] {
            b'+' => Color32::LIGHT_GREEN,
            b'-' => Color32::LIGHT_RED,
            _ => default_color,
        };

        let mut last_appended_idx = 0;
        for highlight_pos in line_highlight_positions {
            let highlight_pos_rel_line = highlight_pos - processed_chars;
            job.append(
                &line[last_appended_idx..highlight_pos_rel_line],
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: line_color,
                    ..Default::default()
                },
            );

            let highlight_color = get_highlight_color(&selected_highlight_pos, highlight_pos);

            job.append(
                &line[highlight_pos_rel_line..highlight_pos_rel_line + highlight_len],
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: highlight_color,
                    ..Default::default()
                },
            );
            last_appended_idx = highlight_pos_rel_line + highlight_len;
        }

        if last_appended_idx < line.len() {
            job.append(
                &line[last_appended_idx..],
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: line_color,
                    ..Default::default()
                },
            );
        }

        processed_chars += line.len();
    }

    cache.push(hash, job.clone());

    job
}

#[allow(clippy::too_many_arguments)]
fn add_diff_hunks_to_ui(
    ui: &mut Ui,
    layout_cache: &mut Cache<u64, LayoutJob>,
    diff: &ProcessedDiff,
    force_state: Option<bool>,
    highlight_len: usize,
    highlight_positions: &[ProcessedDiffOffset],
    selected_highlight_pos: Option<&ProcessedDiffOffset>,
    jump_to_selected: bool,
) {
    for (file_idx, (file, content)) in diff.file_diff_strings.iter().enumerate() {
        #[derive(Hash)]
        struct CommitViewId<'a> {
            from: &'a ObjectId,
            to: &'a ObjectId,
            file: &'a DiffFileHeader,
        }

        let mut force_open_file = force_state;
        if let Some(force_file_idx) = selected_highlight_pos.map(|o| o.file_index) {
            if jump_to_selected && file_idx == force_file_idx {
                force_open_file = Some(true);
            }
        }

        CollapsingHeader::new(file.to_string())
            .id_source(CommitViewId {
                from: &diff.metadata.from,
                to: &diff.metadata.to,
                file,
            })
            .open(force_open_file)
            .show(ui, |ui| {
                // NOTE: It would be nice if we could use the integrated selection to mark the
                // searched text. Unfortunately the TextEdit can only show selected text when the
                // widget has focus[1]. There's a chance that we could do some trickery by setting
                // focus before rendering the widget and then restoring focus, but that seems like
                // it would cause more UX confusion. So we just color the text in the layouter
                //
                // [1]: https://github.com/emilk/egui/blob/a05520b9d3abcfc5fe0a963c621b8e398005fa02/egui/src/widgets/text_edit/builder.rs#L490

                let this_highlight_positions: Vec<usize> = highlight_positions
                    .iter()
                    .filter_map(|pos| {
                        if pos.file_index == file_idx {
                            Some(pos.string_index)
                        } else {
                            None
                        }
                    })
                    .collect();

                let file_selected_highlight_pos = selected_highlight_pos.and_then(|pos| {
                    if pos.file_index == file_idx {
                        Some(pos.string_index)
                    } else {
                        None
                    }
                });

                let response = TextEdit::multiline(&mut content.as_str())
                    .font(TextStyle::Monospace)
                    .desired_width(ui.available_width())
                    .layouter(&mut |ui, s, wrap_width| {
                        let job = diff_view_layouter(
                            ui,
                            layout_cache,
                            s,
                            wrap_width,
                            file_selected_highlight_pos,
                            this_highlight_positions.clone(),
                            highlight_len,
                        );
                        ui.fonts().layout_job(job)
                    })
                    .show(ui);

                if let Some(selected_highlight_pos) = selected_highlight_pos {
                    if jump_to_selected && selected_highlight_pos.file_index == file_idx {
                        let cursor = response
                            .galley
                            .from_ccursor(CCursor::new(selected_highlight_pos.string_index));
                        let mut pos = response.galley.pos_from_cursor(&cursor);
                        pos.min.x += response.text_draw_pos.x;
                        pos.min.y += response.text_draw_pos.y;
                        pos.max = pos.min;
                        ui.scroll_to_rect(pos, Some(Align::Center));
                    }
                }
            });
    }
}

fn diff_content_to_string(content: &DiffContent) -> Result<String> {
    let s = match content {
        DiffContent::Patch(hunks) => {
            let mut s = String::new();
            for (hunk, content) in hunks {
                write!(s, "{}", hunk)?;
                write!(s, "{}", std::str::from_utf8(content)?)?;
            }
            s
        }
        DiffContent::Binary => "Binary content changed".to_string(),
    };

    Ok(s)
}

fn find_in_diff(diff: &FileDiffStrings, text: &str) -> Vec<ProcessedDiffOffset> {
    diff.iter()
        .enumerate()
        .flat_map(|(file_index, file)| {
            file.1
                .match_indices(text)
                .map(move |(string_index, _)| ProcessedDiffOffset {
                    file_index,
                    string_index,
                })
        })
        .collect()
}

/// Function that determines the highlight color during view layout
/// selected_pos should be relative to the entire string being layouted
/// line_pos should be how many characters were processed bef
fn get_highlight_color(selected_pos: &Option<usize>, highlight_pos: usize) -> Color32 {
    match selected_pos {
        Some(v) => {
            if *v == highlight_pos {
                Color32::YELLOW
            } else {
                Color32::LIGHT_BLUE
            }
        }
        None => Color32::LIGHT_BLUE,
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn find_in_diff_no_match() {
        let file_diff_strings = FromIterator::from_iter([(
            DiffFileHeader {
                old_file: None,
                new_file: None,
            },
            "Test".into(),
        )]);
        assert_eq!(find_in_diff(&file_diff_strings, "asdfasdf"), vec![]);
    }

    #[test]
    fn find_in_diff_sequence() {
        let file_diff_strings = FromIterator::from_iter([
            (
                DiffFileHeader {
                    old_file: None,
                    new_file: None,
                },
                "TestTest".into(),
            ),
            (
                DiffFileHeader {
                    old_file: None,
                    new_file: None,
                },
                "TestTest".into(),
            ),
        ]);
        assert_eq!(
            find_in_diff(&file_diff_strings, "Test"),
            vec![
                ProcessedDiffOffset {
                    file_index: 0,
                    string_index: 0
                },
                ProcessedDiffOffset {
                    file_index: 0,
                    string_index: 4
                },
                ProcessedDiffOffset {
                    file_index: 1,
                    string_index: 0
                },
                ProcessedDiffOffset {
                    file_index: 1,
                    string_index: 4
                },
            ]
        );
    }

    #[test]
    fn test_extract_line_highlight_positions() {
        let mut highlight_positions = vec![132, 43, 10, 9, 1];

        let line_highlight_positions =
            extract_line_highlight_positions(0, 2, &mut highlight_positions);

        assert_eq!(&line_highlight_positions, &[1]);
        assert_eq!(&highlight_positions, &[132, 43, 10, 9]);

        let line_highlight_positions =
            extract_line_highlight_positions(2, 10, &mut highlight_positions);

        assert_eq!(&line_highlight_positions, &[9, 10]);
        assert_eq!(&highlight_positions, &[132, 43]);

        let line_highlight_positions =
            extract_line_highlight_positions(12, 10, &mut highlight_positions);

        assert_eq!(&line_highlight_positions, &[]);
        assert_eq!(&highlight_positions, &[132, 43]);

        let line_highlight_positions =
            extract_line_highlight_positions(43, 100, &mut highlight_positions);

        assert_eq!(&line_highlight_positions, &[43, 132]);
        assert_eq!(&highlight_positions, &[]);

        let line_highlight_positions =
            extract_line_highlight_positions(43, 100, &mut highlight_positions);

        assert_eq!(&line_highlight_positions, &[]);
        assert_eq!(&highlight_positions, &[]);
    }
}
