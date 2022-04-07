mod tristate_checkbox;

use tristate_checkbox::TristateCheckbox;

use crate::{
    app::{AppEvent, AppRequest, CheckoutItem, RepoState, ViewState},
    git::{
        graph::{Edge, GraphPoint},
        Commit, Diff, DiffContent, HistoryGraph, ObjectId, ReferenceId,
    },
    util::Cache,
};

use anyhow::{Context, Error, Result};
use clipboard::{ClipboardContext, ClipboardProvider};
use eframe::{
    egui::{
        self, text::LayoutJob, Align, Color32, Layout, Pos2, Rect, Response, RichText, ScrollArea,
        Sense, Stroke, TextEdit, TextFormat, TextStyle, Ui, Widget,
    },
    epi,
};
use log::{debug, error, warn};

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::PathBuf,
    sync::{
        mpsc::{Receiver, Sender},
        Arc, Mutex,
    },
};

struct GuiInner {
    tx: Sender<AppRequest>,
    output: Vec<String>,
    git_command: String,
    show_console: bool,
    outgoing_requests: HashSet<ObjectId>,
    repo_state: RepoState,
    view_state: ViewState,
    pending_view_state: ViewState,
    last_requsted_view_state: ViewState,
    commit_graph: Option<HistoryGraph>,
    commit_cache: Cache<ObjectId, Commit>,
    selected_commit: Option<ObjectId>,
    last_requested_diff: Option<ObjectId>,
    current_diff: Option<Diff>,
    clipboard: ClipboardContext,
}

impl GuiInner {
    const MAX_CACHED_COMMITS: usize = 1000;

    fn new(tx: Sender<AppRequest>) -> Result<GuiInner> {
        Ok(GuiInner {
            tx,
            output: Vec::new(),
            git_command: String::new(),
            show_console: true,
            outgoing_requests: HashSet::new(),
            repo_state: Default::default(),
            view_state: Default::default(),
            pending_view_state: Default::default(),
            last_requsted_view_state: Default::default(),
            commit_graph: Default::default(),
            commit_cache: Cache::new(Self::MAX_CACHED_COMMITS),
            selected_commit: None,
            last_requested_diff: None,
            current_diff: None,
            clipboard: ClipboardContext::new()
                .map_err(|_| Error::msg("Failed to construct clipboard"))?,
        })
    }

    fn reset(&mut self) {
        self.git_command = String::new();
        self.outgoing_requests = HashSet::new();
        self.repo_state = Default::default();
        self.view_state = Default::default();
        self.pending_view_state = Default::default();
        self.last_requsted_view_state = Default::default();
        self.commit_graph = Default::default();
        self.commit_cache = Cache::new(Self::MAX_CACHED_COMMITS);
        self.selected_commit = None;
        self.last_requested_diff = None;
        self.current_diff = None;
    }

    fn handle_event(&mut self, response: AppEvent) {
        match response {
            AppEvent::OutputLogged(s) => {
                // FIXME: Rolling buffer
                self.output.push(s);
            }
            AppEvent::CommitFetched { repo, commit } => {
                let current_repo_is_same = self.repo_state.repo == repo;
                if current_repo_is_same {
                    self.outgoing_requests.remove(&commit.metadata.id);
                    self.commit_cache.push(commit.metadata.id.clone(), commit);
                } else {
                    warn!("Dropping commit in gui: {}", commit.metadata.id);
                }
            }
            AppEvent::DiffFetched { repo, diff } => {
                if self.repo_state.repo == repo {
                    self.current_diff = Some(diff);
                }
            }
            AppEvent::CommitGraphFetched(view_state, mut graph) => {
                self.view_state = view_state;

                // Sort the start positions in increasing order
                graph.edges.sort_by(|a, b| a.a.y.cmp(&b.a.y));

                self.commit_graph = Some(graph);
            }
            AppEvent::RepoStateUpdated(repo_state) => {
                if self.repo_state.repo != repo_state.repo {
                    self.reset();
                    self.pending_view_state.selected_references = vec![ReferenceId::head()];
                }

                self.pending_view_state.update_with_repo_state(&repo_state);
                self.view_state.update_with_repo_state(&repo_state);
                if self.repo_state != repo_state {
                    self.repo_state = repo_state;
                    // Reset requested view state to force a re-request
                    self.last_requsted_view_state = Default::default();
                }
            }
            AppEvent::Error(e) => {
                // FIXME: Proper error text
                self.output.push(e);
            }
        }
    }

    fn open_repo(&mut self, repo: PathBuf) {
        self.tx
            .send(AppRequest::OpenRepo(repo))
            .expect("App handle invalid");
    }

    fn send_current_git_command(&mut self) {
        let cmd = std::mem::take(&mut self.git_command);
        self.tx
            .send(AppRequest::ExecuteGitCommand(self.repo_state.clone(), cmd))
            .expect("Failed to request git command");
    }

    fn request_pending_view_state(&mut self) -> Result<()> {
        if self.pending_view_state != self.last_requsted_view_state {
            self.tx.send(AppRequest::GetCommitGraph {
                expected_repo: self.repo_state.repo.clone(),
                viewer_id: "GUI".into(),
                view_state: self.pending_view_state.clone(),
            })?;
            self.last_requsted_view_state = self.pending_view_state.clone();
        }
        Ok(())
    }

    fn request_commit(&mut self, id: ObjectId) -> Result<()> {
        if !self.outgoing_requests.contains(&id) {
            debug!("Requesting commit {}", id);

            self.tx
                .send(AppRequest::GetCommit {
                    expected_repo: self.repo_state.repo.clone(),
                    id: id.clone(),
                })
                .context("Failed to request commit")?;

            self.outgoing_requests.insert(id);
        }
        Ok(())
    }

    fn request_checkout(&mut self, item: CheckoutItem) -> Result<()> {
        self.tx
            .send(AppRequest::Checkout(self.repo_state.clone(), item))
            .context("Failed to send checkout request")?;

        Ok(())
    }

    fn handle_commit_log_actions(
        &mut self,
        actions: Vec<commit_log::CommitLogAction>,
    ) -> Result<()> {
        for action in actions {
            match action {
                commit_log::CommitLogAction::FetchCommit(id) => {
                    self.request_commit(id)?;
                }
                commit_log::CommitLogAction::CheckoutObject(id) => {
                    self.request_checkout(CheckoutItem::Object(id))?;
                }
            }
        }
        Ok(())
    }

    fn request_missing_diff(&mut self) -> Result<()> {
        let selected_commit = match &self.selected_commit {
            Some(v) => v,
            None => return Ok(()),
        };

        if Some(selected_commit) == self.last_requested_diff.as_ref() {
            return Ok(());
        }

        let commit = match self.commit_cache.get(selected_commit) {
            Some(v) => v,
            None => return Ok(()),
        };

        let parent = match commit.metadata.parents.get(0) {
            // FIXME: Choose which parent to diff to
            // FIXME: Support initial commit
            Some(v) => v,
            None => return Ok(()),
        };

        self.tx.send(AppRequest::GetDiff {
            expected_repo: self.repo_state.repo.clone(),
            from: parent.clone(),
            to: commit.metadata.id.clone(),
        })?;

        self.last_requested_diff = self.selected_commit.clone();

        Ok(())
    }

    fn update(&mut self, ctx: &egui::Context) -> Result<()> {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            if let Some(repo) = render_toolbar(ui, &mut self.show_console) {
                self.open_repo(repo);
            }
        });

        if self.show_console {
            let send_git_command = egui::TopBottomPanel::bottom("output")
                .resizable(true)
                .default_height(250.0)
                .min_height(100.0)
                .show(ctx, |ui| {
                    render_console(ui, &self.output, &mut self.git_command)
                })
                .inner;

            if send_git_command {
                self.send_current_git_command();
            }
        }

        egui::TopBottomPanel::bottom("commit_view_panel")
            .default_height(ctx.available_rect().height() / 2.0)
            .resizable(true)
            .min_height(100.0)
            .show(ctx, |ui| {
                render_commit_view(
                    ui,
                    &self.commit_cache,
                    self.selected_commit.as_ref(),
                    self.current_diff.as_ref(),
                );
            });

        let sidebar_action = egui::SidePanel::right("sidebar")
            .resizable(true)
            .show(ctx, |ui| {
                render_side_panel(
                    ui,
                    &self.repo_state,
                    &self.view_state,
                    &mut self.pending_view_state,
                    &mut self.clipboard,
                )
            })
            .inner;

        let commit_log_actions = egui::CentralPanel::default()
            .show(ctx, |ui| -> Vec<commit_log::CommitLogAction> {
                commit_log::render(
                    ui,
                    &self.repo_state,
                    self.commit_graph.as_ref(),
                    &self.commit_cache,
                    &mut self.selected_commit,
                    &mut self.clipboard,
                )
            })
            .inner;

        match sidebar_action {
            SidebarAction::Checkout(id) => {
                self.request_checkout(CheckoutItem::Reference(id))?;
            }
            SidebarAction::Delete(id) => {
                self.tx
                    .send(AppRequest::Delete(self.repo_state.clone(), id))
                    .context("Failed to send delete request")?;
            }
            SidebarAction::None => (),
        }

        self.request_missing_diff()?;
        self.handle_commit_log_actions(commit_log_actions)?;
        self.request_pending_view_state()?;

        Ok(())
    }
}

pub struct Gui {
    rx: Option<Receiver<AppEvent>>,
    inner: Arc<Mutex<GuiInner>>,
}

impl Gui {
    pub fn new(tx: Sender<AppRequest>, rx: Receiver<AppEvent>) -> Result<Gui> {
        let inner = GuiInner::new(tx)?;
        Ok(Gui {
            rx: Some(rx),
            inner: Arc::new(Mutex::new(inner)),
        })
    }
}

impl epi::App for Gui {
    fn name(&self) -> &str {
        "Spit"
    }

    fn setup(
        &mut self,
        ctx: &egui::Context,
        frame: &epi::Frame,
        _storage: Option<&dyn epi::Storage>,
    ) {
        // Colors are unreadable in light mode, force dark mode for now
        ctx.set_visuals(egui::Visuals::dark());
        // We need to spawn a thread to process events
        let inner = Arc::clone(&self.inner);
        let frame = frame.clone();
        let rx = std::mem::take(&mut self.rx).expect("Setup called with uninitialized rx");
        std::thread::spawn(move || {
            while let Ok(response) = rx.recv() {
                let mut inner = inner.lock().unwrap();
                inner.handle_event(response);
                frame.request_repaint();
            }
        });
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &epi::Frame) {
        let mut inner = self.inner.lock().unwrap();
        let res = inner.update(ctx);

        if let Err(e) = res {
            // FIXME: Ratelimit
            error!("{:?}", e);
        }
    }
}

fn render_commit_view(
    ui: &mut Ui,
    cached_commits: &Cache<ObjectId, Commit>,
    selected_commit: Option<&ObjectId>,
    current_diff: Option<&Diff>,
) {
    let mut message = selected_commit
        .and_then(|id| cached_commits.get(id))
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
        .unwrap_or_else(String::new);

    if let Some(diff) = current_diff {
        if Some(&diff.to) == selected_commit {
            for (file, content) in &diff.items {
                message.push('\n');
                message.push_str(&file.to_string());
                message.push('\n');
                match content {
                    DiffContent::Patch(hunks) => {
                        for (hunk, content) in hunks {
                            message.push_str(hunk);
                            message.push('\n');
                            message.push_str(
                                std::str::from_utf8(content)
                                    .unwrap_or("Patch content is not valid utf8"),
                            );
                        }
                    }
                    DiffContent::Binary => {
                        message.push_str("Binary content changed\n");
                    }
                }
            }
        }
    }

    egui::ScrollArea::vertical()
        .id_source("commit_view")
        .max_height(f32::INFINITY)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            TextEdit::multiline(&mut message.as_str())
                .font(TextStyle::Monospace)
                .desired_width(ui.available_width())
                .ui(ui);
        });
}

fn render_toolbar(ui: &mut egui::Ui, show_console: &mut bool) -> Option<PathBuf> {
    let mut ret = None;
    ui.horizontal(|ui| {
        let response = ui.button("Open repo");
        if response.clicked() {
            let repo = rfd::FileDialog::new().pick_folder();
            ret = repo;
        }

        let button_text = if *show_console {
            "Hide console"
        } else {
            "Show console"
        };

        let response = ui.button(button_text);
        if response.clicked() {
            *show_console = !*show_console;
        }
    });
    ret
}

fn render_console(ui: &mut egui::Ui, output: &[String], git_command: &mut String) -> bool {
    // UI management...
    // As far as I can tell, ScrollArea is going to take up the remaining spcace if I do not set
    // auto_shrink to true, however I want auto_shrink to be false or else I cannot resize the pane
    // we are in
    //
    // As far as I can tell the scroll area needs to be laid out top to bottom or else the scroll
    // area will be extremely long and mostly empty
    //
    // From the above two constraints...
    // * Layout backwards so that we can put our text entry at the bottom and have the ui
    // automatically track remaining space
    // * Layout forwards from within the backwards layout to get the scroll area to work right
    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
        let response = egui::TextEdit::multiline(git_command)
            .id_source("git_command")
            .desired_rows(1)
            .desired_width(ui.available_width())
            .font(TextStyle::Monospace)
            .show(ui)
            .response;

        ui.with_layout(Layout::default(), |ui| {
            egui::ScrollArea::vertical()
                .id_source("console")
                .auto_shrink([false, false])
                .stick_to_bottom()
                .show(ui, |ui| {
                    for s in output {
                        ui.monospace(s);
                    }
                });
        });

        response.has_focus() && ui.input().key_pressed(egui::Key::Enter)
    })
    .inner
}

enum SidebarAction {
    Checkout(ReferenceId),
    Delete(ReferenceId),
    None,
}

fn render_side_panel(
    ui: &mut Ui,
    repo_state: &RepoState,
    view_state: &ViewState,
    pending_view_state: &mut ViewState,
    clipboard: &mut ClipboardContext,
) -> SidebarAction {
    let mut new_selected = Vec::with_capacity(pending_view_state.selected_references.len());
    let mut action = SidebarAction::None;

    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for branch in repo_state.branches.iter() {
                let real_state = view_state.selected_references.contains(&branch.id);
                let mut selected = pending_view_state.selected_references.contains(&branch.id);

                let text = reference_richtext(&branch.id, repo_state);

                let response = TristateCheckbox::new(&real_state, &mut selected, text).ui(ui);
                response.context_menu(|ui| {
                    if ui.button("Copy").clicked() {
                        try_set_clipboard(clipboard, branch.id.to_string());
                        ui.close_menu();
                    }

                    if ui.button("Checkout").clicked() {
                        action = SidebarAction::Checkout(branch.id.clone());
                        ui.close_menu();
                    }

                    ui.separator();

                    if ui.button("Delete").clicked() {
                        action = SidebarAction::Delete(branch.id.clone());
                        ui.close_menu();
                    }
                });
                if selected {
                    new_selected.push(branch.id.clone());
                }
            }
        });

    pending_view_state.selected_references = new_selected;
    action
}

fn reference_richtext(id: &ReferenceId, repo_state: &RepoState) -> RichText {
    let color = reference_color(id);

    let text = RichText::new(id.to_string()).color(color);

    if reference_underline(id, repo_state) {
        text.underline()
    } else {
        text
    }
}

fn reference_underline(id: &ReferenceId, repo_state: &RepoState) -> bool {
    repo_state.head == *id
}

fn reference_color(id: &ReferenceId) -> Color32 {
    match id {
        ReferenceId::Symbolic(_) => Color32::LIGHT_BLUE,
        ReferenceId::LocalBranch(_) => Color32::LIGHT_GREEN,
        ReferenceId::RemoteBranch(_) => Color32::LIGHT_RED,
        ReferenceId::Unknown => Color32::RED,
    }
}

fn try_set_clipboard(clipboard: &mut ClipboardContext, s: String) {
    if let Err(e) = clipboard.set_contents(s) {
        error!("Failed to set clipboard contents: {}", e);
    }
}

mod commit_log {
    use super::*;

    /// Helper struct for calculating where ui elements should be drawn
    struct PositionConverter {
        row_height: f32,
        spacing: egui::Vec2,
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
            egui::Rect::from_min_max(
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
        let edge_end_idx =
            match edges.binary_search_by(|elem| (elem.a.y as usize).cmp(&row_range.end)) {
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

    fn render_commit_message(ui: &mut Ui, message: LayoutJob, selected: bool) -> Response {
        // Would be nice to use SeletableLabel, but I couldn't find a way to prevent it from
        // wrapping
        let (pos, galley, message_response) = egui::Label::new(message)
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

        galley.paint_with_visuals(ui.painter(), pos, ui.visuals().noninteractive());

        message_response
    }

    fn render_commit_node(ui: &mut Ui, node_pos: &GraphPoint, converter: &PositionConverter) {
        let stroke = ui.style().visuals.widgets.open.fg_stroke;
        let node_pos = Pos2 {
            x: converter.graph_x_to_ui_x(node_pos.x),
            y: converter.graph_y_to_ui_y(node_pos.y),
        };
        ui.painter().circle_filled(node_pos, 3.0, stroke.color);
    }

    fn build_branch_id_lookup(state: &RepoState) -> HashMap<ObjectId, Vec<ReferenceId>> {
        let mut ret = HashMap::new();
        for branch in &state.branches {
            let entry = ret.entry(branch.head.clone()).or_insert(vec![]);
            entry.push(branch.id.clone());
        }
        ret
    }

    pub(super) enum CommitLogAction {
        FetchCommit(ObjectId),
        CheckoutObject(ObjectId),
    }

    pub(super) fn render(
        ui: &mut Ui,
        repo_state: &RepoState,
        commit_graph: Option<&HistoryGraph>,
        commit_cache: &Cache<ObjectId, Commit>,
        selected_commit: &mut Option<ObjectId>,
        clipboard: &mut ClipboardContext,
    ) -> Vec<CommitLogAction> {
        let commit_graph = match commit_graph {
            Some(v) => v,
            None => return Vec::new(),
        };

        if commit_graph.nodes.is_empty() {
            return Vec::new();
        }

        let text_style = egui::TextStyle::Body;
        let row_height = ui.text_style_height(&text_style);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_height, commit_graph.nodes.len(), |ui, row_range| {
                if row_range.end > commit_graph.nodes.len()
                    || row_range.start > commit_graph.nodes.len()
                {
                    ui.scroll_to_cursor(None);
                    return Vec::new();
                }

                let converter = PositionConverter::new(ui, row_height, row_range.clone());

                let mut actions = Vec::new();

                let max_edge_x = render_edges(ui, &commit_graph.edges, &converter, &row_range);
                let text_rect = converter.text_rect(max_edge_x);
                let mut text_ui = ui.child_ui(text_rect, egui::Layout::default());

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
                    render_commit_node(ui, &node.position, &converter);

                    let mut job = LayoutJob::default();
                    let style = text_ui.style();
                    let font = style.text_styles[&TextStyle::Body].clone();

                    if let Some(ids) = branch_id_lookup.get(&node.id) {
                        for id in ids {
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

                    let selected = selected_commit.as_ref() == Some(&node.id);
                    let commit_message_response =
                        render_commit_message(&mut text_ui, job, selected);
                    if commit_message_response.clicked() {
                        *selected_commit = Some(node.id.clone());
                    }

                    commit_message_response.context_menu(|ui| {
                        if ui.button("Copy hash").clicked() {
                            try_set_clipboard(clipboard, node.id.to_string());
                            ui.close_menu();
                        }

                        if ui.button("Checkout").clicked() {
                            warn!("Unimplemented checkout");
                            actions.push(CommitLogAction::CheckoutObject(node.id.clone()));
                            ui.close_menu();
                        }
                    });
                }

                actions
            })
            .inner
    }
}
