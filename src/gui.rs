use crate::{
    app::{AppEvent, AppRequest},
    git::{Commit, HistoryGraph, ObjectId},
};

use anyhow::{Context, Result};
use eframe::{
    egui::{self, Pos2, Widget},
    epi,
};
use log::{debug, error};

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    sync::mpsc::{Receiver, Sender},
};

pub struct Gui {
    tx: Sender<AppRequest>,
    rx: Receiver<AppEvent>,

    output: Vec<String>,
    git_command: String,
    commit_graph: Option<HistoryGraph>,
    show_console: bool,
    outgoing_requests: HashSet<ObjectId>,
    cached_commits: HashMap<ObjectId, Commit>,
    cached_commit_order: VecDeque<ObjectId>,
}

impl Gui {
    const MAX_CACHED_COMMITS: usize = 1000;

    pub fn new(tx: Sender<AppRequest>, rx: Receiver<AppEvent>) -> Gui {
        Gui {
            tx,
            rx,
            output: Vec::new(),
            git_command: String::new(),
            commit_graph: None,
            show_console: false,
            outgoing_requests: HashSet::new(),
            cached_commits: HashMap::new(),
            cached_commit_order: VecDeque::new(),
        }
    }

    fn handle_event(&mut self, response: AppEvent) {
        match response {
            AppEvent::CommandExecuted(s) => {
                // FIXME: Rolling buffer
                self.output.push(s);
            }
            AppEvent::CommitFetched(commit) => {
                if self.cached_commit_order.len() >= Self::MAX_CACHED_COMMITS {
                    let popped = self.cached_commit_order.pop_front().unwrap();
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

    fn process_events(&mut self) -> bool {
        let mut event_processed = false;
        while let Ok(response) = self.rx.try_recv() {
            self.handle_event(response);
            event_processed = true;
        }
        event_processed
    }
}

impl epi::App for Gui {
    fn name(&self) -> &str {
        "Git"
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &epi::Frame) {
        let res = (|| -> Result<()> {
            if self.process_events() {
                ctx.request_repaint();
            }

            egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
                render_toolbar(ui, &self.tx, &mut self.show_console);
            });

            if self.show_console {
                egui::TopBottomPanel::bottom("git_log").show(ctx, |ui| {
                    render_console(ui, &self.output, &self.tx, &mut self.git_command)
                });
            }

            let missing_commits = egui::CentralPanel::default()
                .show(ctx, |ui| -> Vec<ObjectId> {
                    let text_style = egui::TextStyle::Body;
                    let row_height = ui.text_style_height(&text_style);
                    let commit_graph = match self.commit_graph.as_ref() {
                        Some(v) => v,
                        None => return Vec::new(),
                    };

                    if commit_graph.nodes.is_empty() {
                        return Vec::new();
                    }

                    egui::ScrollArea::vertical()
                        .auto_shrink([false, true])
                        .show_rows(ui, row_height, commit_graph.nodes.len(), |ui, row_range| {
                            render_commit_log(
                                ui,
                                row_range,
                                row_height,
                                commit_graph,
                                &self.cached_commits,
                            )
                        })
                        .inner
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
        })();

        if let Err(e) = res {
            // FIXME: Ratelimit
            error!("{:?}", e);
        }
    }
}

fn render_toolbar(ui: &mut egui::Ui, tx: &Sender<AppRequest>, show_console: &mut bool) {
    ui.horizontal(|ui| {
        let response = ui.button("Open repo");
        if response.clicked() {
            let repo = rfd::FileDialog::new().pick_folder();
            if let Some(repo) = repo {
                tx.send(AppRequest::OpenRepo(repo))
                    .expect("Failed to issue request to open repo");
            }
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
}

fn render_console(
    ui: &mut egui::Ui,
    output: &[String],
    tx: &Sender<AppRequest>,
    git_command: &mut String,
) {
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

    if response.has_focus() && ui.input().key_pressed(egui::Key::Enter) {
        let cmd = std::mem::take(git_command);
        tx.send(AppRequest::ExecuteGitCommand(cmd))
            .expect("Failed to request git command");
    }
}

fn render_commit_log(
    ui: &mut egui::Ui,
    row_range: Range<usize>,
    row_height: f32,
    commit_graph: &HistoryGraph,
    commit_lookup: &HashMap<ObjectId, Commit>,
) -> Vec<ObjectId> {
    let mut missing = Vec::new();

    let stroke = ui.style().visuals.widgets.open.fg_stroke;
    const X_MULTIPLIER: f32 = 10.0;
    let x_offset = 10.0;

    let mut max_edge_x = 0;
    let edge_end_idx = match commit_graph
        .edges
        .binary_search_by(|elem| (elem.a.y as usize).cmp(&row_range.end))
    {
        Ok(v) => v,
        Err(v) => v,
    };

    let y_spacing = ui.style().spacing.item_spacing.y;

    let x_idx_to_x_pos = |idx| idx as f32 * X_MULTIPLIER + x_offset;
    let y_idx_to_y_pos = |idx| idx as f32 * (row_height + y_spacing) + row_height / 2.0;
    for edge in &commit_graph.edges[..edge_end_idx] {
        // FIXME: as usize
        // FIXME: Filtering every frame is expensive
        if (edge.b.y as usize) < row_range.start || (edge.a.y as usize) > row_range.end {
            continue;
        }

        let a = Pos2 {
            x: x_idx_to_x_pos(edge.a.x),
            y: y_idx_to_y_pos(edge.a.y as f32 - row_range.start as f32),
        };
        let b = Pos2 {
            x: x_idx_to_x_pos(edge.b.x),
            y: y_idx_to_y_pos(edge.b.y as f32 - row_range.start as f32),
        };
        ui.painter().line_segment([a, b], stroke);
        let edge_end = i32::max(edge.a.x, edge.b.x);
        max_edge_x = i32::max(edge_end, max_edge_x);
    }

    let text_rect = egui::Rect::from_min_max(
        Pos2::new(
            x_idx_to_x_pos(max_edge_x) + ui.spacing().item_spacing.x,
            0.0,
        ),
        Pos2::new(ui.available_width(), ui.available_height()),
    );
    let mut text_ui = ui.child_ui(text_rect, egui::Layout::default());

    for (idx, node) in commit_graph.nodes[row_range].iter().enumerate() {
        let node_pos = Pos2 {
            x: x_idx_to_x_pos(node.position.x),
            y: y_idx_to_y_pos(idx as f32),
        };
        ui.painter().circle_filled(node_pos, 3.0, stroke.color);

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

        egui::Label::new(&message).wrap(false).ui(&mut text_ui);
    }

    missing
}
