use crate::{
    git::{decompress, pack::Pack, Branch, Commit, CommitMetadata, ObjectId},
    Timer,
};

use anyhow::{Context, Error, Result};
use flate2::Decompress;
use log::debug;

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

pub(crate) struct Repo {
    git2_repo: git2::Repository,
    git_dir: PathBuf,
    packs: Vec<Pack>,
    // NOTE: We do not store the commit metadata within the hashmap directly because it makes it
    // difficult to hand out references to the metadata without copying it out. Instead we hand out
    // metadata IDs that look up the CommitMetadata on demand.
    metadata_lookup: HashMap<ObjectId, usize>,
    metadata_storage: Vec<CommitMetadata>,
    decompressor: Decompress,
}

impl Repo {
    pub(crate) fn new(repo_root: &Path) -> Result<Repo> {
        let git_dir = repo_root.join(".git");
        let packs = find_packs(&git_dir)?;
        let decompressor = Decompress::new(true);
        let git2_repo = git2::Repository::open(repo_root).context("Failed to open git2 repo")?;

        Ok(Repo {
            git2_repo,
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
            author
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
    ) -> Result<impl Iterator<Item = &CommitMetadata>> {
        // FIXME: Fall back on libgit2 on failure

        let child_indices = self.build_reverse_dag(heads)?;

        // NOTE: From this point on it's guaranteed that all parents of heads are in our
        // metadata_storage, so from this point on it's safe for us to use the metadata storage
        // directly

        build_sorted_metadata_indicies(child_indices, &self.metadata_lookup, &self.metadata_storage)
    }

    /// Build the reversed dag for the given heads. The output is a Vec of Vecs that represents the
    /// child indices for each metadata_storage index
    fn build_reverse_dag(&mut self, heads: &[ObjectId]) -> Result<Vec<Vec<usize>>> {
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

        Ok(child_indices)
    }

    pub(crate) fn branches(&self) -> Result<impl Iterator<Item = Result<Branch>> + '_> {
        Ok(self.git2_repo.branches(None)?.map(|b| -> Result<Branch> {
            let b = b?.0;
            let name = b.name()?.ok_or_else(|| Error::msg("Invalid branch name"))?;
            let name = name.to_string();
            let reference = b.into_reference().resolve()?;
            let oid = reference
                .target()
                .ok_or_else(|| Error::msg("Failed to resolve reference"))?;
            Ok(Branch {
                name,
                head: oid.into(),
            })
        }))
    }

    pub(crate) fn git_dir(&self) -> &Path {
        &self.git_dir
    }
}

fn build_sorted_metadata_indicies<'a>(
    mut child_indices: Vec<Vec<usize>>,
    index_lookup: &HashMap<ObjectId, usize>,
    storage: &'a [CommitMetadata],
) -> Result<impl Iterator<Item = &'a CommitMetadata>> {
    // Effectively Kahn's algorithm but we choose insertion order based off timestamp

    assert!(child_indices.len() <= storage.len());

    let mut timer = Timer::new();

    let mut no_child_options = get_childless_indices(&child_indices);
    sort_commit_metadata_indices_by_timestamp(&mut no_child_options, storage);
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
                let insertion_pos = match no_child_options
                    .binary_search_by(|&x| storage[x].timestamp.cmp(&storage[parent_idx].timestamp))
                {
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

/// Sorts the indices so that the latest metadata indices are at the end of the array
fn sort_commit_metadata_indices_by_timestamp(indices: &mut [usize], storage: &[CommitMetadata]) {
    indices.sort_by(|&a, &b| storage[a].timestamp.cmp(&storage[b].timestamp));
}

/// Find the indices in child_indices where there are no children
fn get_childless_indices(child_indices: &[Vec<usize>]) -> Vec<usize> {
    child_indices
        .iter()
        .enumerate()
        .filter_map(|(idx, child_indices)| {
            if child_indices.is_empty() {
                Some(idx)
            } else {
                None
            }
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
    use tempfile::TempDir;

    const GIT_DIR_TARBALL: &[u8] =
        include_bytes!("../../res/test/multi_obj_multi_pack_octopus_merge.tar");
    #[test]
    fn test_unpacked() -> Result<()> {
        let git_dir = TempDir::new()?;
        tar::Archive::new(GIT_DIR_TARBALL)
            .unpack(git_dir.path())
            .unwrap();

        let mut history = Repo::new(&git_dir.path().to_path_buf())?;

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

        let mut history = Repo::new(&git_dir.path().to_path_buf())?;

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
}
