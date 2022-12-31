mod commit_log;
mod commit_view;
mod download_dialog;
mod editor;
mod sidebar;
mod tristate_checkbox;

pub use editor::Editor;

use commit_log::CommitLog;
use commit_view::{CommitView, CommitViewAction};
use download_dialog::DownloadDialog;
use sidebar::{Sidebar, SidebarAction};

use crate::{
    app::{AppEvent, AppRequest, RepoState, ViewState},
    git::{Commit, Identifier, ObjectId, ReferenceId},
    util::Cache,
};

use anyhow::{Context, Error, Result};
use clipboard::{ClipboardContext, ClipboardProvider};
use eframe::{
    egui::{self, Align, Color32, Layout, RichText, ScrollArea, TextEdit, TextStyle, Ui},
    App, CreationContext,
};
use log::{debug, error, warn};

use std::{
    collections::HashSet,
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
    repo_state: Arc<RepoState>,
    view_state: ViewState,
    pending_view_state: ViewState,
    last_requsted_view_state: ViewState,
    commit_cache: Cache<ObjectId, Commit>,
    commit_view: CommitView,
    commit_log: CommitLog,
    sidebar: Sidebar,
    download_dialog: DownloadDialog,
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
            commit_cache: Cache::new(Self::MAX_CACHED_COMMITS),
            commit_view: CommitView::new(),
            commit_log: Default::default(),
            sidebar: Sidebar::new(),
            download_dialog: DownloadDialog::new(),
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
        self.commit_cache = Cache::new(Self::MAX_CACHED_COMMITS);
        self.download_dialog.reset();
        self.commit_view.reset();
        self.commit_log.reset();
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
                    self.commit_view.update_diff(diff);
                }
            }
            AppEvent::CommitGraphFetched(view_state, graph) => {
                self.view_state = view_state;
                self.commit_log.update_graph(graph);
            }
            AppEvent::SearchFinished {
                viewer_id: _,
                matched_id,
            } => {
                self.commit_log.search_finished(matched_id);
            }
            AppEvent::RepoStateUpdated(repo_state) => {
                if self.repo_state.repo != repo_state.repo {
                    self.reset();
                    self.pending_view_state.selected_references =
                        FromIterator::from_iter([ReferenceId::head()]);
                }

                let repo_state = Arc::new(repo_state);
                self.pending_view_state.update_with_repo_state(&repo_state);
                self.view_state.update_with_repo_state(&repo_state);
                self.sidebar.update_with_repo_state(Arc::clone(&repo_state));
                self.commit_log
                    .update_with_repo_state(Arc::clone(&repo_state));

                if repo_state.repo != self.repo_state.repo {
                    self.download_dialog.update_remote_state(Default::default());
                }

                if *self.repo_state != *repo_state {
                    self.repo_state = repo_state;
                    // Reset requested view state to force a re-request
                    self.last_requsted_view_state = Default::default();
                }
            }
            AppEvent::RemoteStateUpdated(remote_state) => {
                if remote_state.repo == self.repo_state.repo {
                    self.download_dialog.update_remote_state(remote_state);
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
            .send(AppRequest::ExecuteGitCommand(
                (*self.repo_state).clone(),
                cmd,
            ))
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

    fn request_checkout(&mut self, id: Identifier) -> Result<()> {
        self.tx
            .send(AppRequest::Checkout((*self.repo_state).clone(), id))
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
                commit_log::CommitLogAction::Checkout(id) => {
                    self.request_checkout(id)?;
                }
                commit_log::CommitLogAction::DeleteReference(id) => {
                    self.tx
                        .send(AppRequest::Delete((*self.repo_state).clone(), id))
                        .context("Failed to send delete request")?;
                }
                commit_log::CommitLogAction::CherryPick(id) => {
                    self.tx
                        .send(AppRequest::CherryPick((*self.repo_state).clone(), id))
                        .context("Failed to send delete request")?;
                }
                commit_log::CommitLogAction::Diff(id) => {
                    self.tx
                        .send(AppRequest::Diff(id))
                        .context("Failed to send delete request")?;
                }
                commit_log::CommitLogAction::Merge(id) => {
                    self.tx
                        .send(AppRequest::Merge((*self.repo_state).clone(), id))
                        .context("Failed to send delete request")?;
                }
                commit_log::CommitLogAction::Append(s) => {
                    self.git_command.push_str(&s);
                }
                commit_log::CommitLogAction::Search {
                    commit_list,
                    search_string,
                } => {
                    self.tx
                        .send(AppRequest::Search {
                            expected_repo: self.repo_state.repo.clone(),
                            viewer_id: "GUI".into(),
                            search_string,
                            commit_list,
                        })
                        .context("Failed to request search")?;
                }
            }
        }

        Ok(())
    }

    fn ensure_selected_commit_in_cache(&mut self) -> Result<()> {
        let selected_commit = match self.commit_log.selected_commit() {
            Some(v) => v,
            None => return Ok(()),
        };

        self.commit_cache.pin(selected_commit.clone());

        if self.commit_cache.get(selected_commit).is_some() {
            return Ok(());
        }

        let selected_commit = selected_commit.clone();
        self.request_commit(selected_commit)
            .context("Failed to request selected commit")?;

        Ok(())
    }

    fn handle_commit_view_action(&mut self, action: CommitViewAction) -> Result<()> {
        match action {
            CommitViewAction::RequestDiff(diff_request) => {
                self.tx.send(AppRequest::GetDiff {
                    expected_repo: self.repo_state.repo.clone(),
                    from: diff_request.from,
                    to: diff_request.to,
                    ignore_whitespace: diff_request.ignore_whitespace,
                })?;
            }
            CommitViewAction::None => (),
        }

        Ok(())
    }

    fn update(&mut self, ctx: &egui::Context) -> Result<()> {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            match render_toolbar(ui, &mut self.show_console) {
                ToolbarAction::OpenRepo(repo) => {
                    self.open_repo(repo);
                }
                ToolbarAction::ShowDownloadDialog => {
                    self.download_dialog.set_open(true);
                    if let Err(e) = self.tx.send(AppRequest::UpdateRemotes {
                        expected_repo: self.repo_state.repo.clone(),
                    }) {
                        error!("Failed to request remote references: {}", e);
                    };
                }
                ToolbarAction::None => (),
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

        let commit_view_action = egui::TopBottomPanel::bottom("commit_view_panel")
            .default_height(ctx.available_rect().height() / 2.0)
            .resizable(true)
            .min_height(100.0)
            .show(ctx, |ui| {
                self.commit_view
                    .show(ui, &self.commit_cache, self.commit_log.selected_commit())
            })
            .inner;

        let sidebar_action = egui::SidePanel::right("sidebar")
            .resizable(true)
            .show(ctx, |ui| {
                self.sidebar.show(
                    ui,
                    &self.view_state,
                    &mut self.pending_view_state,
                    &mut self.clipboard,
                )
            })
            .inner;

        let commit_log_actions = egui::CentralPanel::default()
            .show(ctx, |ui| -> Vec<commit_log::CommitLogAction> {
                self.commit_log
                    .show(ui, &self.commit_cache, &mut self.clipboard)
            })
            .inner;

        if let Some(remote_ref) = self.download_dialog.show(ctx) {
            self.tx
                .send(AppRequest::FetchRemoteRef(
                    self.repo_state.repo.clone(),
                    remote_ref,
                ))
                .context("Failed to send remote fetch request")?;
        }

        match sidebar_action {
            SidebarAction::Checkout(id) => {
                self.request_checkout(Identifier::Reference(id))?;
            }
            SidebarAction::Delete(id) => {
                self.tx
                    .send(AppRequest::Delete((*self.repo_state).clone(), id))
                    .context("Failed to send delete request")?;
            }
            SidebarAction::None => (),
        }

        self.ensure_selected_commit_in_cache()?;
        self.handle_commit_view_action(commit_view_action)?;
        self.handle_commit_log_actions(commit_log_actions)?;
        self.request_pending_view_state()?;

        Ok(())
    }
}

pub struct Gui {
    inner: Arc<Mutex<GuiInner>>,
}

impl Gui {
    pub fn new(
        tx: Sender<AppRequest>,
        rx: Receiver<AppEvent>,
        cc: &CreationContext<'_>,
    ) -> Result<Gui> {
        let inner = Arc::new(Mutex::new(GuiInner::new(tx)?));

        let ctx = &cc.egui_ctx;

        // Colors are unreadable in light mode, force dark mode for now
        ctx.set_visuals(egui::Visuals::dark());
        // We need to spawn a thread to process events
        std::thread::spawn({
            let inner = Arc::clone(&inner);
            let ctx = ctx.clone();
            move || {
                while let Ok(response) = rx.recv() {
                    let mut inner = inner.lock().unwrap();
                    inner.handle_event(response);
                    ctx.request_repaint();
                }
            }
        });

        Ok(Gui { inner })
    }
}

impl App for Gui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut inner = self.inner.lock().unwrap();
        let res = inner.update(ctx);

        if let Err(e) = res {
            // FIXME: Ratelimit
            error!("{:?}", e);
        }
    }
}

enum ToolbarAction {
    OpenRepo(PathBuf),
    ShowDownloadDialog,
    None,
}

fn render_toolbar(ui: &mut egui::Ui, show_console: &mut bool) -> ToolbarAction {
    let mut ret = ToolbarAction::None;
    ui.horizontal(|ui| {
        let response = ui.button("Open repo");
        if response.clicked() {
            let repo = rfd::FileDialog::new().pick_folder();
            if let Some(v) = repo {
                ret = ToolbarAction::OpenRepo(v);
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

        if ui.button("Download references").clicked() {
            ret = ToolbarAction::ShowDownloadDialog;
        }
    });
    ret
}

// Clippy wants this to be a reference but that doesn't allow egui to change the length of the
// string etc.
#[allow(clippy::ptr_arg)]
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
        let response = egui::TextEdit::singleline(git_command)
            .id_source("git_command")
            .desired_rows(1)
            .desired_width(ui.available_width())
            .font(TextStyle::Monospace)
            .show(ui)
            .response;

        ui.with_layout(Layout::default(), |ui| {
            ScrollArea::vertical()
                .id_source("console")
                .auto_shrink([false, false])
                .stick_to_bottom()
                .show(ui, |ui| {
                    let s = output.join("\n");
                    let mut s_s = s.as_str();

                    TextEdit::multiline(&mut s_s)
                        .desired_width(ui.available_width())
                        .font(ui.style().text_styles[&TextStyle::Monospace].clone())
                        .show(ui);
                });
        });

        let enter_pressed = response.lost_focus() && ui.input().key_pressed(egui::Key::Enter);
        if enter_pressed {
            response.request_focus();
        }
        enter_pressed
    })
    .inner
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
        ReferenceId::Tag(_) => Color32::LIGHT_YELLOW,
        ReferenceId::Unknown => Color32::RED,
    }
}

fn try_set_clipboard(clipboard: &mut ClipboardContext, s: String) {
    if let Err(e) = clipboard.set_contents(s) {
        error!("Failed to set clipboard contents: {}", e);
    }
}

enum SearchAction {
    Changed,
    Next,
    Prev,
    None,
}

struct SearchBar<'a> {
    search_text: &'a mut String,
    width: Option<f32>,
}

impl<'a> SearchBar<'a> {
    fn new(search_text: &mut String) -> SearchBar {
        SearchBar {
            search_text,
            width: None,
        }
    }

    fn desired_width(mut self, width: f32) -> SearchBar<'a> {
        self.width = Some(width);
        self
    }

    fn show(self, ui: &mut Ui) -> SearchAction {
        let width = self.width.unwrap_or_else(|| ui.available_width());

        ui.allocate_ui_with_layout(
            egui::vec2(width, ui.spacing().interact_size.y),
            Layout::right_to_left(),
            |ui| {
                let next_response = ui.button("next");
                let prev_response = ui.button("prev");

                let text_response = TextEdit::singleline(self.search_text)
                    .desired_width(ui.available_width())
                    .hint_text("search")
                    .show(ui)
                    .response;

                if text_response.lost_focus() && ui.input().key_pressed(eframe::egui::Key::Enter) {
                    text_response.request_focus();
                    if ui.input().modifiers.shift {
                        SearchAction::Prev
                    } else {
                        SearchAction::Next
                    }
                } else if text_response.changed() {
                    SearchAction::Changed
                } else if next_response.clicked() {
                    SearchAction::Next
                } else if prev_response.clicked() {
                    SearchAction::Prev
                } else {
                    SearchAction::None
                }
            },
        )
        .inner
    }
}
