use chrono::{DateTime, Utc};
mod decompress;
pub(crate) mod graph;
mod object_id;
mod pack;
mod repo;

pub(crate) use graph::{build_git_history_graph, HistoryGraph};
pub(crate) use object_id::ObjectId;
pub(crate) use repo::Repo;

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
