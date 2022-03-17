use crate::git::{build_git_history_graph, History, HistoryGraph};

use anyhow::{bail, Context, Result};
use log::error;
use std::{
    path::PathBuf,
    process::Command,
    sync::mpsc::{Receiver, Sender},
};

pub enum AppRequest {
    OpenRepo(PathBuf),
    ExecuteGitCommand(String),
}

pub enum AppEvent {
    CommandExecuted(String),
    CommitLogProcessed(HistoryGraph),
    Error(String),
}

pub struct App {
    tx: Sender<AppEvent>,
    rx: Receiver<AppRequest>,
    repo: Option<git2::Repository>,
    history: Option<History>,
}

impl App {
    pub fn new(tx: Sender<AppEvent>, rx: Receiver<AppRequest>) -> App {
        App {
            tx,
            rx,
            repo: None,
            history: None,
        }
    }
    pub fn run(&mut self) {
        while let Ok(req) = self.rx.recv() {
            if let Err(e) = self.handle_req(req) {
                let err_s = format!("{:?}", e);
                error!("{}", err_s);
                let _ = self.tx.send(AppEvent::Error(err_s));
            }
        }
    }

    fn handle_req(&mut self, req: AppRequest) -> Result<()> {
        match req {
            AppRequest::ExecuteGitCommand(cmd) => {
                if !cmd.starts_with("git ") {
                    bail!("Invalid git command: {}", cmd);
                }
                let cmd = &cmd[4..];

                let repo = match &self.repo {
                    Some(repo) => repo,
                    None => bail!("Invalid repo"),
                };

                let git_dir = repo.workdir().unwrap_or_else(|| repo.path());

                let args = [
                    "-c",
                    &format!("git -C \"{}\" {} 2>&1", git_dir.display(), cmd.trim()),
                ];
                let output = Command::new("/bin/bash")
                    .args(args)
                    .output()
                    .with_context(|| format!("Failed to execute {}", cmd))?;

                let parsed = String::from_utf8(output.stdout)
                    .context("Git response was not a valid utf8 string")?;

                self.tx
                    .send(AppEvent::CommandExecuted(parsed))
                    .context("Failed to send response to gui")?;
            }
            AppRequest::OpenRepo(path) => {
                let repo =
                    git2::Repository::open(&path).context("Failed to open git repository")?;
                let history = History::new(&path).context("Failed to load git history")?;
                self.repo = Some(repo);
                self.history = Some(history);
                let repo = self.repo.as_ref().unwrap();
                let history = self.history.as_mut().unwrap();
                let graph = build_git_history_graph(repo, history)?;
                self.tx
                    .send(AppEvent::CommitLogProcessed(graph))
                    .context("Failed to send response commit log")?;
            }
        }

        Ok(())
    }
}
