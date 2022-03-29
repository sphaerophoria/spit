use crate::git::{build_git_history_graph, Branch, BranchId, Commit, HistoryGraph, ObjectId, Repo};

use anyhow::{bail, Context, Error, Result};
use log::{debug, error, info};
use notify::{self, RawEvent, RecommendedWatcher, RecursiveMode, Watcher};

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
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
    Refresh,
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
    notifier: RecommendedWatcher,
    repo: Option<Repo>,
}

impl App {
    pub fn new(
        event_tx: Sender<AppEvent>,
        request_tx: Sender<AppRequest>,
        request_rx: Receiver<AppRequest>,
    ) -> Result<App> {
        Ok(App {
            tx: event_tx,
            rx: request_rx,
            notifier: spawn_watcher(request_tx)?,
            repo: None,
        })
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
                let mut repo = Repo::new(path.clone()).context("Failed to load git history")?;

                let repo_state = get_repo_state(&mut repo)?;

                self.tx
                    .send(AppEvent::RepoStateUpdated(repo_state))
                    .context("Failed to send response branches")?;

                self.repo = Some(repo);
                // FIXME: There is a race here where if a new object is created between when we
                // fetched the repo state and now we will not update the repo, however if we move
                // this up and changing repos fails the old path will not be watched anymore, and
                // we may miss an update in the old repo.
                self.notifier.watch(path, RecursiveMode::Recursive)?;
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
            AppRequest::Refresh => {
                let repo_state = self.get_repo_state()?;

                self.tx
                    .send(AppEvent::RepoStateUpdated(repo_state))
                    .context("Failed to send response branches")?;
            }
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

fn path_is_lock_file(path: Option<&Path>) -> bool {
    let path = match path {
        Some(p) => p,
        None => return false,
    };

    let extension = match path.extension() {
        Some(e) => e,
        None => return false,
    };

    extension == OsStr::new("lock")
}

fn debounce_event(notifier_rx: &Receiver<RawEvent>) {
    let debounce_max = Instant::now() + Duration::from_secs(2);
    let debounce_period = Duration::from_millis(500);

    while let Ok(_event) = notifier_rx.recv_timeout(debounce_period) {
        if Instant::now() > debounce_max {
            return;
        }
    }
}

fn spawn_watcher(app_tx: Sender<AppRequest>) -> Result<RecommendedWatcher> {
    let (notifier_tx, notifier_rx) = mpsc::channel();
    let notifier = notify::raw_watcher(notifier_tx)?;
    thread::spawn(move || {
        while let Ok(event) = notifier_rx.recv() {
            if path_is_lock_file(event.path.as_deref()) {
                continue;
            }

            // Debounce to avoid spam refreshing
            debounce_event(&notifier_rx);

            if let Err(_e) = app_tx.send(AppRequest::Refresh) {
                info!("App handle is no longer valid, closing watcher");
                return;
            }
        }
    });

    Ok(notifier)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_lock_file_check() {
        assert_eq!(path_is_lock_file(None), false);
        assert_eq!(path_is_lock_file(Some(&Path::new("test.test"))), false);
        assert_eq!(path_is_lock_file(Some(&Path::new("test.lock"))), true);
        // I don't know what I think this should be, but lets at least prove that we know how it
        // works
        assert_eq!(path_is_lock_file(Some(&Path::new(".lock"))), false);
        assert_eq!(path_is_lock_file(Some(&Path::new("lock"))), false);
        assert_eq!(path_is_lock_file(Some(&Path::new("test/test.lock"))), true);
    }

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
