use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};

use std::{fmt, result::Result as StdResult, str::FromStr};

mod decompress;
mod graph;
mod history;
mod pack;
pub mod proto;

pub(crate) use graph::{build_git_history_graph, HistoryGraph};
pub(crate) use history::History;

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct ObjectId {
    id: [u8; 20],
}

impl From<[u8; 20]> for ObjectId {
    fn from(id: [u8; 20]) -> Self {
        ObjectId { id }
    }
}

impl TryFrom<&[u8]> for ObjectId {
    type Error = std::array::TryFromSliceError;

    fn try_from(id: &[u8]) -> StdResult<Self, Self::Error> {
        Ok(ObjectId { id: id.try_into()? })
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = [0; 40];
        faster_hex::hex_encode(&self.id, &mut buf).map_err(|_| fmt::Error)?;

        fmt.write_str(unsafe { std::str::from_utf8_unchecked(&buf) })?;

        Ok(())
    }
}

impl FromStr for ObjectId {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let mut id = [0; 20];
        if s.len() != 40 {
            bail!("Object ID strings should be 40 chars");
        }

        faster_hex::hex_decode(s.as_bytes(), &mut id)
            .context("Failed to decode ObjectId string")?;

        Ok(ObjectId { id })
    }
}

impl std::ops::Deref for ObjectId {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.id
    }
}

impl std::ops::DerefMut for ObjectId {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.id
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CommitMetadata {
    pub(crate) id: ObjectId,
    pub(crate) parents: Vec<ObjectId>,
    pub(crate) timestamp: DateTime<Utc>,
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
