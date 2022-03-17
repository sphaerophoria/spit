use anyhow::Result;

use std::path::Path;

fn main() -> Result<()> {
    env_logger::init();
    personal_git_gui::git::proto::prototype_test(Path::new(&std::env::args().nth(1).unwrap()))?;

    Ok(())
}
