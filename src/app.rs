use crate::git::{build_git_history_graph, Commit, HistoryGraph, ObjectId, Repo};

use anyhow::{bail, Context, Result};
use log::error;
use std::{
    path::PathBuf,
    process::Command,
    sync::mpsc::{Receiver, Sender},
};

pub enum AppRequest {
    OpenRepo(PathBuf),
    GetCommit(ObjectId),
    ExecuteGitCommand(String),
}

pub enum AppEvent {
    CommandExecuted(String),
    CommitLogProcessed(HistoryGraph),
    CommitFetched(Commit),
    Error(String),
}

pub struct App {
    tx: Sender<AppEvent>,
    rx: Receiver<AppRequest>,
    repo: Option<Repo>,
}

impl App {
    pub fn new(tx: Sender<AppEvent>, rx: Receiver<AppRequest>) -> App {
        App { tx, rx, repo: None }
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

                let git_dir = repo.git_dir();

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
            AppRequest::GetCommit(id) => match &mut self.repo {
                Some(repo) => {
                    self.tx
                        .send(AppEvent::CommitFetched(repo.get_commit(&id)?))
                        .context("Failed to send commit fetched")?;
                }
                None => {
                    bail!("Commit requested without valid repo");
                }
            },
            AppRequest::OpenRepo(path) => {
                let repo = Repo::new(&path).context("Failed to load git history")?;
                self.repo = Some(repo);
                let repo = self.repo.as_mut().unwrap();
                let graph = build_git_history_graph(repo)?;
                self.tx
                    .send(AppEvent::CommitLogProcessed(graph))
                    .context("Failed to send response commit log")?;
            }
        }

        Ok(())
    }
}
