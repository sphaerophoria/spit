use anyhow::{bail, Context, Result};
use std::{fmt, result::Result as StdResult, str::FromStr};

#[derive(Clone, Debug, Hash, Eq, PartialEq, PartialOrd, Ord)]
pub struct ObjectId {
    id: [u8; 20],
}

impl From<[u8; 20]> for ObjectId {
    fn from(id: [u8; 20]) -> Self {
        ObjectId { id }
    }
}

impl From<&git2::Oid> for ObjectId {
    fn from(id: &git2::Oid) -> Self {
        let id: [u8; 20] = id.as_bytes().try_into().expect("Invalid OID");
        ObjectId { id }
    }
}

impl From<git2::Oid> for ObjectId {
    fn from(id: git2::Oid) -> Self {
        From::from(&id)
    }
}

impl From<ObjectId> for git2::Oid {
    fn from(id: ObjectId) -> Self {
        From::from(&id)
    }
}

impl From<&ObjectId> for git2::Oid {
    fn from(id: &ObjectId) -> Self {
        git2::Oid::from_bytes(&id.id).expect("Invalid id")
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
