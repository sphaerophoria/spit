use crate::{
    git::{
        decompress, pack::Pack, Branch, Commit, CommitMetadata, Diff, DiffContent, DiffFileHeader,
        DiffMetadata, ObjectId, ReferenceId,
    },
    util::Timer,
};

use anyhow::{Context, Error, Result};
use flate2::Decompress;
use log::{debug, error};

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum SortType {
    AuthorTimestamp,
    CommitterTimestamp,
}

impl Default for SortType {
    fn default() -> SortType {
        SortType::CommitterTimestamp
    }
}

pub(crate) struct Repo {
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
    pub(crate) fn new(repo_root: PathBuf) -> Result<Repo> {
        let decompressor = Decompress::new(true);
        let git2_repo =
            git2::Repository::open(repo_root.clone()).context("Failed to open git2 repo")?;

        let git_dir = git2_repo.path().to_path_buf();
        let packs = find_packs(&git_dir)?;

        Ok(Repo {
            git2_repo,
            repo_root,
            git_dir,
            packs,
            metadata_lookup: HashMap::new(),
            metadata_storage: Vec::new(),
            decompressor,
        })
    }

    #[allow(unused)]
    pub(crate) fn get_commit_metadata(&mut self, id: &ObjectId) -> Result<Option<&CommitMetadata>> {
        Ok(self
            .get_commit_metadata_idx(id)?
            .map(|idx| &self.metadata_storage[idx]))
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
            .context("Failed to lookup commit metadata")?
            .ok_or_else(|| Error::msg("No metadata for commit"))?;

        Ok(Commit {
            metadata: metadata.clone(),
            message,
            author,
        })
    }

    /// Private implementation of get_commit_metadata that returns the vector index instead of a
    /// reference to dodge ownership rules associated with handing out CommitMetadata references
    /// when walking our history
    fn get_commit_metadata_idx(&mut self, id: &ObjectId) -> Result<Option<usize>> {
        // FIXME: This function does not read nicely at all...

        if let Some(idx) = self.metadata_lookup.get(id) {
            return Ok(Some(*idx));
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
            Ok(Some(storage_idx))
        } else {
            for pack in &mut self.packs {
                match pack.get_commit_metadata(id.clone()) {
                    Ok(Some(metadata)) => {
                        self.metadata_lookup
                            .insert(metadata.id.clone(), storage_idx);
                        self.metadata_storage.push(metadata);
                        return Ok(Some(storage_idx));
                    }
                    Ok(None) => continue,
                    Err(e) => return Err(e),
                }
            }
            Ok(None)
        }
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
        // FIXME: Fall back on libgit2 on failure

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
            .map(|head| -> Result<usize> {
                self.get_commit_metadata_idx(head)?
                    .ok_or_else(|| Error::msg("Repository does not have requested id"))
            })
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
                let parent_idx = self
                    .get_commit_metadata_idx(&parent)?
                    .ok_or_else(|| Error::msg("Parent is not present"))?;

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

    pub(crate) fn branches(&self) -> Result<impl Iterator<Item = Result<Branch>> + '_> {
        Ok(self.git2_repo.branches(None)?.map(|b| -> Result<Branch> {
            let (b, t) = b?;
            let name = b
                .name()?
                .ok_or_else(|| Error::msg("Invalid branch name"))?
                .to_string();
            let id = match t {
                git2::BranchType::Local => ReferenceId::LocalBranch(name),
                git2::BranchType::Remote => ReferenceId::RemoteBranch(name),
            };
            Ok(Branch {
                id,
                head: git2_branch_object(b)?,
            })
        }))
    }

    pub(crate) fn find_reference_object(&self, id: &ReferenceId) -> Result<ObjectId> {
        let ref_name = id.reference_string()?;
        Ok(self
            .git2_repo
            .find_reference(&ref_name)?
            .resolve()?
            .target()
            .ok_or_else(|| Error::msg("Failed to resolve reference"))?
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

    pub(crate) fn diff(
        &self,
        id1: &ObjectId,
        id2: &ObjectId,
        ignore_whitespace: bool,
    ) -> Result<Diff> {
        let oid1 = id1.into();
        let oid2 = id2.into();

        let t1 = self.git2_repo.find_commit(oid1)?.tree()?;
        let t2 = self.git2_repo.find_commit(oid2)?.tree()?;

        let mut options = git2::DiffOptions::new();
        options.ignore_whitespace(ignore_whitespace);
        let diff = self
            .git2_repo
            .diff_tree_to_tree(Some(&t1), Some(&t2), Some(&mut options))?;

        let mut current_hunk_header = String::new();
        let mut output = Diff {
            metadata: DiffMetadata {
                from: id1.clone(),
                to: id2.clone(),
                ignore_whitespace,
            },
            items: Default::default(),
        };
        let mut binary_files = Vec::new();

        diff.foreach(
            &mut |_d, _f| true,
            Some(&mut |delta, _blob| {
                let file_header = DiffFileHeader {
                    old_file: delta.old_file().path().map(|x| x.to_path_buf()),
                    new_file: delta.new_file().path().map(|x| x.to_path_buf()),
                };

                binary_files.push(file_header);
                true
            }),
            None,
            Some(&mut |delta, hunk, line| {
                let file_header = DiffFileHeader {
                    old_file: delta.old_file().path().map(|x| x.to_path_buf()),
                    new_file: delta.new_file().path().map(|x| x.to_path_buf()),
                };

                if let Some(hunk) = hunk {
                    current_hunk_header = std::str::from_utf8(hunk.header()).unwrap().to_string();
                }

                let file_entry = output
                    .items
                    .entry(file_header)
                    .or_insert_with(|| DiffContent::Patch(Default::default()));
                let file_entry = match file_entry {
                    DiffContent::Binary => {
                        error!("Found line diff for binary file");
                        return true;
                    }
                    DiffContent::Patch(v) => v,
                };
                let v = file_entry
                    .entry(current_hunk_header.clone())
                    .or_insert_with(Vec::new);

                v.push(line.origin() as u8);
                v.extend_from_slice(line.content());
                true
            }),
        )?;

        output
            .items
            .extend(binary_files.into_iter().map(|f| (f, DiffContent::Binary)));

        Ok(output)
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

        let mut history = Repo::new(git_dir.path().to_path_buf())?;

        let oid = "83fc68fe02d76e37231b8f880bca5f151cb62e39".parse()?;
        let expected_parent: ObjectId = "ce4f6371c0a653f6206e4020704674d63fc8e3d4".parse()?;
        let metadata = history
            .get_commit_metadata(&oid)?
            .expect("Expected to find commit");

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

        let mut history = Repo::new(git_dir.path().to_path_buf())?;

        let oid = "760e2389d32e245213eaf71d88e314fa63709c79".parse()?;
        let expected_parent: ObjectId = "54c637bcfcaab19064ac59db025bc05d941a3bf3".parse()?;
        let metadata = history
            .get_commit_metadata(&oid)?
            .expect("Expected to find commit");

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

        let repo = Repo::new(git_dir.path().to_path_buf())?;
        let mut branches = repo.branches()?.collect::<Result<Vec<_>>>()?;
        branches.sort();

        assert_eq!(
            branches,
            &[
                Branch {
                    id: ReferenceId::LocalBranch("master".to_string()),
                    head: "83fc68fe02d76e37231b8f880bca5f151cb62e39".parse()?
                },
                Branch {
                    id: ReferenceId::LocalBranch("test_branch".to_string()),
                    head: "ce4f6371c0a653f6206e4020704674d63fc8e3d4".parse()?
                },
                Branch {
                    id: ReferenceId::RemoteBranch("origin/master".to_string()),
                    head: "83fc68fe02d76e37231b8f880bca5f151cb62e39".parse()?
                },
                Branch {
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

        let repo = Repo::new(git_dir.path().to_path_buf())?;

        let head = repo.find_reference_object(&ReferenceId::LocalBranch("test_branch".into()))?;
        assert_eq!(head, "ce4f6371c0a653f6206e4020704674d63fc8e3d4".parse()?);

        let head = repo.find_reference_object(&ReferenceId::head())?;
        assert_eq!(head, "83fc68fe02d76e37231b8f880bca5f151cb62e39".parse()?);

        let head =
            repo.find_reference_object(&ReferenceId::RemoteBranch("origin/master".into()))?;
        assert_eq!(head, "83fc68fe02d76e37231b8f880bca5f151cb62e39".parse()?);
        Ok(())
    }

    #[test]
    fn test_new_commit() -> Result<()> {
        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL).unpack(git_dir.path())?;

        let mut repo = Repo::new(git_dir.path().to_path_buf())?;
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

        let object_id = repo.find_reference_object(&ReferenceId::head())?;
        let new_head = repo
            .metadata_iter(&[object_id], SortType::CommitterTimestamp)?
            .next()
            .unwrap();

        assert_ne!(original_head.id, new_head.id);
        assert_eq!(new_head.parents.len(), 1);
        assert_eq!(new_head.parents, &[original_head.id]);

        Ok(())
    }
}
