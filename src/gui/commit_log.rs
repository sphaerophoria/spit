use crate::{
    app::RepoState,
    git::{
        graph::{Edge, GraphPoint},
        Commit, HistoryGraph, Identifier, ObjectId, ReferenceId,
    },
    gui::{reference_color, reference_underline, try_set_clipboard, SearchAction, SearchBar},
    util::Cache,
};

use clipboard::ClipboardContext;
use eframe::egui::{
    text::LayoutJob, Button, Frame, Label, Layout, Pos2, Rect, Response, ScrollArea, Sense, Stroke,
    TextFormat, TextStyle, Ui, Vec2, Widget, WidgetText,
};

use std::{collections::HashMap, ops::Range, sync::Arc};

struct PositionConverter {
    row_height: f32,
    spacing: Vec2,
    max_rect: Rect,
    scroll_range: Range<usize>,
}

impl PositionConverter {
    fn new(ui: &Ui, row_height: f32, scroll_range: Range<usize>) -> PositionConverter {
        let max_rect = ui.max_rect();
        let spacing = ui.style().spacing.item_spacing;
        PositionConverter {
            row_height,
            spacing,
            max_rect,
            scroll_range,
        }
    }

    fn graph_x_to_ui_x(&self, graph_x: i32) -> f32 {
        const X_MULTIPLIER: f32 = 10.0;
        graph_x as f32 * X_MULTIPLIER + self.max_rect.left() + self.spacing.x
    }

    fn graph_y_to_ui_y(&self, graph_y: i32) -> f32 {
        (graph_y - self.scroll_range.start as i32) as f32 * (self.row_height + self.spacing.y)
            + self.row_height / 2.0
            + self.max_rect.top()
    }

    fn text_rect(&self, max_x_pos: i32) -> Rect {
        Rect::from_min_max(
            Pos2::new(
                self.graph_x_to_ui_x(max_x_pos) + self.spacing.x,
                self.max_rect.top(),
            ),
            Pos2::new(self.max_rect.right(), self.max_rect.bottom()),
        )
    }
}

fn render_edges(
    ui: &mut Ui,
    edges: &[Edge],
    converter: &PositionConverter,
    row_range: &Range<usize>,
) -> i32 {
    let stroke = ui.style().visuals.widgets.open.fg_stroke;

    let mut max_edge_x = 0;
    let edge_end_idx = match edges.binary_search_by(|elem| (elem.a.y as usize).cmp(&row_range.end))
    {
        Ok(v) => v,
        Err(v) => v,
    };

    for edge in &edges[..edge_end_idx] {
        // FIXME: Filtering every frame is expensive
        if (edge.b.y as usize) < row_range.start || (edge.a.y as usize) > row_range.end {
            continue;
        }

        let a = Pos2 {
            x: converter.graph_x_to_ui_x(edge.a.x),
            y: converter.graph_y_to_ui_y(edge.a.y),
        };
        let b = Pos2 {
            x: converter.graph_x_to_ui_x(edge.b.x),
            y: converter.graph_y_to_ui_y(edge.b.y),
        };
        ui.painter().line_segment([a, b], stroke);
        let edge_end = i32::max(edge.a.x, edge.b.x);
        max_edge_x = i32::max(edge_end, max_edge_x);
    }

    max_edge_x
}

fn render_commit_message<M: Into<WidgetText>>(ui: &mut Ui, message: M, selected: bool) -> Response {
    // Would be nice to use SeletableLabel, but I couldn't find a way to prevent it from
    // wrapping
    let (pos, galley, message_response) = Label::new(message)
        .wrap(false)
        .sense(Sense::click())
        .layout_in_ui(ui);

    if selected {
        let visuals = ui.style().interact_selectable(&message_response, true);
        ui.painter().rect(
            message_response.rect,
            visuals.rounding,
            visuals.bg_fill,
            visuals.bg_stroke,
        );
    } else {
        let visuals = ui.style().interact_selectable(&message_response, false);
        ui.painter()
            .rect_stroke(message_response.rect, visuals.rounding, visuals.bg_stroke);
    }

    ui.painter().galley(pos, galley, ui.visuals().text_color());

    message_response
}

fn render_commit_node(
    ui: &mut Ui,
    node_pos: &GraphPoint,
    converter: &PositionConverter,
    filled: bool,
) {
    let stroke = ui.style().visuals.widgets.open.fg_stroke;
    let node_pos = Pos2 {
        x: converter.graph_x_to_ui_x(node_pos.x),
        y: converter.graph_y_to_ui_y(node_pos.y),
    };

    const CIRCLE_SIZE: f32 = 3.0;
    if filled {
        ui.painter()
            .circle_filled(node_pos, CIRCLE_SIZE, stroke.color);
    } else {
        ui.painter().circle_stroke(node_pos, CIRCLE_SIZE, stroke);
    }
}

fn build_branch_id_lookup(state: &RepoState) -> HashMap<ObjectId, Vec<ReferenceId>> {
    let mut ret: HashMap<ObjectId, Vec<ReferenceId>> = HashMap::new();
    for branch in &state.references {
        let entry = ret.entry(branch.head.clone()).or_default();
        entry.push(branch.id.clone());
    }

    ret
}

fn add_no_wrap_button(ui: &mut Ui, label: &str) -> Response {
    Button::new(label).wrap(false).ui(ui)
}

fn generate_search<'a, T>(
    len: usize,
    selected_commit: Option<&ObjectId>,
    iter: T,
    search_string: String,
) -> CommitLogAction
where
    T: Iterator<Item = &'a ObjectId>,
{
    let mut commit_list = Vec::with_capacity(len);
    let mut rotate_pos = 0;
    for (i, id) in iter.enumerate() {
        if Some(id) == selected_commit {
            rotate_pos = i + 1;
        }

        commit_list.push(id.clone());
    }

    commit_list.rotate_left(rotate_pos);

    CommitLogAction::Search {
        commit_list,
        search_string,
    }
}

fn generate_search_next(
    commit_graph: &HistoryGraph,
    selected_commit: Option<&ObjectId>,
    search_string: String,
) -> CommitLogAction {
    generate_search(
        commit_graph.nodes.len(),
        selected_commit,
        commit_graph.nodes.iter().map(|x| &x.id),
        search_string,
    )
}

fn generate_search_prev(
    commit_graph: &HistoryGraph,
    selected_commit: Option<&ObjectId>,
    search_string: String,
) -> CommitLogAction {
    generate_search(
        commit_graph.nodes.len(),
        selected_commit,
        commit_graph.nodes.iter().rev().map(|x| &x.id),
        search_string,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_commit_graph(
    row_range: Range<usize>,
    commit_graph: &HistoryGraph,
    ui: &mut Ui,
    row_height: f32,
    selected_commit: &mut SelectedItem,
    next_selected_commit: &mut SelectedItem,
    commit_cache: &Cache<ObjectId, Commit>,
    repo_state: &RepoState,
    actions: &mut Vec<CommitLogAction>,
    clipboard: &mut ClipboardContext,
) {
    if row_range.end > commit_graph.nodes.len() || row_range.start > commit_graph.nodes.len() {
        ui.scroll_to_cursor(None);
        return;
    }

    let converter = PositionConverter::new(ui, row_height, row_range.clone());

    if let SelectedItem::None = next_selected_commit {
    } else {
        *selected_commit = next_selected_commit.clone();

        let selected_pos = commit_graph
            .nodes
            .iter()
            .position(|node| &SelectedItem::Object(node.id.clone()) == next_selected_commit);

        if let Some(selected_pos) = selected_pos {
            let min_y = converter.graph_y_to_ui_y(selected_pos as i32);
            let max_y = converter.graph_y_to_ui_y((selected_pos + 1) as i32);
            let min_pos = Pos2::new(0.0, min_y);
            let max_pos = Pos2::new(0.0, max_y);
            ui.scroll_to_rect(Rect::from_min_max(min_pos, max_pos), None);
        }
    }
    *next_selected_commit = SelectedItem::None;

    let max_edge_x = render_edges(ui, &commit_graph.edges, &converter, &row_range);
    let text_rect = converter.text_rect(max_edge_x);
    let mut text_ui = ui.child_ui(text_rect, Layout::default());

    // I'm unsure that this is right, however both Ui::max_rect and Ui::clip_rect are
    // not small enough
    let clip_rect = ui.clip_rect();
    text_ui.set_clip_rect(Rect::from_min_max(
        Pos2::new(
            f32::max(clip_rect.left(), text_rect.left()),
            f32::max(clip_rect.top(), text_rect.top()),
        ),
        Pos2::new(
            f32::min(clip_rect.right(), text_rect.right()),
            f32::min(clip_rect.bottom(), text_rect.bottom()),
        ),
    ));

    let branch_id_lookup = build_branch_id_lookup(repo_state);
    for node in &commit_graph.nodes[row_range] {
        render_commit_node(ui, &node.position, &converter, true);

        let mut job = LayoutJob::default();
        let style = text_ui.style();
        let font = style.text_styles[&TextStyle::Body].clone();
        let mut node_branches = Vec::new();

        if let Some(ids) = branch_id_lookup.get(&node.id) {
            for id in ids {
                node_branches.push(id);

                let name = id.to_string();
                let color = reference_color(id);
                let underline = reference_underline(id, repo_state);
                let mut textformat = TextFormat::simple(font.clone(), color);
                if underline {
                    textformat.underline = Stroke::new(2.0, color);
                }

                job.append(&name, 0.0, textformat);
                job.append(
                    " ",
                    0.0,
                    TextFormat::simple(font.clone(), style.visuals.text_color()),
                );
            }
        }

        let message = match commit_cache.get(&node.id) {
            Some(v) => v
                .message
                .split('\n')
                .next()
                .map(|v| v.to_string())
                .unwrap_or_else(|| v.message.clone()),
            None => {
                actions.push(CommitLogAction::FetchCommit(node.id.clone()));
                node.id.to_string()
            }
        };

        job.append(
            &message,
            0.0,
            TextFormat::simple(font, style.visuals.text_color()),
        );

        let selected = *selected_commit == SelectedItem::Object(node.id.clone());
        let commit_message_response = render_commit_message(&mut text_ui, job, selected);
        if commit_message_response.clicked() {
            *selected_commit = SelectedItem::Object(node.id.clone());
        }

        commit_message_response.context_menu(|ui| {
            let all_refs = node_branches
                .iter()
                .map(|x| Identifier::Reference((*x).clone()));

            let hash_and_all_refs = [Identifier::Object(node.id.clone())]
                .into_iter()
                .chain(all_refs.clone());

            let hash_and_local_branches = [Identifier::Object(node.id.clone())].into_iter().chain(
                node_branches.iter().filter_map(|x| match x {
                    ReferenceId::LocalBranch(_) => Some(Identifier::Reference((*x).clone())),
                    _ => None,
                }),
            );

            let local_refs = node_branches.iter().filter_map(|x| match x {
                ReferenceId::LocalBranch(_) | ReferenceId::Tag(_) => Some((*x).clone()),
                _ => None,
            });

            if let Some(identifier) = add_submenu(ui, "Checkout", hash_and_local_branches.clone()) {
                actions.push(CommitLogAction::Checkout(identifier));
            }

            if let Some(identifier) = add_submenu(ui, "Delete", local_refs.clone()) {
                actions.push(CommitLogAction::DeleteReference(identifier));
            }

            if add_no_wrap_button(ui, "Cherry pick").clicked() {
                actions.push(CommitLogAction::CherryPick(node.id.clone()));
                ui.close_menu();
            }

            if let Some(identifier) = add_submenu(ui, "Merge", hash_and_all_refs.clone()) {
                actions.push(CommitLogAction::Merge(identifier));
            }

            if add_no_wrap_button(ui, "Open diff tool").clicked() {
                actions.push(CommitLogAction::DiffTool(node.id.clone()));
                ui.close_menu();
            }

            ui.separator();

            if let Some(identifier) = add_submenu(ui, "Copy", hash_and_all_refs.clone()) {
                try_set_clipboard(clipboard, identifier.to_string());
            }

            if let Some(identifier) =
                add_submenu(ui, "Append to command", hash_and_all_refs.clone())
            {
                actions.push(CommitLogAction::Append(identifier.to_string()));
            }

            if let Some(identifier) = add_submenu(
                ui,
                "Append to command (with space)",
                hash_and_all_refs.clone(),
            ) {
                actions.push(CommitLogAction::Append(format!("{} ", identifier)));
            }
        });
    }
}

// Question mark hurts readability here IMO
#[allow(clippy::question_mark)]
fn add_submenu<I, T>(ui: &mut Ui, name: &str, ids: I) -> Option<T>
where
    I: IntoIterator<Item = T>,
    T: ToString,
{
    let mut ids = ids.into_iter().peekable();

    if ids.peek().is_none() {
        return None;
    }

    ui.menu_button(name, |ui| {
        for id in ids {
            if add_no_wrap_button(ui, &id.to_string()).clicked() {
                ui.close_menu();
                return Some(id);
            }
        }
        None
    })
    .inner
    .flatten()
}

pub(super) enum CommitLogAction {
    FetchCommit(ObjectId),
    Checkout(Identifier),
    DeleteReference(ReferenceId),
    CherryPick(ObjectId),
    Merge(Identifier),
    Append(String),
    DiffTool(ObjectId),
    Search {
        commit_list: Vec<ObjectId>,
        search_string: String,
    },
}

#[derive(Clone, PartialEq)]
pub(super) enum SelectedItem {
    Index,
    Object(ObjectId),
    None,
}

impl Default for SelectedItem {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Default)]
pub(super) struct CommitLog {
    repo_state: Arc<RepoState>,
    commit_graph: Option<HistoryGraph>,
    selected_commit: SelectedItem,
    next_selected_commit: SelectedItem,
    search_string: String,
}

impl CommitLog {
    pub(super) fn update_with_repo_state(&mut self, repo_state: Arc<RepoState>) {
        self.repo_state = repo_state;
    }

    pub(super) fn update_graph(&mut self, mut commit_graph: HistoryGraph) {
        // Sort the start positions in increasing order
        commit_graph.edges.sort_by(|a, b| a.a.y.cmp(&b.a.y));
        self.commit_graph = Some(commit_graph);
    }

    pub(super) fn search_finished(&mut self, id: Option<ObjectId>) {
        self.next_selected_commit = match id {
            Some(v) => SelectedItem::Object(v),
            None => SelectedItem::None,
        }
    }

    pub(super) fn reset(&mut self) {
        self.repo_state = Default::default();
        self.commit_graph = Default::default();
        self.selected_commit = Default::default();
    }

    pub(super) fn selected_commit(&self) -> &SelectedItem {
        &self.selected_commit
    }

    fn selected_commit_as_obj_id(&self) -> Option<&ObjectId> {
        match &self.selected_commit {
            SelectedItem::Object(v) => Some(v),
            SelectedItem::None | SelectedItem::Index => None,
        }
    }

    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        commit_cache: &Cache<ObjectId, Commit>,
        clipboard: &mut ClipboardContext,
    ) -> Vec<CommitLogAction> {
        let search_action = SearchBar::new(&mut self.search_string).show(ui);

        let commit_graph = match &self.commit_graph {
            Some(v) => v,
            None => return Vec::new(),
        };

        if commit_graph.nodes.is_empty() {
            return Vec::new();
        }

        let mut actions = Vec::new();
        match search_action {
            SearchAction::Next => actions.push(generate_search_next(
                commit_graph,
                self.selected_commit_as_obj_id(),
                self.search_string.clone(),
            )),
            SearchAction::Prev => actions.push(generate_search_prev(
                commit_graph,
                self.selected_commit_as_obj_id(),
                self.search_string.clone(),
            )),
            _ => (),
        };

        let text_style = TextStyle::Body;
        let row_height = ui.text_style_height(&text_style);

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(
                ui,
                row_height,
                commit_graph.nodes.len() + 1,
                |ui, mut row_range| {
                    if row_range.start == 0 {
                        let converter = PositionConverter::new(ui, row_height, 0..0);

                        render_commit_node(ui, &GraphPoint { x: 0, y: 0 }, &converter, false);
                        render_commit_node(ui, &GraphPoint { x: 0, y: 1 }, &converter, false);

                        ui.allocate_ui_at_rect(converter.text_rect(0), |ui| {
                            let index_selected = self.selected_commit == SelectedItem::Index;

                            let index_message_response =
                                render_commit_message(ui, "Index", index_selected);
                            if index_message_response.clicked() {
                                self.selected_commit = SelectedItem::Index;
                            }
                        });
                    }

                    row_range.start = row_range.start.saturating_sub(1);
                    row_range.end = row_range.end.saturating_sub(1);

                    Frame::none().show(ui, |ui| {
                        render_commit_graph(
                            row_range,
                            commit_graph,
                            ui,
                            row_height,
                            &mut self.selected_commit,
                            &mut self.next_selected_commit,
                            commit_cache,
                            &self.repo_state,
                            &mut actions,
                            clipboard,
                        );
                    });
                },
            );

        actions
    }
}
