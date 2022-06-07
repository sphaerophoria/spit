use anyhow::{bail, Context, Result};
use flate2::{write::ZlibEncoder, Compression};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use tempfile::TempDir;

use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
    process::Command,
};

#[derive(Clone, Deserialize)]
struct TreeItem {
    name: String,
    blob_id: String,
}

#[derive(Deserialize)]
#[serde(tag = "object_type")]
enum Object {
    Blob {
        data: String,
    },
    Tree {
        items: Vec<TreeItem>,
    },
    Commit {
        tree: String,
        parent: Option<String>,
        author: String,
        author_timestamp: u32,
        author_timezone: i16,
        committer: String,
        committer_timestamp: u32,
        committer_timezone: i16,
        message: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "pack_type")]
enum PackItem {
    Base(Object),
}

#[derive(Deserialize)]
pub struct Config {
    branches: HashMap<String, String>,
    unpacked: HashMap<String, Object>,
    packed: Vec<HashMap<String, PackItem>>,
}

impl Config {
    fn find(&self, id: &str) -> Result<&Object> {
        if let Some(obj) = self.unpacked.get(id) {
            return Ok(obj);
        }

        for pack_config in &self.packed {
            match pack_config.get(id) {
                Some(PackItem::Base(obj)) => return Ok(obj),
                _ => continue,
            }
        }

        bail!("Id {} not found", id);
    }
}

struct ObjReprCache<'a> {
    config: &'a Config,
    obj_reprs: HashMap<String, Vec<u8>>,
}

impl<'a> ObjReprCache<'a> {
    fn new(config: &Config) -> ObjReprCache {
        let obj_reprs = HashMap::new();

        ObjReprCache { config, obj_reprs }
    }

    fn obj_repr(&mut self, id: &str) -> Result<&[u8]> {
        if !self.obj_reprs.contains_key(id) {
            let obj = self
                .config
                .find(id)
                .with_context(|| format!("Failed to find id {}", id))?;

            let obj_repr = match obj {
                Object::Blob { data } => blob_to_obj_repr(data.as_bytes()),
                Object::Tree { items } => self
                    .tree_to_obj_repr(items)
                    .with_context(|| format!("Failed to generate tree for {}", id))?,
                Object::Commit {
                    tree,
                    parent,
                    author,
                    author_timezone,
                    author_timestamp,
                    committer,
                    committer_timezone,
                    committer_timestamp,
                    message,
                } => self
                    .commit_to_obj_repr(
                        tree,
                        parent.as_ref().map(|s| s.as_ref()),
                        author,
                        *author_timestamp,
                        *author_timezone,
                        committer,
                        *committer_timestamp,
                        *committer_timezone,
                        message,
                    )
                    .with_context(|| format!("Failed to generate tree for {}", id))?,
            };

            self.obj_reprs.insert(id.to_string(), obj_repr);
        }

        Ok(self.obj_reprs.get(id).unwrap())
    }

    fn tree_to_obj_repr(&mut self, tree: &[TreeItem]) -> Result<Vec<u8>> {
        let mut tree_data = Vec::new();
        for item in tree {
            let blob_repr = self
                .obj_repr(&item.blob_id)
                .with_context(|| format!("Failed to resolve blob {}", item.blob_id))?;

            tree_data.extend(b"100644 ");
            tree_data.extend(item.name.as_bytes());
            tree_data.push(0);
            tree_data.extend(
                hex_to_bytes(&sha1(blob_repr)).ok_or_else(|| anyhow::anyhow!("Bad object id"))?,
            );
        }

        let len = tree_data.len();

        let mut ret = Vec::new();
        ret.extend(b"tree ");
        ret.extend(len.to_string().as_bytes());
        ret.push(0);
        ret.extend(tree_data);
        Ok(ret)
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_to_obj_repr(
        &mut self,
        tree: &str,
        parent: Option<&str>,
        author: &str,
        author_timestamp: u32,
        author_timezone: i16,
        committer: &str,
        committer_timestamp: u32,
        committer_timezone: i16,
        message: &str,
    ) -> Result<Vec<u8>> {
        let author_timezone = format_timezone(author_timezone);
        let committer_timezone = format_timezone(committer_timezone);
        let author_timestamp = author_timestamp.to_string();
        let committer_timestamp = committer_timestamp.to_string();
        let parent_str = if let Some(parent) = parent {
            let parent_repr = self
                .obj_repr(parent)
                .context("Failed to lookup parent for commit")?;
            let parent_sha = sha1(parent_repr);
            format!("parent {}\n", parent_sha)
        } else {
            "".to_string()
        };

        let tree_sha = sha1(self.obj_repr(tree).context("Failed to find tree")?);
        let content = format!(
            "tree {}\n\
                {}author {} {} {}\n\
                committer {} {} {}\n\
                \n\
                {}",
            tree_sha,
            parent_str,
            author,
            author_timestamp,
            author_timezone,
            committer,
            committer_timestamp,
            committer_timezone,
            message
        )
        .into_bytes();

        let mut ret = Vec::new();
        ret.extend(b"commit ");
        ret.extend(content.len().to_string().as_bytes());
        ret.push(0);
        ret.extend(content);

        Ok(ret)
    }
}

pub fn parse_config<R: Read>(config: R) -> Result<Config> {
    let config: Config = serde_json::from_reader(config)?;
    Ok(config)
}

pub fn create_repository(config: &Config, output: &Path) -> Result<()> {
    let mut repr_cache = ObjReprCache::new(config);

    std::fs::create_dir_all(output)
        .with_context(|| format!("Failed to create output dir {}", output.display()))?;

    let command_ret = Command::new("git")
        .arg("init")
        .current_dir(output)
        .output()
        .context("Failed to create git dir")?;

    if !command_ret.status.success() {
        bail!(
            "Failed to create git dir: {}",
            std::str::from_utf8(&command_ret.stderr).unwrap()
        );
    }

    for id in config.unpacked.keys() {
        let obj_repr = repr_cache
            .obj_repr(id)
            .with_context(|| format!("Failed to generate object representation for id: {}", id))?;
        write_obj_to_git_repo(obj_repr, output)
            .with_context(|| format!("Failed to write object to repo: {}", id))?;
    }

    for pack in &config.packed {
        write_pack_to_git_repo(pack, &mut repr_cache, output).context("Failed to write pack")?;
    }

    let branch_dir = output.join(".git/refs/heads");
    std::fs::create_dir_all(&branch_dir).context("Failed to create branch dir")?;
    println!("branch_dir: {}", branch_dir.display());

    for (branch_name, obj_id) in &config.branches {
        let mut branch_file = OpenOptions::new()
            .truncate(true)
            .write(true)
            .create(true)
            .open(branch_dir.join(branch_name))
            .context("Failed to open branch file")?;

        branch_file
            .write_all(sha1(repr_cache.obj_repr(obj_id)?).as_bytes())
            .context("Failed to write branch")?;
    }

    Ok(())
}

fn blob_to_obj_repr(data: &[u8]) -> Vec<u8> {
    let mut ret = Vec::new();

    ret.extend(b"blob ");
    ret.extend(data.len().to_string().as_bytes());
    ret.push(0);
    ret.extend(data);

    ret
}

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    // FIXME:
    if s.len() % 2 == 0 {
        (0..s.len())
            .step_by(2)
            .map(|i| {
                s.get(i..i + 2)
                    .and_then(|sub| u8::from_str_radix(sub, 16).ok())
            })
            .collect()
    } else {
        None
    }
}

fn format_timezone(tz: i16) -> String {
    let mut ret = String::new();
    if tz < 0 {
        ret.push('-')
    };

    use std::fmt::Write;
    write!(ret, "{:04}", tz.abs()).expect("Unable to write timezone");

    ret
}

fn sha1(obj: &[u8]) -> String {
    let mut hasher = Sha1::default();
    hasher.update(&obj);
    let repr_hash = hasher.finalize();
    faster_hex::hex_string(&repr_hash)
}

fn write_obj_to_git_repo(obj: &[u8], repo: &Path) -> Result<()> {
    let repr_hash = sha1(obj);

    let objects = repo.join(".git/objects");
    let parent = objects.join(&repr_hash[..2]);
    std::fs::create_dir_all(&parent).context("Failed to create object dir")?;
    let object = parent.join(&repr_hash[2..]);

    if object.exists() {
        return Ok(());
    }

    let obj_file = OpenOptions::new()
        .write(true)
        .create(true)
        .open(&object)
        .with_context(|| format!("Failed to open object {} for writing", object.display()))?;

    let mut zlib_encoder = ZlibEncoder::new(obj_file, Compression::default());
    zlib_encoder
        .write_all(obj)
        .context("Failed to write object data")?;

    Ok(())
}

fn write_to_pack<W: Write>(hasher: &mut sha1::Sha1, mut output: W, data: &[u8]) -> Result<()> {
    hasher.update(data);
    output.write_all(data)?;
    Ok(())
}

fn be_u32_arr(i: u32) -> [u8; 4] {
    [
        ((i >> 24) & 0xff) as u8,
        ((i >> 16) & 0xff) as u8,
        ((i >> 8) & 0xff) as u8,
        (i & 0xff) as u8,
    ]
}

fn encode_obj_header(typ: u8, mut size: u32) -> Vec<u8> {
    let mut ret = vec![];

    let first_c = ((size > 0xf) as u8) << 7;
    let first_size = (size & 0xf) as u8;
    let first_b = (((typ & 0x7) << 4) as u8) | first_size | first_c;
    ret.push(first_b);
    size >>= 4;

    if first_c == 0 {
        return ret;
    }

    loop {
        let c = size & !0x7f;
        let b = (size & 0x7f) as u8;

        ret.push(((c << 7) as u8) | b);

        if c == 0 {
            break;
        }

        size >>= 7;
    }
    println!("{:?}", ret);

    ret
}

fn write_pack_to_git_repo<'a>(
    pack: &HashMap<String, PackItem>,
    repr_cache: &mut ObjReprCache<'a>,
    repo: &Path,
) -> Result<()> {
    let tmp_dir = TempDir::new_in(&repo).context("Failed to generate tmp dir")?;
    let tmp_pack_path = tmp_dir.path().join("test.pack");
    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&tmp_pack_path)
        .context("Failed to open tmp pack")?;

    let mut hasher = sha1::Sha1::new();

    write_to_pack(&mut hasher, &mut output, b"PACK")?;
    write_to_pack(&mut hasher, &mut output, &be_u32_arr(2))?;
    write_to_pack(&mut hasher, &mut output, &be_u32_arr(pack.len() as u32))?;

    for (id, item) in pack {
        println!("{}", id);
        let mut compressor = flate2::write::ZlibEncoder::new(Vec::new(), Compression::default());

        let obj_repr = repr_cache.obj_repr(id)?;

        let pack_repr = obj_repr.splitn(2, |x| *x == 0).nth(1).unwrap();
        compressor.write_all(pack_repr)?;
        let compressed = compressor.finish()?;

        // FIXME: Delta/Offs
        let header_id = match item {
            PackItem::Base(Object::Commit { .. }) => 1,
            PackItem::Base(Object::Tree { .. }) => 2,
            PackItem::Base(Object::Blob { .. }) => 3,
        };

        write_to_pack(
            &mut hasher,
            &mut output,
            &encode_obj_header(header_id, pack_repr.len() as u32),
        )?;
        write_to_pack(&mut hasher, &mut output, &compressed)?;
    }

    let pack_hash = hasher.finalize();
    output
        .write_all(&pack_hash)
        .context("Failed to write pack sha1")?;

    output.flush().context("Failed to flush pack file")?;

    let index_pack_output = Command::new("git")
        .current_dir(repo)
        .arg("index-pack")
        .arg(&tmp_pack_path)
        .output()
        .context("Failed to run index-pack")?;

    if !index_pack_output.status.success() {
        bail!(
            "Failed to index pack: {}",
            std::str::from_utf8(&index_pack_output.stderr).unwrap()
        );
    }

    let pack_idx_id = std::str::from_utf8(&index_pack_output.stdout)
        .unwrap()
        .trim();

    let pack_dir = repo.join(".git/objects/pack");
    std::fs::create_dir_all(&pack_dir).context("Failed to create pack dir")?;

    let existing_index_path = tmp_pack_path.with_extension("idx");
    std::fs::rename(
        existing_index_path,
        pack_dir.join(format!("pack-{}.idx", pack_idx_id)),
    )
    .context("Failed to rename index")?;

    std::fs::rename(
        tmp_pack_path,
        pack_dir.join(format!("pack-{}.pack", pack_idx_id)),
    )
    .context("Failed to persist pack file")?;

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fs::File;
    use std::io::Read;
    use std::process::Command;

    #[test]
    fn write_commit() {
        let repo = tempfile::TempDir::new().unwrap();

        let config_data = include_bytes!("../../res/test/dual_commit_unpacked.json");
        let config = parse_config(config_data as &[u8]).unwrap();
        create_repository(&config, repo.path()).unwrap();

        let git_data = Command::new("git")
            .current_dir(repo.path())
            .arg("checkout")
            .arg("master")
            .output()
            .unwrap();

        std::io::stdout().write_all(&git_data.stderr).unwrap();

        assert!(git_data.status.success());

        assert!(repo.path().join("tmp.txt").exists());

        let mut written = File::open(repo.path().join("tmp.txt")).unwrap();
        let mut written_content = String::new();
        written.read_to_string(&mut written_content).unwrap();
        assert_eq!(written_content, "My test blob");

        let num_commits = Command::new("bash")
            .args(["-c", "git log --oneline | wc -l"])
            .current_dir(repo.path())
            .output()
            .unwrap();

        assert_eq!(
            std::str::from_utf8(&num_commits.stdout).unwrap().trim(),
            "2"
        );
    }
}
