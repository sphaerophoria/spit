use crate::{
    app::CheckoutItem,
    git::{ObjectId, ReferenceId},
};
use anyhow::{bail, Error, Result};

fn escaped_string(s: &str) -> String {
    shell_escape::escape(s.into()).into()
}

fn split_remote_reference(s: &str) -> Result<(&str, &str)> {
    s.split_once('/')
        .ok_or_else(|| Error::msg("remote reference has no slash"))
}

pub(crate) fn checkout(item_id: &CheckoutItem) -> String {
    let ref_s = match item_id {
        CheckoutItem::Object(id) => id.to_string(),
        CheckoutItem::Reference(id) => id.to_string(),
    };
    format!("git checkout {}", escaped_string(&ref_s))
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

pub(crate) fn cherry_pick(id: &ObjectId) -> String {
    format!("git cherry-pick {}", escaped_string(&id.to_string()))
}
