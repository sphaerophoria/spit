mod decompress;
pub(crate) mod graph;
mod object_id;
mod pack;
mod repo;

pub(crate) use graph::{build_git_history_graph, HistoryGraph};
pub(crate) use object_id::ObjectId;
pub(crate) use repo::Repo;

use anyhow::{Error, Result};
use chrono::{DateTime, Utc};
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::PathBuf,
};

#[derive(Debug, Clone)]
pub(crate) struct CommitMetadata {
    pub(crate) id: ObjectId,
    pub(crate) parents: Vec<ObjectId>,
    pub(crate) timestamp: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReferenceId {
    Symbolic(String),
    LocalBranch(String),
    RemoteBranch(String),
    Unknown,
}

impl Default for ReferenceId {
    fn default() -> Self {
        ReferenceId::Unknown
    }
}

impl ReferenceId {
    pub(crate) fn head() -> ReferenceId {
        ReferenceId::Symbolic("HEAD".to_string())
    }

    pub(crate) fn reference_string(&self) -> Result<String> {
        let s = match self {
            ReferenceId::Symbolic(name) => name.clone(),
            ReferenceId::LocalBranch(name) => format!("refs/heads/{}", name),
            ReferenceId::RemoteBranch(name) => format!("refs/remotes/{}", name),
            ReferenceId::Unknown => {
                return Err(Error::msg("Cannot find object id of unknown reference"));
            }
        };

        Ok(s)
    }
}

impl fmt::Display for ReferenceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReferenceId::Symbolic(name)
            | ReferenceId::RemoteBranch(name)
            | ReferenceId::LocalBranch(name) => f.write_str(name)?,
            ReferenceId::Unknown => f.write_str("Unknown")?,
        }

        Ok(())
    }
}

impl<'a> TryFrom<git2::Reference<'a>> for ReferenceId {
    type Error = Error;
    fn try_from(r: git2::Reference<'a>) -> Result<Self> {
        TryFrom::try_from(&r)
    }
}

impl<'a> TryFrom<&git2::Reference<'a>> for ReferenceId {
    type Error = Error;
    fn try_from(r: &git2::Reference<'a>) -> Result<Self> {
        let name = r
            .name()
            .ok_or_else(|| Error::msg("Branch name is invalid"))?;
        let id = if r.is_branch() {
            if r.is_remote() {
                const REMOTES_START: &str = "refs/remotes/";
                if !name.starts_with(REMOTES_START) {
                    return Err(Error::msg(format!(
                        "{} does not start with {}",
                        name, REMOTES_START
                    )));
                }
                ReferenceId::RemoteBranch(name[REMOTES_START.len()..].to_string())
            } else {
                const LOCAL_START: &str = "refs/heads/";
                if !name.starts_with(LOCAL_START) {
                    return Err(Error::msg(format!(
                        "{} does not start with {}",
                        name, LOCAL_START
                    )));
                }
                ReferenceId::LocalBranch(name[LOCAL_START.len()..].to_string())
            }
        } else {
            ReferenceId::Unknown
        };
        Ok(id)
    }
}

#[derive(Debug, Eq, PartialEq, Clone, PartialOrd, Ord)]
pub struct Branch {
    pub(crate) id: ReferenceId,
    pub(crate) head: ObjectId,
}

#[derive(Debug, Clone)]
pub struct Commit {
    pub(crate) metadata: CommitMetadata,
    pub(crate) message: String,
    pub(crate) author: String,
}

#[derive(Debug, Clone)]
struct CommitMetadataWithoutId {
    pub(crate) parents: Vec<ObjectId>,
    pub(crate) timestamp: DateTime<Utc>,
}

impl CommitMetadataWithoutId {
    fn into_full_metadata(self, id: ObjectId) -> CommitMetadata {
        CommitMetadata {
            id,
            parents: self.parents,
            timestamp: self.timestamp,
        }
    }
}

#[derive(Eq, PartialEq, Hash, Clone, Ord, PartialOrd)]
pub(crate) struct DiffFileHeader {
    old_file: Option<PathBuf>,
    new_file: Option<PathBuf>,
}

impl fmt::Display for DiffFileHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.old_file, &self.new_file) {
            (Some(old_file), None) => {
                (&old_file.display() as &dyn fmt::Display).fmt(f)?;
                f.write_str(" (deleted)")?;
            }
            (None, Some(new_file)) => {
                (&new_file.display() as &dyn fmt::Display).fmt(f)?;
                f.write_str(" (created)")?;
            }
            (Some(old_file), Some(new_file)) => {
                if old_file != new_file {
                    (&new_file.display() as &dyn fmt::Display).fmt(f)?;
                    f.write_str(" (was ")?;
                    (&old_file.display() as &dyn fmt::Display).fmt(f)?;
                    f.write_str(")")?;
                } else {
                    (&new_file.display() as &dyn fmt::Display).fmt(f)?;
                }
            }
            (None, None) => f.write_str("Unknown file")?,
        }

        Ok(())
    }
}

type DiffHunkHeader = String;

pub(crate) enum DiffContent {
    Patch(HashMap<DiffHunkHeader, Vec<u8>>),
    Binary,
}

pub struct Diff {
    #[allow(unused)]
    pub(crate) from: ObjectId,
    pub(crate) to: ObjectId,
    pub(crate) items: BTreeMap<DiffFileHeader, DiffContent>,
}
