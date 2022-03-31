use crate::git::{build_git_history_graph, Branch, BranchId, Commit, HistoryGraph, ObjectId, Repo};

use anyhow::{bail, Context, Error, Result};
use log::{debug, error};
use std::{
    path::PathBuf,
    process::Command,
    sync::mpsc::{Receiver, Sender},
};

#[derive(Clone, Eq, PartialEq, Default)]
pub struct RepoState {
    pub(crate) repo: PathBuf,
    pub(crate) branches: Vec<Branch>,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct ViewState {
    pub(crate) selected_branches: Vec<BranchId>,
}

impl ViewState {
    pub(crate) fn update_with_repo_state(&mut self, repo_state: &RepoState) {
        let selected_branches = std::mem::take(&mut self.selected_branches);
        let had_any_branches = !selected_branches.is_empty();
        self.selected_branches = selected_branches
            .into_iter()
            .filter(|selected| repo_state.branches.iter().any(|b| &b.id == selected))
            .collect();

        if self.selected_branches.is_empty() && had_any_branches {
            self.selected_branches = vec![BranchId::Head]
        }
    }
}

pub enum AppRequest {
    OpenRepo(PathBuf),
    GetCommitGraph {
        expected_repo: PathBuf,
        view_state: ViewState,
    },
    GetCommit {
        expected_repo: PathBuf,
        id: ObjectId,
    },
    ExecuteGitCommand(RepoState, String),
}

pub enum AppEvent {
    CommandExecuted(String),
    RepoStateUpdated(RepoState),
    CommitGraphFetched(ViewState, HistoryGraph),
    CommitFetched { repo: PathBuf, commit: Commit },
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
            AppRequest::ExecuteGitCommand(repo_state, cmd) => {
                if !cmd.starts_with("git ") {
                    bail!("Invalid git command: {}", cmd);
                }

                if self.get_repo_state()? != repo_state {
                    bail!("Repo state has changed since {} was executed", cmd);
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
            AppRequest::GetCommit { expected_repo, id } => match &mut self.repo {
                Some(repo) => {
                    if repo.git_dir() != expected_repo {
                        debug!(
                            "Ignoring commit request for {}, {} is no longer open",
                            id,
                            expected_repo.display()
                        );
                        return Ok(());
                    }

                    self.tx
                        .send(AppEvent::CommitFetched {
                            repo: expected_repo,
                            commit: repo.get_commit(&id)?,
                        })
                        .context("Failed to send commit fetched")?;
                }
                None => {
                    bail!("Commit requested without valid repo");
                }
            },
            AppRequest::OpenRepo(path) => {
                let mut repo = Repo::new(path).context("Failed to load git history")?;

                let repo_state = get_repo_state(&mut repo)?;

                self.tx
                    .send(AppEvent::RepoStateUpdated(repo_state))
                    .context("Failed to send response branches")?;

                self.repo = Some(repo);
            }
            AppRequest::GetCommitGraph {
                expected_repo,
                view_state,
            } => match &mut self.repo {
                Some(repo) => {
                    if repo.git_dir() != expected_repo {
                        bail!(
                            "Current repo does not match expected repo: {}, {}",
                            repo.git_dir().display(),
                            expected_repo.display()
                        );
                    }

                    let heads = view_state
                        .selected_branches
                        .iter()
                        .map(|id| repo.find_branch_head(id))
                        .collect::<Result<Vec<_>>>()?;

                    let graph = build_git_history_graph(repo, &heads)?;

                    self.tx
                        .send(AppEvent::CommitGraphFetched(view_state, graph))
                        .context("Failed to send response commit log")?;
                }
                None => {
                    bail!("Branches selected without valid repo");
                }
            },
        }

        Ok(())
    }

    fn get_repo_state(&mut self) -> Result<RepoState> {
        let repo = self.repo.as_mut().ok_or_else(|| Error::msg("No repo"))?;
        get_repo_state(repo)
    }
}

fn get_repo_state(repo: &mut Repo) -> Result<RepoState> {
    let mut branches = vec![Ok(Branch {
        head: repo.find_branch_head(&BranchId::Head)?,
        id: BranchId::Head,
    })];
    branches.extend(repo.branches().context("Failed to retrieve branches")?);
    let branches = branches.into_iter().collect::<Result<_>>()?;

    Ok(RepoState {
        repo: repo.git_dir().to_path_buf(),
        branches,
    })
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn view_state_deleted_branch() -> Result<()> {
        let mut view_state = ViewState {
            selected_branches: vec![
                BranchId::Head,
                BranchId::Remote("Test".to_string()),
                BranchId::Local("Test".to_string()),
            ],
        };

        view_state.update_with_repo_state(&RepoState {
            repo: PathBuf::new(),
            branches: vec![
                Branch {
                    id: BranchId::Head,
                    head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse()?,
                },
                Branch {
                    id: BranchId::Remote("Test".to_string()),
                    head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse()?,
                },
            ],
        });

        assert_eq!(view_state.selected_branches.len(), 2);
        assert_eq!(
            view_state.selected_branches,
            &[BranchId::Head, BranchId::Remote("Test".to_string())]
        );
        Ok(())
    }

    #[test]
    fn view_state_preserve_no_selection() -> Result<()> {
        let mut view_state = ViewState {
            selected_branches: vec![],
        };

        view_state.update_with_repo_state(&RepoState {
            repo: PathBuf::new(),
            branches: vec![Branch {
                id: BranchId::Head,
                head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse()?,
            }],
        });

        assert_eq!(view_state.selected_branches.len(), 0);
        assert_eq!(view_state.selected_branches, &[]);

        Ok(())
    }

    #[test]
    fn view_state_swap_to_head() -> Result<()> {
        let mut view_state = ViewState {
            selected_branches: vec![BranchId::Local("master".into())],
        };

        view_state.update_with_repo_state(&RepoState {
            repo: PathBuf::new(),
            branches: vec![Branch {
                id: BranchId::Head,
                head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse()?,
            }],
        });

        // Only selected branch remove, swap to HEAD
        assert_eq!(view_state.selected_branches.len(), 1);
        assert_eq!(view_state.selected_branches, &[BranchId::Head]);

        Ok(())
    }
}
