use crate::git::{Identifier, ObjectId, ReferenceId, RemoteRef};
use anyhow::{bail, Error, Result};

fn escaped_string(s: &str) -> String {
    shell_escape::escape(s.into()).into()
}

fn split_remote_reference(s: &str) -> Result<(&str, &str)> {
    s.split_once('/')
        .ok_or_else(|| Error::msg("remote reference has no slash"))
}

pub(crate) fn checkout(id: &Identifier) -> String {
    format!("git checkout {}", escaped_string(&id.to_string()))
}

pub(crate) fn delete(ref_id: &ReferenceId) -> Result<String> {
    let ret = match ref_id {
        ReferenceId::Symbolic(name) => {
            format!("git symbolic-ref --delete {}", escaped_string(name))
        }
        ReferenceId::LocalBranch(name) => format!("git branch -D {}", escaped_string(name)),
        ReferenceId::RemoteBranch(name) => {
            let (remote, name) = split_remote_reference(name)?;
            bail!(
                "Refusing to run remote modifying operation. Run git push {} :{}",
                escaped_string(remote),
                escaped_string(name)
            )
        }
        ReferenceId::Tag(name) => {
            format!("git tag -d {}", escaped_string(name))
        }
        ReferenceId::Unknown => bail!("Cannot remove unknown ref"),
    };
    Ok(ret)
}

pub(crate) fn cherry_pick(id: &ObjectId) -> String {
    format!("git cherry-pick {}", escaped_string(&id.to_string()))
}

pub(crate) fn difftool(id: &ObjectId) -> String {
    format!(
        "git difftool -d {0}~1..{0} &",
        escaped_string(&id.to_string())
    )
}

pub(crate) fn merge(id: &Identifier) -> String {
    format!("git merge {}", escaped_string(&id.to_string()))
}

pub(crate) fn fetch_remote_ref(remote_ref: &RemoteRef) -> String {
    let ref_escaped = escaped_string(&remote_ref.ref_name);
    let remote_escaped = escaped_string(&remote_ref.remote);
    format!(
        "git fetch {} {}:refs/remotes/{}/{}",
        remote_escaped, ref_escaped, remote_escaped, ref_escaped
    )
}
