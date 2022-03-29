use crate::{
    app::{AppEvent, AppRequest, RepoState, ViewState},
    git::{
        graph::{Edge, GraphPoint},
        Branch, BranchId, Commit, HistoryGraph, ObjectId,
    },
    util::Cache,
};

use anyhow::{Context, Result};
use eframe::{
    egui::{self, Pos2, Rect, Response, ScrollArea, Sense, TextEdit, TextStyle, Ui, Widget},
    epi,
};
use log::{debug, error, warn};

use std::{
    collections::HashSet,
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
}

impl GuiInner {
    const MAX_CACHED_COMMITS: usize = 1000;

    fn new(tx: Sender<AppRequest>) -> GuiInner {
        GuiInner {
            tx,
            output: Vec::new(),
            git_command: String::new(),
            show_console: false,
            outgoing_requests: HashSet::new(),
            repo_state: Default::default(),
            view_state: Default::default(),
            pending_view_state: Default::default(),
            last_requsted_view_state: Default::default(),
            commit_graph: Default::default(),
            commit_cache: Cache::new(Self::MAX_CACHED_COMMITS),
            selected_commit: None,
        }
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
    }

    fn handle_event(&mut self, response: AppEvent) {
        match response {
            AppEvent::CommandExecuted(s) => {
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
            AppEvent::CommitGraphFetched(view_state, mut graph) => {
                self.view_state = view_state;

                // Sort the start positions in increasing order
                graph.edges.sort_by(|a, b| a.a.y.cmp(&b.a.y));

                self.commit_graph = Some(graph);
            }
            AppEvent::RepoStateUpdated(repo_state) => {
                if self.repo_state.repo != repo_state.repo {
                    self.reset();
                    self.pending_view_state.selected_branches = vec![BranchId::Head];
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
                view_state: self.pending_view_state.clone(),
            })?;
            self.last_requsted_view_state = self.pending_view_state.clone();
        }
        Ok(())
    }

    fn request_missing_commits(&mut self, missing_commits: Vec<ObjectId>) -> Result<()> {
        for id in missing_commits {
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
        }
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
                .show(ctx, |ui| {
                    render_console(ui, &self.output, &mut self.git_command)
                })
                .inner;

            if send_git_command {
                self.send_current_git_command();
            }
        }

        egui::TopBottomPanel::bottom("commit_view_panel")
            .default_height(150.0)
            .resizable(true)
            .show(ctx, |ui| {
                render_commit_view(ui, &self.commit_cache, self.selected_commit.as_ref());
            });

        egui::SidePanel::right("sidebar")
            .resizable(true)
            .show(ctx, |ui| {
                render_side_panel(
                    ui,
                    &self.repo_state.branches,
                    &self.view_state,
                    &mut self.pending_view_state,
                )
            });

        let missing_commits = egui::CentralPanel::default()
            .show(ctx, |ui| -> Vec<ObjectId> {
                commit_log::render(
                    ui,
                    self.commit_graph.as_ref(),
                    &self.commit_cache,
                    &mut self.selected_commit,
                )
            })
            .inner;

        self.request_missing_commits(missing_commits)?;
        self.request_pending_view_state()?;

        Ok(())
    }
}

pub struct Gui {
    rx: Option<Receiver<AppEvent>>,
    inner: Arc<Mutex<GuiInner>>,
}

impl Gui {
    pub fn new(tx: Sender<AppRequest>, rx: Receiver<AppEvent>) -> Gui {
        let inner = GuiInner::new(tx);
        Gui {
            rx: Some(rx),
            inner: Arc::new(Mutex::new(inner)),
        }
    }
}

impl epi::App for Gui {
    fn name(&self) -> &str {
        "Spit"
    }

    fn setup(
        &mut self,
        _ctx: &egui::Context,
        frame: &epi::Frame,
        _storage: Option<&dyn epi::Storage>,
    ) {
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
) {
    let message = selected_commit
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

    egui::ScrollArea::vertical()
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
    egui::ScrollArea::vertical()
        .stick_to_bottom()
        .auto_shrink([false, true])
        .max_height(250.0)
        .show(ui, |ui| {
            for s in output {
                ui.monospace(s);
            }
        });

    let response = egui::TextEdit::multiline(git_command)
        .font(egui::TextStyle::Monospace)
        .ui(ui);

    response.has_focus() && ui.input().key_pressed(egui::Key::Enter)
}

fn render_side_panel(
    ui: &mut Ui,
    branches: &[Branch],
    _view_state: &ViewState,
    pending_view_state: &mut ViewState,
) {
    let mut new_selected = Vec::with_capacity(pending_view_state.selected_branches.len());

    ScrollArea::vertical()
        .auto_shrink([true, false])
        .show(ui, |ui| {
            for branch in branches.iter() {
                let mut selected = pending_view_state.selected_branches.contains(&branch.id);

                ui.checkbox(&mut selected, &branch.id.to_string());
                if selected {
                    new_selected.push(branch.id.clone());
                }
            }
        });

    pending_view_state.selected_branches = new_selected;
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

    fn render_commit_message(ui: &mut Ui, message: &str, selected: bool) -> Response {
        // Would be nice to use SeletableLable, but I couldn't find a way to prevent it from
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

    pub(super) fn render(
        ui: &mut Ui,
        commit_graph: Option<&HistoryGraph>,
        commit_cache: &Cache<ObjectId, Commit>,
        selected_commit: &mut Option<ObjectId>,
    ) -> Vec<ObjectId> {
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

                let mut missing = Vec::new();

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

                for node in &commit_graph.nodes[row_range] {
                    render_commit_node(ui, &node.position, &converter);

                    let message = match commit_cache.get(&node.id) {
                        Some(v) => v
                            .message
                            .split('\n')
                            .next()
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| v.message.clone()),
                        None => {
                            missing.push(node.id.clone());
                            node.id.to_string()
                        }
                    };

                    let selected = selected_commit.as_ref() == Some(&node.id);
                    if render_commit_message(&mut text_ui, &message, selected).clicked() {
                        *selected_commit = Some(node.id.clone());
                    }
                }

                missing
            })
            .inner
    }
}
