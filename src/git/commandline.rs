use crate::git::ReferenceId;
use anyhow::{bail, Error, Result};

fn escaped_string(s: &str) -> String {
    shell_escape::escape(s.into()).into()
}

fn split_remote_reference(s: &str) -> Result<(&str, &str)> {
    s.split_once('/')
        .ok_or_else(|| Error::msg("remote reference has no slash"))
}

pub(crate) fn checkout(ref_id: &ReferenceId) -> String {
    format!("git checkout {}", escaped_string(&ref_id.to_string()))
}

pub(crate) fn delete(ref_id: &ReferenceId) -> Result<String> {
    let ret = match ref_id {
        ReferenceId::Symbolic(name) => {
            format!("git symbolic-ref --delete {}", escaped_string(name))
        }
        ReferenceId::LocalBranch(name) => format!("git branch -D {}", escaped_string(name)),
        ReferenceId::RemoteBranch(name) => {
            let (remote, name) = split_remote_reference(name)?;
            format!(
                "git push {} :{}",
                escaped_string(remote),
                escaped_string(name)
            )
        }
        ReferenceId::Unknown => bail!("Cannot remove unknown ref"),
    };
    Ok(ret)
}
