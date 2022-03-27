mod decompress;
pub(crate) mod graph;
mod object_id;
mod pack;
mod repo;

pub(crate) use graph::{build_git_history_graph, HistoryGraph};
pub(crate) use object_id::ObjectId;
pub(crate) use repo::Repo;

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
pub enum BranchId {
    Head,
    Local(String),
    Remote(String),
}

impl ToString for BranchId {
    fn to_string(&self) -> String {
        match self {
            BranchId::Head => "HEAD".to_string(),
            BranchId::Remote(name) => name.clone(),
            BranchId::Local(name) => name.clone(),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, PartialOrd, Ord)]
pub struct Branch {
    pub(crate) id: BranchId,
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
