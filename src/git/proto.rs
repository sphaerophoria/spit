use crate::{
    Timer,
    git::{history::History, ObjectId}
};

use anyhow::Result;

use std::path::Path;

pub fn prototype_test(path: &Path) -> Result<()> {
    let mut hist = History::new(path)?;
    let repo = git2::Repository::open(path)?;

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
    for branch in repo.branches(None)? {
        let (branch, _) = branch?;
        let r = branch.into_reference();
        let r = r.resolve().unwrap();
        let oid = r.target().unwrap();
        parents.push(oid.as_bytes().try_into()?);
    }

    for metadata in hist.metadata_iter(&parents)?.take(100) {
        println!("{}: {}", metadata.id, metadata.timestamp);
    }
    println!("First iter took: {}", timer.elapsed().as_secs_f32());
    timer.reset();

    let _ = hist.metadata_iter(&parents)?;
    println!("Second iter took: {}", timer.elapsed().as_secs_f32());

    Ok(())
}
