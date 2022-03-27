use crate::{
    app::{AppEvent, AppRequest},
    git::{
        graph::{Edge, GraphPoint},
        Branch, Commit, HistoryGraph, ObjectId,
    },
};

use anyhow::{Context, Result};
use eframe::{
    egui::{self, Pos2, Rect, Response, ScrollArea, Sense, TextEdit, TextStyle, Ui, Widget},
    epi,
};
use log::{debug, error};

use std::{
    collections::{HashMap, HashSet, VecDeque},
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
    commit_graph: Option<HistoryGraph>,
    show_console: bool,
    outgoing_requests: HashSet<ObjectId>,
    branches: Vec<Branch>,
    selected_branches: Vec<usize>,
    cached_commits: HashMap<ObjectId, Commit>,
    cached_commit_order: VecDeque<ObjectId>,
    selected_commit: Option<ObjectId>,
}

impl GuiInner {
    const MAX_CACHED_COMMITS: usize = 1000;

    fn new(tx: Sender<AppRequest>) -> GuiInner {
        GuiInner {
            tx,
            output: Vec::new(),
            git_command: String::new(),
            commit_graph: None,
            show_console: false,
            outgoing_requests: HashSet::new(),
            branches: Vec::new(),
            selected_branches: Vec::new(),
            cached_commits: HashMap::new(),
            cached_commit_order: VecDeque::new(),
            selected_commit: None,
        }
    }

    fn reset(&mut self) {
        self.git_command = String::new();
        self.commit_graph = None;
        self.outgoing_requests = HashSet::new();
        self.branches = Vec::new();
        self.cached_commits = HashMap::new();
        self.cached_commit_order = VecDeque::new();
        self.selected_commit = None;
        self.selected_branches = Vec::new();
    }

    fn handle_event(&mut self, response: AppEvent) {
        match response {
            AppEvent::CommandExecuted(s) => {
                // FIXME: Rolling buffer
                self.output.push(s);
            }
            AppEvent::CommitFetched(commit) => {
                if self.cached_commit_order.len() >= Self::MAX_CACHED_COMMITS {
                    let mut popped = self.cached_commit_order.pop_front().unwrap();
                    if Some(&popped) == self.selected_commit.as_ref() {
                        // Ensure that the selected commit stays in the queue
                        popped = self.cached_commit_order.pop_front().unwrap();
                        self.cached_commit_order
                            .push_back(self.selected_commit.clone().unwrap());
                    }
                    self.cached_commits.remove(&popped);
                    debug!("Clearing commit {}", popped);
                }

                debug!("Received commit {}", commit.metadata.id);
                self.outgoing_requests.remove(&commit.metadata.id);
                self.cached_commit_order
                    .push_back(commit.metadata.id.clone());
                self.cached_commits
                    .insert(commit.metadata.id.clone(), commit);
            }
            AppEvent::BranchesUpdated(branches) => {
                self.branches = branches;
                self.selected_branches = vec![0];
            }
            AppEvent::Error(e) => {
                // FIXME: Proper error text
                self.output.push(e);
            }
            AppEvent::CommitLogProcessed(mut graph) => {
                graph.edges.sort_by(|a, b| a.a.y.cmp(&b.a.y));
                self.commit_graph = Some(graph);
            }
        }
    }

    fn open_repo(&mut self, repo: PathBuf) {
        self.reset();
        self.tx
            .send(AppRequest::OpenRepo(repo))
            .expect("App handle invalid");
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
                let cmd = std::mem::take(&mut self.git_command);
                self.tx
                    .send(AppRequest::ExecuteGitCommand(cmd))
                    .expect("Failed to request git command");
            }
        }

        egui::TopBottomPanel::bottom("commit_view_panel")
            .default_height(150.0)
            .resizable(true)
            .show(ctx, |ui| {
                render_commit_view(ui, &self.cached_commits, self.selected_commit.as_ref());
            });

        let selected_branches = egui::SidePanel::right("sidebar")
            .resizable(true)
            .show(ctx, |ui| {
                render_side_panel(ui, &self.branches, &self.selected_branches)
            })
            .inner;

        if self.selected_branches != selected_branches {
            self.selected_branches = selected_branches;
            let selected_branches = self
                .selected_branches
                .iter()
                .map(|idx| self.branches[*idx].clone())
                .collect();
            self.tx
                .send(AppRequest::SelectBranches(selected_branches))
                .expect("Failed to request branch selection");
        };

        let missing_commits = egui::CentralPanel::default()
            .show(ctx, |ui| -> Vec<ObjectId> {
                commit_log::render(
                    ui,
                    &self.commit_graph,
                    &self.cached_commits,
                    &mut self.selected_commit,
                )
            })
            .inner;

        for id in missing_commits {
            if !self.outgoing_requests.contains(&id) {
                debug!("Requesting commit {}", id);

                self.tx
                    .send(AppRequest::GetCommit(id.clone()))
                    .context("Failed to request commit")?;

                self.outgoing_requests.insert(id);
            }
        }

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
    cached_commits: &HashMap<ObjectId, Commit>,
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

fn render_side_panel(ui: &mut Ui, branches: &[Branch], selected_branches: &[usize]) -> Vec<usize> {
    let mut ret = Vec::new();
    ScrollArea::vertical()
        .auto_shrink([true, false])
        .show(ui, |ui| {
            for (idx, branch) in branches.iter().enumerate() {
                let mut selected = selected_branches.contains(&idx);
                ui.checkbox(&mut selected, &branch.name);
                if selected {
                    ret.push(idx);
                }
            }
        });

    ret
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
            // FIXME: as usize
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
        commit_graph: &Option<HistoryGraph>,
        commit_lookup: &HashMap<ObjectId, Commit>,
        selected_commit: &mut Option<ObjectId>,
    ) -> Vec<ObjectId> {
        let commit_graph = match commit_graph.as_ref() {
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

                    let message = match commit_lookup.get(&node.id) {
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
