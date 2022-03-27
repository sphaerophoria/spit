use crate::git::{build_git_history_graph, Branch, Commit, HistoryGraph, ObjectId, Repo};

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
    SelectBranches(Vec<Branch>),
    ExecuteGitCommand(String),
}

pub enum AppEvent {
    CommandExecuted(String),
    CommitLogProcessed(HistoryGraph),
    CommitFetched(Commit),
    BranchesUpdated(Vec<Branch>),
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

                let graph = build_git_history_graph(repo, &[repo.head()?])?;
                self.tx
                    .send(AppEvent::CommitLogProcessed(graph))
                    .context("Failed to send response commit log")?;

                let mut branches = vec![Ok(Branch {
                    head: repo.head()?,
                    name: "HEAD".to_string(),
                })];
                branches.extend(repo.branches().context("Failed to retrieve branches")?);

                self.tx
                    .send(AppEvent::BranchesUpdated(
                        branches.into_iter().collect::<Result<Vec<Branch>>>()?,
                    ))
                    .context("Failed to send response branches")?;
            }
            AppRequest::SelectBranches(branches) => match &mut self.repo {
                Some(repo) => {
                    let heads = branches.into_iter().map(|b| b.head).collect::<Vec<_>>();
                    let graph = build_git_history_graph(repo, &heads)?;
                    self.tx
                        .send(AppEvent::CommitLogProcessed(graph))
                        .context("Failed to send response commit log")?;
                }
                None => {
                    bail!("Branches selected without valid repo");
                }
            },
        }

        Ok(())
    }
}
