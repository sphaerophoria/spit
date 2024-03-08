mod priority_queue;

use crate::{
    app::priority_queue::PriorityQueue,
    git::{
        self, build_git_history_graph, Commit, Diff, DiffTarget, HistoryGraph, Identifier,
        ModifiedFiles, ObjectId, Reference, ReferenceId, RemoteRef, Repo, SortType,
    },
};

use anyhow::{bail, Context, Error, Result};
use log::{debug, error, info};
use notify::{self, Event, RecommendedWatcher, RecursiveMode, Watcher};
use spiff::{DiffCollectionProcessor, DiffOptions};

use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fmt,
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

#[derive(Default, PartialEq, Eq)]
pub struct RemoteState {
    pub(crate) repo: PathBuf,
    pub(crate) references: Vec<RemoteRef>,
}

#[derive(Debug, Default, Eq, PartialEq, Clone)]
pub(crate) struct IndexState {
    pub(crate) files: HashMap<PathBuf, ObjectId>,
}

#[derive(Clone, Eq, PartialEq, Default)]
pub struct RepoState {
    pub(crate) repo: PathBuf,
    pub(crate) index: IndexState,
    pub(crate) head: ReferenceId,
    pub(crate) references: Vec<Reference>,
}

impl RepoState {
    pub(crate) fn head_object_id(&self) -> ObjectId {
        for reference in &self.references {
            if reference.id == self.head {
                return reference.head.clone();
            }
        }
        panic!("did not find object id for head");
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct ViewState {
    pub(crate) selected_references: HashSet<ReferenceId>,
    pub(crate) sort_type: SortType,
}

impl ViewState {
    pub(crate) fn update_with_repo_state(&mut self, repo_state: &RepoState) {
        let selected_references = std::mem::take(&mut self.selected_references);
        let had_any_branches = !selected_references.is_empty();
        self.selected_references = selected_references
            .into_iter()
            .filter(|selected| repo_state.references.iter().any(|b| &b.id == selected))
            .collect();

        if self.selected_references.is_empty() && had_any_branches {
            self.selected_references = FromIterator::from_iter([ReferenceId::head()]);
        }
    }
}

struct DiffProcessorWithData {
    data: ModifiedFiles,
    processor: Option<DiffCollectionProcessor<'static>>,
}

pub enum AppRequest {
    OpenRepo(PathBuf),
    GetCommitGraph {
        expected_repo: PathBuf,
        // Unique ID from the UI for preempting
        viewer_id: String,
        view_state: ViewState,
    },
    Refresh {
        paths: Vec<PathBuf>,
    },
    GetCommit {
        expected_repo: PathBuf,
        id: ObjectId,
    },
    GetDiff {
        expected_repo: PathBuf,
        from: DiffTarget,
        to: DiffTarget,
        options: DiffOptions,
        search_query: String,
    },
    Search {
        expected_repo: PathBuf,
        viewer_id: String,
        commit_list: Vec<ObjectId>,
        search_string: String,
    },
    Checkout(RepoState, Identifier),
    Delete(RepoState, ReferenceId),
    CherryPick(RepoState, ObjectId),
    DiffTool(ObjectId),
    Merge(RepoState, Identifier),
    ExecuteGitCommand(RepoState, String),
    UpdateRemotes {
        expected_repo: PathBuf,
    },
    FetchRemoteRef(PathBuf, RemoteRef),
    FetchAll(PathBuf),
}

impl fmt::Debug for AppRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppRequest::OpenRepo(_) => {
                write!(f, "OpenRepo")
            }
            AppRequest::GetCommitGraph { .. } => {
                write!(f, "GetCommitGraph")
            }
            AppRequest::Refresh { .. } => {
                write!(f, "Refresh")
            }
            AppRequest::GetCommit { .. } => {
                write!(f, "GetCommit")
            }
            AppRequest::GetDiff { .. } => {
                write!(f, "GetDiff")
            }
            AppRequest::Search { .. } => {
                write!(f, "Search")
            }
            AppRequest::Checkout(_, _) => {
                write!(f, "Checkout")
            }
            AppRequest::Delete(_, _) => {
                write!(f, "Delete")
            }
            AppRequest::CherryPick(_, _) => {
                write!(f, "CherryPick")
            }
            AppRequest::DiffTool(_) => {
                write!(f, "DiffTool")
            }
            AppRequest::Merge(_, _) => {
                write!(f, "Merge")
            }
            AppRequest::ExecuteGitCommand(_, _) => {
                write!(f, "ExecuteGitCommand")
            }
            AppRequest::UpdateRemotes { .. } => {
                write!(f, "UpdateRemotes")
            }
            AppRequest::FetchRemoteRef(_, _) => {
                write!(f, "FetchRemoteRef")
            }
            AppRequest::FetchAll(_) => {
                write!(f, "FetchAll")
            }
        }
    }
}

pub enum AppEvent {
    OutputLogged(String),
    RepoStateUpdated(RepoState),
    WorkdirUpdated,
    RemoteStateUpdated(RemoteState),
    CommitGraphFetched(ViewState, HistoryGraph),
    CommitFetched {
        repo: PathBuf,
        commit: Commit,
    },
    DiffFetched {
        repo: PathBuf,
        diff: Diff,
    },
    SearchFinished {
        viewer_id: String,
        matched_id: Option<ObjectId>,
    },
    Error(String),
}

impl fmt::Debug for AppEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppEvent::OutputLogged(_) => {
                write!(f, "OutputLogged")
            }
            AppEvent::RepoStateUpdated(_) => {
                write!(f, "RepoStateUpdated")
            }
            AppEvent::WorkdirUpdated => {
                write!(f, "WorkdirUpdated")
            }
            AppEvent::RemoteStateUpdated(_) => {
                write!(f, "RemoteStateUpdated")
            }
            AppEvent::CommitGraphFetched(_, _) => {
                write!(f, "CommitGraphFetched")
            }
            AppEvent::CommitFetched { .. } => {
                write!(f, "CommitFetched")
            }
            AppEvent::DiffFetched { .. } => {
                write!(f, "DiffFetched")
            }
            AppEvent::SearchFinished { .. } => {
                write!(f, "SearchFinished")
            }
            AppEvent::Error(_) => {
                write!(f, "Error")
            }
        }
    }
}

pub struct App {
    tx: Sender<AppEvent>,
    rx: PriorityQueue,
    notifier: RecommendedWatcher,
    repo: Option<Repo>,
    // Pin<Box<..>> to allow self reference
    processor: Option<Pin<Box<DiffProcessorWithData>>>,
}

impl App {
    pub fn new(
        event_tx: Sender<AppEvent>,
        request_tx: Sender<AppRequest>,
        request_rx: Receiver<AppRequest>,
    ) -> Result<App> {
        Ok(App {
            tx: event_tx,
            rx: PriorityQueue::new(request_rx),
            notifier: spawn_watcher(request_tx)?,
            repo: None,
            processor: None,
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

    fn execute_command(&mut self, requested_state: &RepoState, cmd: &str) -> Result<()> {
        if self.get_repo_state()? != *requested_state {
            bail!("Repo state has changed since {} requested", cmd);
        }

        let repo = match &self.repo {
            Some(repo) => repo,
            None => bail!("Invalid repo"),
        };

        let repo_root = repo.repo_root();

        self.tx
            .send(AppEvent::OutputLogged(cmd.to_string()))
            .context("Failed to send response to gui")?;

        // NOTE: This looks really wrong, and that's because it is to some extent. We should not be
        // running bash commands for every git command we want to run. But this has the large benefit
        // that every action the program executes can be copy pasted by a user and run again. This
        // makes interop with command line users very nice and is worth the architectural incorrectness
        // of shelling out
        let mut bash_cmd = OsString::new();
        bash_cmd.push("2>&1 ");
        bash_cmd.push(cmd);

        let editor = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|x| x.to_path_buf()))
            .map(|p| p.join("spit-editor"));

        let mut command = Command::new("bash");

        command.arg("-c").arg(bash_cmd).current_dir(repo_root);

        if let Some(editor) = editor {
            command.env("EDITOR", editor);
        }

        let output = command
            .output()
            .with_context(|| format!("Failed to run {}", cmd))?;

        let parsed =
            String::from_utf8(output.stdout).context("Git response was not a valid utf8 string")?;

        self.tx
            .send(AppEvent::OutputLogged(parsed))
            .context("Failed to send response to gui")?;

        Ok(())
    }

    fn handle_req(&mut self, req: AppRequest) -> Result<()> {
        match req {
            AppRequest::Checkout(repo_state, checkout_item) => {
                self.execute_command(&repo_state, &git::commandline::checkout(&checkout_item))?;
            }
            AppRequest::Delete(repo_state, reference_id) => {
                self.execute_command(&repo_state, &git::commandline::delete(&reference_id)?)?;
            }
            AppRequest::CherryPick(repo_state, id) => {
                self.execute_command(&repo_state, &git::commandline::cherry_pick(&id))?;
            }
            AppRequest::DiffTool(id) => {
                // Non-modifying action. RepoState not required
                let repo_state = self.get_repo_state()?;
                self.execute_command(&repo_state, &git::commandline::difftool(&id))?;
            }
            AppRequest::Merge(repo_state, id) => {
                self.execute_command(&repo_state, &git::commandline::merge(&id))?;
            }
            AppRequest::ExecuteGitCommand(repo_state, cmd) => {
                let cmd = cmd.trim();
                self.execute_command(&repo_state, cmd)?;
            }
            AppRequest::GetCommit { expected_repo, id } => match &mut self.repo {
                Some(repo) => {
                    if repo.repo_root() != expected_repo {
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
            AppRequest::GetDiff {
                expected_repo,
                from,
                to,
                options,
                search_query,
            } => {
                let repo = self
                    .repo
                    .as_mut()
                    .ok_or_else(|| Error::msg("Commit requested without valid repo"))?;

                if expected_repo != repo.repo_root() {
                    debug!(
                        "Ignoring diff request for {} -> {}, {} is no longer open",
                        from,
                        to,
                        expected_repo.display()
                    );
                    return Ok(());
                }

                if self.processor.as_ref().map(|x| &x.data.id_a) != Some(&from)
                    || self.processor.as_ref().map(|x| &x.data.id_b) != Some(&to)
                    // Index is malleable, there is not a single identifier like for objects. We
                    // could cache the list of object IDs for all items in the index, however this
                    // doesn't seem worth it when we can just refresh it
                    || from == DiffTarget::Index
                    || to == DiffTarget::Index
                {
                    let modified_files = match (&from, &to) {
                        (DiffTarget::Object(a), DiffTarget::Object(b)) => repo
                            .modified_files(a, b)
                            .context("Failed to retrieve modified files")?,
                        (DiffTarget::Object(a), DiffTarget::Index) => repo
                            .modified_files_with_index(a)
                            .context("Failed to retrieve modified files")?,
                        (DiffTarget::Index, DiffTarget::Workdir) => repo
                            .modified_files_index_to_workdir()
                            .context("failed to retrieve modified files")?,
                        _ => {
                            bail!("Unsupported diff target combination");
                        }
                    };

                    self.processor = Some(Box::pin(DiffProcessorWithData {
                        data: modified_files,
                        processor: None,
                    }));

                    // HACK
                    // The DiffCollectionProcessor internally stores different views of the input
                    // data. It's API forces the data to be stored externally.
                    //
                    // At this level of abstraction anywhere we want to store the data across
                    // calls forces self reference. Wrap our processor in a Pin<Box<..>> as a
                    // guarantee that it does not move. Since our processor now lives in the same
                    // struct as the data, we know that the data will not go out of scope while it
                    // is still in use. We can transmute the struct to clobber the incorrect
                    // lifetimes and move on
                    let processor = self.processor.as_mut().unwrap();
                    let internal_processor = DiffCollectionProcessor::new(
                        &processor.data.files_a,
                        &processor.data.files_b,
                        &processor.data.labels,
                    )?;
                    processor.processor = unsafe { std::mem::transmute(internal_processor) };
                }

                let container = self.processor.as_mut().unwrap();
                let processor = container.processor.as_mut().unwrap();
                processor.process_new_options(&options);
                processor.set_search_query(search_query);
                let processed_diffs = processor.generate();

                self.tx.send(AppEvent::DiffFetched {
                    repo: expected_repo,
                    diff: Diff {
                        metadata: git::DiffMetadata { from, to, options },
                        diff: processed_diffs,
                    },
                })?;
            }
            AppRequest::Search {
                expected_repo,
                viewer_id,
                commit_list,
                search_string,
            } => {
                let repo = self
                    .repo
                    .as_mut()
                    .ok_or_else(|| Error::msg("Commit requested without valid repo"))?;

                if repo.repo_root() != expected_repo {
                    bail!(
                        "Current repo does not match expected repo: {}, {}",
                        repo.repo_root().display(),
                        expected_repo.display()
                    );
                }

                let mut matched_id = None;
                for id in commit_list {
                    let commit = repo
                        .get_commit(&id)
                        .context("Search requested with invalid id")?;

                    if commit_matches_search(&commit, &search_string) {
                        matched_id = Some(id);
                        break;
                    }
                }

                self.tx
                    .send(AppEvent::SearchFinished {
                        viewer_id,
                        matched_id,
                    })
                    .context("Failed to send search response")?;
            }
            AppRequest::OpenRepo(path) => {
                let mut repo = Repo::new(path, true).context("Failed to load git history")?;

                let repo_state = get_repo_state(&mut repo)?;

                self.tx
                    .send(AppEvent::RepoStateUpdated(repo_state))
                    .context("Failed to send response branches")?;

                // FIXME: There is a race here where if a new object is created between when we
                // fetched the repo state and now we will not update the repo, however if we move
                // this up and changing repos fails the old path will not be watched anymore, and
                // we may miss an update in the old repo.
                //
                // FIXME: We need to unwatch the previous dir
                self.notifier
                    .watch(repo.git_dir(), RecursiveMode::Recursive)?;
                self.notifier
                    .watch(repo.repo_root(), RecursiveMode::Recursive)?;
                self.repo = Some(repo);
            }
            AppRequest::GetCommitGraph {
                expected_repo,
                view_state,
                ..
            } => match &mut self.repo {
                Some(repo) => {
                    if repo.repo_root() != expected_repo {
                        bail!(
                            "Current repo does not match expected repo: {}, {}",
                            repo.repo_root().display(),
                            expected_repo.display()
                        );
                    }

                    let heads = view_state
                        .selected_references
                        .iter()
                        .map(|id| repo.find_reference_commit_id(id))
                        .collect::<Result<Vec<_>>>()?;

                    let graph = build_git_history_graph(repo, &heads, view_state.sort_type)?;

                    self.tx
                        .send(AppEvent::CommitGraphFetched(view_state, graph))
                        .context("Failed to send response commit log")?;
                }
                None => {
                    bail!("Branches selected without valid repo");
                }
            },
            AppRequest::Refresh { paths } => {
                let Some(repo) = &mut self.repo else {
                    return Ok(());
                };

                // FIXME: Should this be split out somewhere
                let git_dir = repo.git_dir();
                let working_dir = repo.repo_root().to_path_buf();

                let mut git_dir_update = false;
                let mut working_dir_update = false;

                for path in paths {
                    if is_descendent(&path, git_dir) && !path_is_lock_file(&path) {
                        git_dir_update = true;
                    } else if is_descendent(&path, &working_dir)
                        && !repo
                            .is_ignored(&path)
                            .context("failed to check if path is ignored")?
                    {
                        working_dir_update = true;
                    }
                }

                if git_dir_update {
                    let repo_state = self.get_repo_state()?;

                    self.tx
                        .send(AppEvent::RepoStateUpdated(repo_state))
                        .context("Failed to send response branches")?;
                }

                if working_dir_update {
                    self.tx
                        .send(AppEvent::WorkdirUpdated)
                        .context("failed to notify of workdir change")?;
                }
            }
            AppRequest::UpdateRemotes { expected_repo } => {
                let repo = self
                    .repo
                    .as_mut()
                    .ok_or_else(|| Error::msg("Update remotes requested without valid repo"))?;

                if repo.repo_root() != expected_repo {
                    bail!(
                        "Current repo does not match expected repo: {}, {}",
                        repo.repo_root().display(),
                        expected_repo.display()
                    );
                }

                let references = repo
                    .remote_refs()
                    .context("Failed to retrieve remote references")?;

                self.tx
                    .send(AppEvent::RemoteStateUpdated(RemoteState {
                        repo: expected_repo,
                        references,
                    }))
                    .context("Failed to send update remotes")?;
            }
            AppRequest::FetchRemoteRef(expected_repo, remote_ref) => {
                let repo = self
                    .repo
                    .as_mut()
                    .ok_or_else(|| Error::msg("Update remotes requested without valid repo"))?;

                if repo.repo_root() != expected_repo {
                    bail!(
                        "Current repo does not match expected repo: {}, {}",
                        repo.repo_root().display(),
                        expected_repo.display()
                    );
                }

                let repo_state = self.get_repo_state()?;

                self.execute_command(
                    &repo_state,
                    &git::commandline::fetch_remote_ref(&remote_ref),
                )?;
            }
            AppRequest::FetchAll(expected_repo) => {
                let repo = self
                    .repo
                    .as_mut()
                    .ok_or_else(|| Error::msg("Update remotes requested without valid repo"))?;

                if repo.repo_root() != expected_repo {
                    bail!(
                        "Current repo does not match expected repo: {}, {}",
                        repo.repo_root().display(),
                        expected_repo.display()
                    );
                }

                let repo_state = self.get_repo_state()?;

                self.execute_command(&repo_state, git::commandline::fetch_all())?;
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
    let mut branches = vec![Ok(Reference {
        head: repo.find_reference_commit_id(&ReferenceId::head())?,
        id: ReferenceId::head(),
    })];
    branches.extend(repo.branches().context("Failed to retrieve branches")?);
    let mut references = branches.into_iter().collect::<Result<Vec<_>>>()?;
    let head = repo.resolve_reference(&ReferenceId::head())?;
    let tags = repo.tags().context("Failed to retrieve tags")?;
    let index = repo.index().context("failed to retrieve index")?;
    references.extend(tags);

    Ok(RepoState {
        repo: repo.repo_root().to_path_buf(),
        index,
        head,
        references,
    })
}

fn path_is_lock_file(path: &Path) -> bool {
    let extension = match path.extension() {
        Some(e) => e,
        None => return false,
    };

    extension == OsStr::new("lock")
}

fn debounce_event(notifier_rx: &Receiver<Result<Event, notify::Error>>) -> Result<Vec<PathBuf>> {
    struct DebouncedWatcher {
        observed_paths: HashSet<PathBuf>,
    }

    impl DebouncedWatcher {
        fn handle_event(&mut self, event: Result<Event, notify::Error>) -> Result<()> {
            let event = event.context("failed to read event")?;
            self.observed_paths.extend(event.paths);
            Ok(())
        }
    }

    let mut watcher = DebouncedWatcher {
        observed_paths: HashSet::new(),
    };

    let event = notifier_rx
        .recv()
        .context("failed to get event from notifier")?;
    watcher
        .handle_event(event)
        .context("failed to handle event")?;

    let debounce_end = Instant::now() + Duration::from_millis(500);

    loop {
        let wait_time = debounce_end - Instant::now();
        let Ok(event) = notifier_rx.recv_timeout(wait_time) else {
            return Ok(watcher.observed_paths.into_iter().collect());
        };

        watcher
            .handle_event(event)
            .context("failed to handle event")?;
    }
}

fn spawn_watcher(app_tx: Sender<AppRequest>) -> Result<RecommendedWatcher> {
    let (notifier_tx, notifier_rx) = mpsc::channel();
    let notifier = notify::recommended_watcher(notifier_tx)?;
    thread::spawn(move || {
        // Wait for event
        loop {
            // Debounce to avoid spam refreshing
            let paths = match debounce_event(&notifier_rx) {
                Ok(v) => v,
                Err(e) => {
                    error!("Notifier thread died: {e}");
                    return;
                }
            };

            if let Err(_e) = app_tx.send(AppRequest::Refresh { paths }) {
                info!("App handle is no longer valid, closing watcher");
                return;
            }
        }
    });

    Ok(notifier)
}

pub fn commit_matches_search(commit: &Commit, search: &str) -> bool {
    if commit.metadata.id.to_string().starts_with(search)
        || commit.author.contains(search)
        || commit.message.contains(search)
    {
        return true;
    }

    false
}

fn is_descendent(path: &Path, potential_ancestor: &Path) -> bool {
    for ancestor in path.ancestors() {
        if ancestor == potential_ancestor {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_lock_file_check() {
        assert_eq!(path_is_lock_file(&Path::new("test.test")), false);
        assert_eq!(path_is_lock_file(&Path::new("test.lock")), true);
        // I don't know what I think this should be, but lets at least prove that we know how it
        // works
        assert_eq!(path_is_lock_file(&Path::new(".lock")), false);
        assert_eq!(path_is_lock_file(&Path::new("lock")), false);
        assert_eq!(path_is_lock_file(&Path::new("test/test.lock")), true);
    }

    #[test]
    fn view_state_deleted_branch() -> Result<()> {
        let mut view_state = ViewState {
            selected_references: FromIterator::from_iter([
                ReferenceId::head(),
                ReferenceId::RemoteBranch("Test".to_string()),
                ReferenceId::LocalBranch("Test".to_string()),
            ]),
            sort_type: SortType::CommitterTimestamp,
        };

        view_state.update_with_repo_state(&RepoState {
            repo: PathBuf::new(),
            index: Default::default(),
            head: ReferenceId::Unknown,
            references: vec![
                Reference {
                    id: ReferenceId::head(),
                    head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse()?,
                },
                Reference {
                    id: ReferenceId::RemoteBranch("Test".to_string()),
                    head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse()?,
                },
            ],
        });

        assert_eq!(view_state.selected_references.len(), 2);
        assert_eq!(
            view_state.selected_references,
            FromIterator::from_iter([
                ReferenceId::head(),
                ReferenceId::RemoteBranch("Test".to_string())
            ])
        );
        Ok(())
    }

    #[test]
    fn view_state_preserve_no_selection() -> Result<()> {
        let mut view_state = ViewState {
            selected_references: Default::default(),
            sort_type: SortType::CommitterTimestamp,
        };

        view_state.update_with_repo_state(&RepoState {
            repo: PathBuf::new(),
            index: Default::default(),
            head: ReferenceId::Unknown,
            references: vec![Reference {
                id: ReferenceId::head(),
                head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse()?,
            }],
        });

        assert_eq!(view_state.selected_references.len(), 0);
        assert_eq!(view_state.selected_references, Default::default());

        Ok(())
    }

    #[test]
    fn view_state_swap_to_head() -> Result<()> {
        let mut view_state = ViewState {
            selected_references: FromIterator::from_iter([ReferenceId::LocalBranch(
                "master".into(),
            )]),
            sort_type: SortType::CommitterTimestamp,
        };

        view_state.update_with_repo_state(&RepoState {
            repo: PathBuf::new(),
            index: Default::default(),
            head: ReferenceId::Unknown,
            references: vec![Reference {
                id: ReferenceId::head(),
                head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse()?,
            }],
        });

        // Only selected branch remove, swap to HEAD
        assert_eq!(view_state.selected_references.len(), 1);
        assert_eq!(
            view_state.selected_references,
            FromIterator::from_iter([ReferenceId::head()])
        );

        Ok(())
    }
}
