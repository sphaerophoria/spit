use anyhow::{Context, Result};

use std::{fs::File, path::PathBuf};

fn main() -> Result<()> {
    let config_path = std::env::args().nth(1).context("No config given")?;
    let output = std::env::args().nth(2).context("No output given")?;

    let mut f = File::open(config_path)?;
    let config = spit::git::repo_writer::parse_config(&mut f)?;
    spit::git::repo_writer::create_repository(&config, &PathBuf::from(output))?;

    Ok(())
}
