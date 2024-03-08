use crate::{
    app::IndexState,
    git::{
        decompress, pack::Pack, Commit, CommitMetadata, DiffTarget, ModifiedFiles, ObjectId,
        Reference, ReferenceId, RemoteRef,
    },
    util::Timer,
};

use anyhow::{anyhow, Context, Error, Result};
use chrono::{DateTime, FixedOffset, NaiveDateTime};
use flate2::Decompress;
use git2::{RepositoryOpenFlags, TreeEntry, TreeWalkMode, TreeWalkResult};
use log::{debug, error, warn};

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum SortType {
    AuthorTimestamp,
    #[default]
    CommitterTimestamp,
}

pub(crate) struct Repo {
    allow_libgit2_fallback: bool,
    git2_repo: git2::Repository,
    git_dir: PathBuf,
    repo_root: PathBuf,
    packs: Vec<Pack>,
    // NOTE: We do not store the commit metadata within the hashmap directly because it makes it
    // difficult to hand out references to the metadata without copying it out. Instead we hand out
    // metadata IDs that look up the CommitMetadata on demand.
    metadata_lookup: HashMap<ObjectId, usize>,
    metadata_storage: Vec<CommitMetadata>,
    decompressor: Decompress,
}

impl Repo {
    pub(crate) fn new(mut repo_root: PathBuf, allow_libgit2_fallback: bool) -> Result<Repo> {
        if repo_root.is_relative() {
            repo_root = std::env::current_dir()
                .context("failed to get cwd for relative repo root")?
                .join(repo_root);
        }

        let decompressor = Decompress::new(true);
        let ceiling_dirs: &[&Path] = &[];
        let git2_repo = git2::Repository::open_ext(
            repo_root.clone(),
            RepositoryOpenFlags::empty(),
            ceiling_dirs,
        )
        .context("Failed to open git2 repo")?;

        let git_dir = git2_repo.path().to_path_buf();
        let packs = find_packs(&git_dir)?;

        Ok(Repo {
            allow_libgit2_fallback,
            git2_repo,
            repo_root,
            git_dir,
            packs,
            metadata_lookup: HashMap::new(),
            metadata_storage: Vec::new(),
            decompressor,
        })
    }

    pub(crate) fn get_commit_metadata(&mut self, id: &ObjectId) -> Result<CommitMetadata> {
        let idx = self.get_commit_metadata_idx(id)?;
        Ok(self.metadata_storage[idx].clone())
    }

    fn get_commit_metadata_libgit2(&self, id: &ObjectId) -> Result<CommitMetadata> {
        let rev = id.into();
        let commit = self
            .git2_repo
            .find_commit(rev)
            .context("Failed to find commit for rev")?;
        let oid = ObjectId::from(&rev);
        let parents = commit
            .parents()
            .map(|p| ObjectId::from(p.id()))
            .collect::<Vec<_>>();
        let to_datetime = |t: git2::Time| -> Result<_> {
            #[allow(deprecated)]
            let date_time = NaiveDateTime::from_timestamp_opt(t.seconds(), 0)
                .ok_or_else(|| anyhow!("Invalid timestamp"))?;
            let offset = FixedOffset::east_opt(t.offset_minutes())
                .ok_or_else(|| anyhow!("Invalid timezone"))?;
            #[allow(deprecated)]
            Ok(DateTime::<FixedOffset>::from_local(date_time, offset))
        };
        let author_timestamp = to_datetime(commit.author().when())
            .context("Failed to get author timestamp")?
            .into();

        let committer_timestamp = to_datetime(commit.time())
            .context("Failed to get committer timestamp")?
            .into();

        Ok(CommitMetadata {
            id: oid,
            parents,
            author_timestamp,
            committer_timestamp,
        })
    }

    pub(crate) fn get_commit(&mut self, id: &ObjectId) -> Result<Commit> {
        let (message, author) = {
            let commit = self
                .git2_repo
                .find_commit(id.into())
                .context("Failed to find commit id")?;

            let message = commit
                .message()
                .map(|m| m.to_string())
                .unwrap_or_else(String::new);

            let author = commit.author().to_string();

            (message, author)
        };

        let metadata = self
            .get_commit_metadata(id)
            .context("Failed to lookup commit metadata")?;

        Ok(Commit {
            metadata,
            message,
            author,
        })
    }

    /// Private implementation of get_commit_metadata that returns the vector index instead of a
    /// reference to dodge ownership rules associated with handing out CommitMetadata references
    /// when walking our history
    fn get_commit_metadata_idx(&mut self, id: &ObjectId) -> Result<usize> {
        // FIXME: This function does not read nicely at all...

        if let Some(idx) = self.metadata_lookup.get(id) {
            return Ok(*idx);
        }

        let mut obj_subpath = [0; 38];
        faster_hex::hex_encode(&id[1..], &mut obj_subpath)?;
        let obj_subpath = std::str::from_utf8(&obj_subpath)?;
        // Check unpacked objects first since they are cheap
        let unpacked_path = self
            .git_dir
            .join(format!("objects/{:02x}/{}", id[0], obj_subpath));

        let storage_idx = self.metadata_storage.len();
        if unpacked_path.exists() {
            let mut f = File::open(unpacked_path).context("Failed to open object file")?;
            let mut commit = Vec::new();
            f.read_to_end(&mut commit)
                .context("Failed to read object file")?;
            let metadata =
                decompress::decompress_commit_metadata(&commit, &mut self.decompressor, false)?;
            self.metadata_lookup.insert(id.clone(), storage_idx);
            self.metadata_storage
                .push(metadata.into_full_metadata(id.clone()));
            return Ok(storage_idx);
        }

        // Double check if any new packs have been added
        let search_packs_for_metadata = |packs: &mut [Pack]| -> Result<Option<CommitMetadata>> {
            for pack in packs {
                match pack.get_commit_metadata(id.clone()) {
                    Ok(Some(metadata)) => {
                        return Ok(Some(metadata));
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        warn!(
                            "Failed to parse rev {}: {:?}. Falling back on libgit2",
                            id, e
                        );
                        return Err(e);
                    }
                }
            }
            Ok(None)
        };

        let mut search_result = search_packs_for_metadata(&mut self.packs);
        if let Ok(None) = search_result {
            self.packs = find_packs(&self.git_dir).context("Failed to reload packs")?;
            search_result = search_packs_for_metadata(&mut self.packs);
        }

        match search_result {
            Ok(Some(metadata)) => {
                self.metadata_lookup
                    .insert(metadata.id.clone(), storage_idx);
                self.metadata_storage.push(metadata);
                return Ok(storage_idx);
            }
            Ok(None) => {
                warn!("Failed to find rev {}", id);
            }
            Err(e) => {
                warn!("Failed to parse rev {}: {:?}", id, e);
            }
        };

        if !self.allow_libgit2_fallback {
            return Err(anyhow!("Failed to find requested commit id: {}", id));
        }

        // If at this point we still haven't found the commit, we fall back on libgit2 to do it for
        // us

        let metadata = self
            .get_commit_metadata_libgit2(id)
            .with_context(|| format!("Failed to use libgit2 to find id {}", id))?;

        self.metadata_lookup
            .insert(metadata.id.clone(), storage_idx);
        self.metadata_storage.push(metadata);

        Ok(storage_idx)
    }

    /// Build an iterator that iterates over metadatas. Items are sorted such that children are always
    /// seen before parents. When there are multiple choices available the most recent commit is
    /// preferred. This list should be effectively time sorted unless a child has an author time
    /// before a parent. In this case the parent will appear after
    pub(crate) fn metadata_iter(
        &mut self,
        heads: &[ObjectId],
        sort_type: SortType,
    ) -> Result<impl Iterator<Item = &CommitMetadata>> {
        let (walked_indices, child_indices) = self.build_reverse_dag(heads)?;

        // NOTE: From this point on it's guaranteed that all parents of heads are in our
        // metadata_storage, so from this point on it's safe for us to use the metadata storage
        // directly

        build_sorted_metadata_indicies(
            sort_type,
            &walked_indices,
            child_indices,
            &self.metadata_lookup,
            &self.metadata_storage,
        )
    }

    /// Build the reversed dag for the given heads. The output is a Vec of Vecs that represents the
    /// child indices for each metadata_storage index
    fn build_reverse_dag(
        &mut self,
        heads: &[ObjectId],
    ) -> Result<(HashSet<usize>, Vec<Vec<usize>>)> {
        let timer = Timer::new();

        let mut to_walk = heads
            .iter()
            .map(|head| -> Result<usize> { self.get_commit_metadata_idx(head) })
            .collect::<Result<Vec<usize>>>()?;

        // Multiple children will have the same parent. Keep track of which indices we've walked to
        // avoid processing the same index twice
        let mut walked = HashSet::new();
        let mut child_indices: Vec<Vec<usize>> = Vec::new();

        while let Some(idx) = to_walk.pop() {
            if walked.contains(&idx) {
                continue;
            }

            walked.insert(idx);

            let parents = self.metadata_storage[idx].parents.clone();

            for parent in parents {
                let parent_idx = self.get_commit_metadata_idx(&parent)?;

                if child_indices.len() <= parent_idx {
                    child_indices.resize(parent_idx + 1, Vec::new());
                }
                child_indices[parent_idx].push(idx);
                to_walk.push(parent_idx);
            }
        }

        debug!(
            "Building reverse dag took: {}",
            timer.elapsed().as_secs_f32()
        );

        Ok((walked, child_indices))
    }

    pub(crate) fn index(&self) -> Result<IndexState> {
        let mut index = self.git2_repo.index().context("failed to get index")?;
        index.read(false).context("failed to refresh index")?;
        let mut files = HashMap::new();
        for entry in index.iter() {
            let path_s =
                String::from_utf8(entry.path).context("index entry does not have utf8 path")?;
            files.insert(path_s.into(), entry.id.into());
        }

        Ok(IndexState { files })
    }

    pub(crate) fn branches(&self) -> Result<impl Iterator<Item = Result<Reference>> + '_> {
        Ok(self
            .git2_repo
            .branches(None)?
            .map(|b| -> Result<Reference> {
                let (b, t) = b?;
                let name = b
                    .name()?
                    .ok_or_else(|| Error::msg("Invalid branch name"))?
                    .to_string();
                let id = match t {
                    git2::BranchType::Local => ReferenceId::LocalBranch(name),
                    git2::BranchType::Remote => ReferenceId::RemoteBranch(name),
                };
                Ok(Reference {
                    id,
                    head: git2_branch_object(b)?,
                })
            }))
    }

    pub(crate) fn tags(&self) -> Result<Vec<Reference>> {
        self.git2_repo
            .tag_names(None)?
            .iter()
            .flatten()
            .map(|t| -> Result<Option<Reference>> {
                let tag_refname = format!("refs/tags/{}", t);
                let reference = self
                    .git2_repo
                    .find_reference(&tag_refname)
                    .with_context(|| {
                        format!("Failed to resolve reference id for tag {}", tag_refname)
                    })?;

                let id = match reference.peel_to_commit() {
                    Ok(v) => v.id(),
                    Err(_e) => {
                        // It's possible for a tag to point directly to a tree (e.g. in the linux
                        // kernel). In this case we do not flag it as an error, but we do not show
                        // it because it doesn't fit our type well
                        return Ok(None);
                    }
                };

                Ok(Some(Reference {
                    id: ReferenceId::Tag(t.to_string()),
                    head: id.into(),
                }))
            })
            .filter_map(|t| t.transpose())
            .collect::<Result<_>>()
    }

    pub(crate) fn remote_refs(&self) -> Result<Vec<RemoteRef>> {
        let mut ret = Vec::new();
        for remote_name in &self.git2_repo.remotes()? {
            let remote_name = match remote_name {
                Some(v) => v,
                None => {
                    error!("Unexpected null remote name");
                    continue;
                }
            };

            let remote_refs = match get_refs_for_remote(&self.git_dir, remote_name) {
                Ok(v) => v,
                Err(e) => {
                    error!("Failed to get refs for remote {}: {}", remote_name, e);
                    continue;
                }
            };
            ret.extend(remote_refs.into_iter());
        }
        Ok(ret)
    }

    pub(crate) fn find_reference_commit_id(&self, id: &ReferenceId) -> Result<ObjectId> {
        let ref_name = id.reference_string()?;
        Ok(self
            .git2_repo
            .find_reference(&ref_name)?
            .resolve()?
            .peel_to_commit()?
            .id()
            .into())
    }

    pub(crate) fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub(crate) fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    pub(crate) fn resolve_reference(&self, id: &ReferenceId) -> Result<ReferenceId> {
        let ref_name = id.reference_string()?;
        self.git2_repo
            .find_reference(&ref_name)?
            .resolve()?
            .try_into()
    }

    pub(crate) fn modified_files(&self, id1: &ObjectId, id2: &ObjectId) -> Result<ModifiedFiles> {
        let t1_files =
            object_id_to_file_list(&self.git2_repo, id1).context("failed to get files for id1")?;
        let t2_files =
            object_id_to_file_list(&self.git2_repo, id2).context("failed to get files for id2")?;

        modified_files_between_trees(
            &self.git2_repo,
            DiffTarget::Object(id1.clone()),
            DiffTarget::Object(id2.clone()),
            &t1_files,
            &t2_files,
        )
    }

    pub(crate) fn modified_files_with_index(&self, id: &ObjectId) -> Result<ModifiedFiles> {
        let index_files =
            index_file_list(&self.git2_repo).context("failed to get files for index")?;
        let object_files = object_id_to_file_list(&self.git2_repo, id)
            .context("failed to get files for object")?;

        modified_files_between_trees(
            &self.git2_repo,
            DiffTarget::Object(id.clone()),
            DiffTarget::Index,
            &object_files,
            &index_files,
        )
    }

    pub(crate) fn modified_files_index_to_workdir(&self) -> Result<ModifiedFiles> {
        let modified_files = modified_files_in_dir(&self.repo_root, &self.git2_repo)
            .context("failed to find modified files")?;
        let index_files =
            index_file_list(&self.git2_repo).context("failed to get files for index")?;

        let mut workdir_files = index_files.clone();
        for file in modified_files {
            workdir_files.insert(
                file.clone().into_os_string().into_encoded_bytes(),
                FileListItem::Path(file),
            );
        }

        modified_files_between_trees(
            &self.git2_repo,
            DiffTarget::Index,
            DiffTarget::Workdir,
            &index_files,
            &workdir_files,
        )
    }

    pub(crate) fn is_ignored(&self, path: &Path) -> Result<bool> {
        let Ok(repo_relative_entry_path) = path.strip_prefix(&self.repo_root) else {
            return Ok(false);
        };

        self.git2_repo
            .is_path_ignored(repo_relative_entry_path)
            .context("failed to check ignore for file")
    }
}

fn git2_branch_object(branch: git2::Branch) -> Result<ObjectId> {
    Ok(branch
        .into_reference()
        .resolve()?
        .target()
        .ok_or_else(|| Error::msg("Failed to resolve reference"))?
        .into())
}

fn get_refs_for_remote(git_path: &Path, remote_name: &str) -> Result<Vec<RemoteRef>> {
    // NOTE: Github does not have as wide libgit2 support as the git CLI client due to libssh2 not
    // supporting SHA-2. This means that ssh keys that work with the git client will not work with
    // libgit2, which is an annoying and unexpected change.
    //
    // Use the git command line and parse the output
    let output = Command::new("git")
        .arg("-C")
        .arg(git_path)
        .args(["ls-remote", "-q", remote_name])
        .output()?;

    if !output.status.success() {
        let err = std::str::from_utf8(&output.stderr).unwrap_or("Failed to parse stderr");
        return Err(Error::msg(format!(
            "ls-remote failed for {}: {}",
            remote_name, err
        )));
    }

    let output_str = std::str::from_utf8(&output.stdout)?;
    output_str
        .lines()
        .map(|l| {
            let mut line_iter = l.split('\t');

            let ref_name = line_iter
                .nth(1)
                .context("Could not get reference id for remote ref")?
                .to_string();

            Ok(RemoteRef {
                remote: remote_name.to_string(),
                ref_name,
            })
        })
        .collect::<Result<Vec<_>>>()
}

fn build_sorted_metadata_indicies<'a>(
    sort_type: SortType,
    walked_indices: &HashSet<usize>,
    mut child_indices: Vec<Vec<usize>>,
    index_lookup: &HashMap<ObjectId, usize>,
    storage: &'a [CommitMetadata],
) -> Result<impl Iterator<Item = &'a CommitMetadata>> {
    // Effectively Kahn's algorithm but we choose insertion order based off timestamp

    assert!(child_indices.len() <= storage.len());

    let mut timer = Timer::new();

    let mut no_child_options = get_childless_indices(walked_indices, &child_indices);
    no_child_options.sort_by(|&a, &b| compare_commit_metadata(&storage[a], &storage[b], sort_type));
    debug!(
        "Filtering childless indices took: {}",
        timer.elapsed().as_secs_f32()
    );
    timer.reset();

    let mut sorted_indices = Vec::new();

    while let Some(idx) = no_child_options.pop() {
        sorted_indices.push(idx);

        let parent_indices = storage[idx]
            .parents
            .iter()
            .map(|parent| index_lookup[parent])
            .collect::<Vec<usize>>();

        for parent_idx in parent_indices {
            if let Some(v) = child_indices[parent_idx].iter().position(|&x| x == idx) {
                child_indices[parent_idx].remove(v);
            }

            if child_indices[parent_idx].is_empty() {
                let insertion_pos = match no_child_options.binary_search_by(|&x| {
                    compare_commit_metadata(&storage[x], &storage[parent_idx], sort_type)
                }) {
                    // Duplicate timestamps are fine
                    Ok(v) => v,
                    Err(v) => v,
                };
                no_child_options.insert(insertion_pos, parent_idx);
            }
        }
    }

    debug!(
        "Building sorted index list took: {}",
        timer.elapsed().as_secs_f32()
    );

    Ok(sorted_indices.into_iter().map(|x| &storage[x]))
}

fn compare_commit_metadata(
    a: &CommitMetadata,
    b: &CommitMetadata,
    sort_type: SortType,
) -> std::cmp::Ordering {
    match sort_type {
        SortType::AuthorTimestamp => a.author_timestamp.cmp(&b.author_timestamp),
        SortType::CommitterTimestamp => a.committer_timestamp.cmp(&b.committer_timestamp),
    }
}

/// Find the indices in child_indices where there are no children
fn get_childless_indices(
    walked_indices: &HashSet<usize>,
    child_indices: &[Vec<usize>],
) -> Vec<usize> {
    walked_indices
        .iter()
        .filter_map(|idx| match child_indices.get(*idx) {
            Some(v) => {
                if v.is_empty() {
                    Some(*idx)
                } else {
                    None
                }
            }
            None => Some(*idx),
        })
        .collect()
}

fn find_pack_paths(git_dir: &Path) -> Result<Vec<PathBuf>> {
    let pack_dir = git_dir.join("objects/pack");

    if !pack_dir.is_dir() {
        return Ok(Vec::new());
    }

    fs::read_dir(&pack_dir)
        .context("Failed to read pack dir")?
        .filter_map(|e| match e {
            Ok(v) => {
                if v.path().extension() == Some(std::ffi::OsStr::new("pack")) {
                    Some(Ok(v.path()))
                } else {
                    None
                }
            }
            Err(e) => Some(Err(anyhow::Error::from(e))),
        })
        .collect()
}

fn find_packs(git_dir: &Path) -> Result<Vec<Pack>> {
    find_pack_paths(git_dir)?
        .into_iter()
        .map(|p| Pack::new(&p))
        .collect()
}

#[derive(Clone, Hash, Eq, PartialEq)]
enum FileListItem {
    Object(git2::Oid),
    Commit(git2::Oid),
    Path(PathBuf),
}

fn index_file_list(git2_repo: &git2::Repository) -> Result<HashMap<Vec<u8>, FileListItem>> {
    let mut ret = HashMap::new();
    let mut index = git2_repo.index().context("failed to get index")?;
    index.read(false).unwrap();
    for entry in index.iter() {
        let id = entry.id;
        let path = entry.path;
        // FIXME: Check object type (i.e. tree, commit, submodule, etc.) like in
        // object_id_to_file_list
        ret.insert(path, FileListItem::Object(id));
    }

    Ok(ret)
}

fn object_id_to_file_list(
    git2_repo: &git2::Repository,
    id: &ObjectId,
) -> Result<HashMap<Vec<u8>, FileListItem>> {
    let oid1 = id.into();
    let tree = git2_repo.find_commit(oid1)?.tree()?;
    let mut ret = HashMap::new();

    let walk_insert_filename = |root: &str, entry: &TreeEntry, files: &mut HashMap<_, _>| {
        let mut full_path = root.as_bytes().to_vec();
        full_path.extend(entry.name_bytes());
        match entry.kind() {
            Some(git2::ObjectType::Tree) => {
                return TreeWalkResult::Ok;
            }
            Some(git2::ObjectType::Commit) => {
                files.insert(full_path, FileListItem::Commit(entry.id()));
            }
            _ => {
                files.insert(full_path, FileListItem::Object(entry.id()));
            }
        }

        TreeWalkResult::Ok
    };

    tree.walk(TreeWalkMode::PreOrder, |root, entry| {
        walk_insert_filename(root, entry, &mut ret)
    })
    .context("Failed to walk tree 1")?;

    Ok(ret)
}

fn modified_files_between_trees(
    git2_repo: &git2::Repository,
    id1: DiffTarget,
    id2: DiffTarget,
    t1_files: &HashMap<Vec<u8>, FileListItem>,
    t2_files: &HashMap<Vec<u8>, FileListItem>,
) -> Result<ModifiedFiles> {
    let mut changed_paths = t1_files
        .iter()
        .filter_map(|(path, id)| {
            if t2_files.get(path) == Some(id) {
                None
            } else {
                Some(path.clone())
            }
        })
        .collect::<BTreeSet<Vec<u8>>>();

    for (path, id) in t2_files.iter() {
        if t1_files.get(path) != Some(id) {
            changed_paths.insert(path.clone());
        }
    }

    let paths_to_contents = |oid_lookup: &HashMap<Vec<u8>, FileListItem>| {
        changed_paths
            .iter()
            .map(|filename| -> Result<Option<_>> {
                let id = match oid_lookup.get(filename) {
                    Some(v) => v,
                    None => return Ok(None),
                };

                let id = match id {
                    FileListItem::Commit(id) => {
                        let stringized = format!("Subproject commit {}", id);
                        return Ok(Some(stringized.into_bytes()));
                    }
                    FileListItem::Object(id) => id,
                    FileListItem::Path(path) => {
                        return Ok(Some(fs::read(path).context("failed to read workdir data")?))
                    }
                };

                let object = git2_repo
                    .find_object(*id, None)
                    .context("Failed to retrieve object")?;

                if let Some(blob) = object.as_blob() {
                    Ok(Some(blob.content().to_vec()))
                } else {
                    let description = object
                        .describe(&git2::DescribeOptions::default())
                        .context("Failed to generate description for object")?;
                    let stringized = description
                        .format(None)
                        .context("Failed to stringize description")?;
                    Ok(Some(stringized.into_bytes()))
                }
            })
            .collect::<Result<Vec<Option<Vec<u8>>>>>()
    };

    let content_1 =
        paths_to_contents(t1_files).context("Failed to retrieve file content for tree 1")?;
    let content_2 =
        paths_to_contents(t2_files).context("Failed to retrieve file content for tree 2")?;
    let labels = changed_paths
        .iter()
        .map(|x| String::from_utf8_lossy(x).to_string())
        .collect::<Vec<_>>();

    Ok(ModifiedFiles {
        id_a: id1,
        id_b: id2,
        files_a: content_1,
        files_b: content_2,
        labels,
    })
}

fn modified_files_in_dir_impl(
    root: &Path,
    path: &Path,
    git2_repo: &git2::Repository,
    output: &mut Vec<PathBuf>,
    index: &git2::Index,
) -> Result<()> {
    let dir_iter = fs::read_dir(path).context("failed to get directory iterator")?;

    for entry in dir_iter {
        let entry = entry.context("failed to iterate directory")?;
        let entry_path = entry.path();
        debug!("Checking modified for {:?}", entry_path);

        let repo_relative_entry_path = entry_path
            .strip_prefix(root)
            .expect("failed to strip root from entry path")
            .to_path_buf();
        debug!("repo relative path: {:?}", repo_relative_entry_path);

        if git2_repo
            .is_path_ignored(&repo_relative_entry_path)
            .context("failed to check ignore for file")?
        {
            debug!("{:?} ignored", entry_path);
            continue;
        }

        if entry
            .file_type()
            .context("failed to get file type of entry")?
            .is_dir()
        {
            modified_files_in_dir_impl(root, &entry_path, git2_repo, output, index)?;
            continue;
        }

        // git2/index.h git_index_stage_t
        // https://libgit2.org/libgit2/ex/HEAD/ls-files.html#git_index_get_bypath-4
        // NOTE: Wanted to use stage_any, but index entry is always none in that case
        // FIXME: Need to think about merge conflicts later
        const STAGE_NORMAL: i32 = 0;
        let index_entry = match index.get_path(&repo_relative_entry_path, STAGE_NORMAL) {
            Some(v) => v,
            None => {
                debug!("File not found in index");
                output.push(repo_relative_entry_path);
                continue;
            }
        };

        let index_entry_blob = git2_repo
            .find_blob(index_entry.id)
            .context("failed to find blob")?;

        let entry_content = fs::read(&entry_path).context("failed to read content of entry")?;
        let index_content = index_entry_blob.content();

        if entry_content != index_content {
            debug!("Content did not match: {entry_content:?}, {index_content:?}");
            output.push(repo_relative_entry_path);
        }
    }
    Ok(())
}

fn modified_files_in_dir(path: &Path, git2_repo: &git2::Repository) -> Result<Vec<PathBuf>> {
    let mut ret = Vec::new();
    let mut index = git2_repo.index().context("failed to retrieve index")?;
    index.read(false).context("failed to update index")?;
    modified_files_in_dir_impl(path, path, git2_repo, &mut ret, &index)?;
    Ok(ret)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    const GIT_DIR_TARBALL: &[u8] =
        include_bytes!("../../res/test/multi_obj_multi_pack_octopus_merge.tar");
    #[test]
    fn test_unpacked() -> Result<()> {
        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL)
            .unpack(git_dir.path())
            .unwrap();

        let mut history = Repo::new(git_dir.path().to_path_buf(), false)?;

        let oid = "83fc68fe02d76e37231b8f880bca5f151cb62e39".parse()?;
        let expected_parent: ObjectId = "ce4f6371c0a653f6206e4020704674d63fc8e3d4".parse()?;
        let metadata = history.get_commit_metadata(&oid)?;

        assert_eq!(metadata.parents.len(), 1);
        assert_ne!(
            metadata.parents.iter().find(|id| **id == expected_parent),
            None
        );

        Ok(())
    }

    #[test]
    fn test_pack_1() -> Result<()> {
        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL)
            .unpack(git_dir.path())
            .unwrap();

        let mut history = Repo::new(git_dir.path().to_path_buf(), false)?;

        let oid = "760e2389d32e245213eaf71d88e314fa63709c79".parse()?;
        let expected_parent: ObjectId = "54c637bcfcaab19064ac59db025bc05d941a3bf3".parse()?;
        let metadata = history.get_commit_metadata(&oid)?;

        assert_eq!(metadata.parents.len(), 1);
        assert_ne!(
            metadata.parents.iter().find(|id| **id == expected_parent),
            None
        );

        Ok(())
    }

    #[test]
    fn test_pack_2() -> Result<()> {
        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL)
            .unpack(git_dir.path())
            .unwrap();

        let pack = Pack::new(
            &git_dir
                .path()
                .join(".git/objects/pack/pack-d263ed5546c1c402dad86f0970272add736ccb1f.pack"),
        )?;

        let oid = "bf57fac4272accfb0a0af73d1648bb406a8e84a2".parse()?;
        let expected_parent_1: ObjectId = "93fc7325bad6205598b6cc601bbdb75d0eab5c48".parse()?;
        let expected_parent_2: ObjectId = "cee9d1a5528b2a8731d79bbb30de24c4a05a8937".parse()?;
        let expected_parent_3: ObjectId = "43ffc82ef7b65acaa19f589a62eba882c8f0ad69".parse()?;

        let metadata = pack
            .get_commit_metadata(oid)?
            .expect("Expected to find commit");

        assert_eq!(metadata.parents.len(), 3);
        assert_ne!(
            metadata.parents.iter().find(|id| **id == expected_parent_1),
            None
        );
        assert_ne!(
            metadata.parents.iter().find(|id| **id == expected_parent_2),
            None
        );
        assert_ne!(
            metadata.parents.iter().find(|id| **id == expected_parent_3),
            None
        );

        Ok(())
    }

    #[test]
    fn test_find_pack_paths() -> Result<()> {
        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL)
            .unpack(git_dir.path())
            .unwrap();

        let packs = find_pack_paths(&git_dir.path().join(".git"))?;

        let expected_pack_1 = git_dir
            .path()
            .join(".git/objects/pack/pack-66c4253986146290e8d86a6057cb8b076f43c325.pack");
        let expected_pack_2 = git_dir
            .path()
            .join(".git/objects/pack/pack-d263ed5546c1c402dad86f0970272add736ccb1f.pack");

        assert_eq!(packs.len(), 2);
        assert_ne!(packs.iter().find(|p| **p == expected_pack_1), None);
        assert_ne!(packs.iter().find(|p| **p == expected_pack_2), None);

        Ok(())
    }

    #[test]
    fn test_branches() -> Result<()> {
        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL).unpack(git_dir.path())?;

        let git_dir2 = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL).unpack(git_dir2.path())?;

        Command::new("git")
            .arg("-C")
            .arg(git_dir2.path())
            .args(&[
                "branch",
                "test_branch",
                "760e2389d32e245213eaf71d88e314fa63709c79",
            ])
            .output()?;

        Command::new("git")
            .arg("-C")
            .arg(git_dir.path())
            .args(&["remote", "add", "origin"])
            .arg(git_dir2.path())
            .output()?;

        Command::new("git")
            .arg("-C")
            .arg(git_dir.path())
            .args(&["fetch", "origin"])
            .output()?;

        Command::new("git")
            .arg("-C")
            .arg(git_dir.path())
            .args(&[
                "branch",
                "test_branch",
                "ce4f6371c0a653f6206e4020704674d63fc8e3d4",
            ])
            .output()?;

        let repo = Repo::new(git_dir.path().to_path_buf(), false)?;
        let mut branches = repo.branches()?.collect::<Result<Vec<_>>>()?;
        branches.sort();

        assert_eq!(
            branches,
            &[
                Reference {
                    id: ReferenceId::LocalBranch("master".to_string()),
                    head: "83fc68fe02d76e37231b8f880bca5f151cb62e39".parse()?
                },
                Reference {
                    id: ReferenceId::LocalBranch("test_branch".to_string()),
                    head: "ce4f6371c0a653f6206e4020704674d63fc8e3d4".parse()?
                },
                Reference {
                    id: ReferenceId::RemoteBranch("origin/master".to_string()),
                    head: "83fc68fe02d76e37231b8f880bca5f151cb62e39".parse()?
                },
                Reference {
                    id: ReferenceId::RemoteBranch("origin/test_branch".to_string()),
                    head: "760e2389d32e245213eaf71d88e314fa63709c79".parse()?
                },
            ]
        );

        Ok(())
    }

    #[test]
    fn test_lookup_branch_head() -> Result<()> {
        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL).unpack(git_dir.path())?;

        let git_dir2 = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL).unpack(git_dir2.path())?;

        Command::new("git")
            .arg("-C")
            .arg(git_dir.path())
            .args(&["remote", "add", "origin"])
            .arg(git_dir2.path())
            .output()?;

        Command::new("git")
            .arg("-C")
            .arg(git_dir.path())
            .args(&["fetch", "origin"])
            .output()?;

        Command::new("git")
            .arg("-C")
            .arg(git_dir.path())
            .args(&[
                "branch",
                "test_branch",
                "ce4f6371c0a653f6206e4020704674d63fc8e3d4",
            ])
            .output()?;

        let repo = Repo::new(git_dir.path().to_path_buf(), false)?;

        let head =
            repo.find_reference_commit_id(&ReferenceId::LocalBranch("test_branch".into()))?;
        assert_eq!(head, "ce4f6371c0a653f6206e4020704674d63fc8e3d4".parse()?);

        let head = repo.find_reference_commit_id(&ReferenceId::head())?;
        assert_eq!(head, "83fc68fe02d76e37231b8f880bca5f151cb62e39".parse()?);

        let head =
            repo.find_reference_commit_id(&ReferenceId::RemoteBranch("origin/master".into()))?;
        assert_eq!(head, "83fc68fe02d76e37231b8f880bca5f151cb62e39".parse()?);
        Ok(())
    }

    #[test]
    fn test_new_commit() -> Result<()> {
        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL).unpack(git_dir.path())?;

        let mut repo = Repo::new(git_dir.path().to_path_buf(), false)?;
        let original_head = repo
            .metadata_iter(
                &["83fc68fe02d76e37231b8f880bca5f151cb62e39".parse()?],
                SortType::CommitterTimestamp,
            )?
            .next()
            .unwrap()
            .clone();

        Command::new("git")
            .arg("-C")
            .arg(git_dir.path())
            .args(&["commit", "-m", "testing", "--allow-empty"])
            .output()?;

        let object_id = repo.find_reference_commit_id(&ReferenceId::head())?;
        let new_head = repo
            .metadata_iter(&[object_id], SortType::CommitterTimestamp)?
            .next()
            .unwrap();

        assert_ne!(original_head.id, new_head.id);
        assert_eq!(new_head.parents.len(), 1);
        assert_eq!(new_head.parents, &[original_head.id]);

        Ok(())
    }

    #[test]
    fn test_refdelta_pack() -> Result<()> {
        const GIT_DIR_TARBALL: &[u8] =
            include_bytes!("../../res/test/two_commits_ref_delta_pack.tar");

        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL).unpack(git_dir.path())?;

        // We allow libgit2 fallback here because our refdelta parser is not yet implemented
        let mut repo = Repo::new(git_dir.path().to_path_buf(), true)?;

        let it = repo.metadata_iter(
            &["a0dc968acca0ab483897a600b50e7b372960a509".parse()?],
            SortType::CommitterTimestamp,
        )?;

        let commits = it.collect::<Vec<_>>();
        assert_eq!(commits.len(), 2);
        assert_eq!(
            commits[0].id,
            "a0dc968acca0ab483897a600b50e7b372960a509".parse()?
        );
        assert_eq!(
            commits[1].id,
            "7686bd4e339afa6ef86c5638049c75e19e5a8943".parse()?
        );

        Ok(())
    }

    #[test]
    fn test_modified_files() -> Result<()> {
        const GIT_DIR_TARBALL: &[u8] = include_bytes!("../../res/test/modified_file_test.tar");

        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL).unpack(git_dir.path())?;

        let repo = Repo::new(git_dir.path().to_path_buf().join("repo"), false)?;

        let modified_files = repo.modified_files(
            &"491819c1d0e44904c905d9daac719a2eb990a5f1".parse()?,
            &"25fa40a48f04500736c199e1b0448ca3bf2c7e52".parse()?,
        )?;

        assert_eq!(modified_files.labels.len(), 4);
        assert!(modified_files.labels.contains(&"test.txt".to_string()));
        assert!(modified_files.labels.contains(&"test2.txt".to_string()));
        assert!(modified_files
            .labels
            .contains(&"test_binary_file".to_string()));
        assert!(modified_files.labels.contains(&"submodule".to_string()));

        let compare_content_a = |file_name, content| {
            let position = modified_files
                .labels
                .iter()
                .position(|x| x == file_name)
                .unwrap();
            assert_eq!(modified_files.files_a[position], content);
        };

        compare_content_a("test.txt", Some("Test text file\n".as_bytes().to_vec()));
        compare_content_a("test2.txt", None);
        compare_content_a(
            "submodule",
            Some(
                "Subproject commit 73f89df6a3c049523eafd798092b1aaf60944ac2"
                    .as_bytes()
                    .to_vec(),
            ),
        );

        let compare_content_b = |file_name, content| {
            let position = modified_files
                .labels
                .iter()
                .position(|x| x == file_name)
                .unwrap();
            assert_eq!(modified_files.files_b[position], content);
        };

        compare_content_b("test.txt", None);
        compare_content_b(
            "test2.txt",
            Some("Modified test text file\n".as_bytes().to_vec()),
        );
        compare_content_b(
            "submodule",
            Some(
                "Subproject commit f4df290482ada1325c22e1907cb2bf1e7819afa7"
                    .as_bytes()
                    .to_vec(),
            ),
        );

        // No point in testing random binary data

        Ok(())
    }

    #[test]
    fn test_pack_reload() -> Result<()> {
        // If we fetch more commits than fetch.unpackLimit, we may end up in a situation where
        // metadata is not in our metadata cache, but goes directly to a new packfile that we have
        // not parsed yet. Previously this resulted in unfindable metadata because we only looked
        // for packfiles when we opened the git dir

        const GIT_DIR_TARBALL: &[u8] = include_bytes!("../../res/test/test_pack_reload.tar");

        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL).unpack(git_dir.path())?;

        let git_dir = git_dir.path().to_path_buf().join("child_repo");

        let mut repo = Repo::new(git_dir.clone(), false)?;

        Command::new("git")
            .current_dir(&git_dir)
            .arg("fetch")
            .arg("--all")
            .output()?;

        let origin_master =
            repo.find_reference_commit_id(&ReferenceId::RemoteBranch("origin/master".to_string()))?;

        let commits = repo
            .metadata_iter(&[origin_master], SortType::CommitterTimestamp)?
            .collect::<Vec<_>>();
        assert_eq!(commits.len(), 7);
        let first_commit = commits[0].clone();
        repo.get_commit(&first_commit.id)?;

        Ok(())
    }
}
