use crate::{
    git::{ObjectId, Repo},
    Timer,
};

use anyhow::Result;

use std::path::Path;

pub fn prototype_test(path: &Path) -> Result<()> {
    let mut repo = Repo::new(path)?;

    //let mut revwalk = repo.revwalk()?;
    //let mut sorting = git2::Sort::TOPOLOGICAL;
    //sorting.insert(git2::Sort::TIME);
    //revwalk.set_sorting(sorting).expect("Invalid sort method");
    //for b in repo.branches(None)? {
    //    let (branch, _branchtype) = b?;
    //    if let Some(t) = branch.get().target() {
    //        revwalk.push(t)?;
    //    }
    //}

    //revwalk.next();

    let mut timer = Timer::new();
    let mut parents: Vec<ObjectId> = Vec::new();
    for branch in repo.branches()? {
        parents.push(branch?.head);
    }

    for metadata in repo.metadata_iter(&parents)?.take(100) {
        println!("{}: {}", metadata.id, metadata.timestamp);
    }
    println!("First iter took: {}", timer.elapsed().as_secs_f32());
    timer.reset();

    let _ = repo.metadata_iter(&parents)?;
    println!("Second iter took: {}", timer.elapsed().as_secs_f32());

    Ok(())
}
