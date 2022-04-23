use crate::git::{decompress, CommitMetadata, CommitMetadataWithoutId, ObjectId};

use anyhow::{bail, Context, Result};
use flate2::Decompress;
use memmap2::Mmap;

use std::{fs::File, path::Path};

mod index_impl {
    use super::PackIndex;

    use crate::git::ObjectId;

    use anyhow::{bail, Result};
    use memmap2::Mmap;

    use std::{fs::File, path::Path};

    const OBJECT_SIZE: usize = 20;
    const FANOUT_ENTRY_SIZE: usize = 4;
    const CRC_SIZE: usize = 4;
    const OFFSET_ENTRY_SIZE: usize = 4;

    pub(super) struct PackIndexV2 {
        index_data: Mmap,
    }

    impl PackIndexV2 {
        const FANOUT_START: usize = 8;
        const OBJECT_START: usize = Self::FANOUT_START + 256 * FANOUT_ENTRY_SIZE;
    }

    impl PackIndex for PackIndexV2 {
        fn object_offset(&self, obj: &ObjectId) -> Result<Option<usize>> {
            let num_elems = read_fanout(&self.index_data, Self::FANOUT_START, 255) as usize;
            let object_index = match binary_search_object_index(
                &self.index_data,
                Self::FANOUT_START,
                Self::OBJECT_START,
                obj,
            ) {
                Some(index) => index,
                None => return Ok(None),
            };
            let offset_table_offset =
                Self::OBJECT_START + num_elems * OBJECT_SIZE + num_elems * CRC_SIZE;
            Ok(Some(offset_from_index(
                &self.index_data,
                offset_table_offset,
                object_index,
            )?))
        }
    }

    pub(super) fn construct_index(path: &Path) -> Result<Box<dyn PackIndex + Send + Sync>> {
        let f = File::open(path).unwrap();
        let index_data = unsafe { Mmap::map(&f).unwrap() };

        if index_data[0..4] != [0xff, 0x74, 0x4f, 0x63] {
            bail!("Unknown header for pack index, may be unimplemented V1 index");
        }

        let version = u32::from_be_bytes(index_data[4..8].try_into().unwrap());
        if version == 2 {
            return Ok(Box::new(PackIndexV2 { index_data }));
        }

        bail!("Unsupported index version");
    }

    pub(super) fn read_fanout(data: &[u8], fanout_start: usize, idx: u8) -> u32 {
        let data_start = fanout_start + (idx as usize) * FANOUT_ENTRY_SIZE;
        let data_end = data_start + FANOUT_ENTRY_SIZE;
        u32::from_be_bytes(
            data[data_start..data_end]
                .try_into()
                .expect("Slice not 4 bytes"),
        )
    }

    pub(super) fn binary_search_object_index(
        data: &[u8],
        fanout_start: usize,
        object_start: usize,
        desired_obj: &[u8],
    ) -> Option<usize> {
        assert_eq!(desired_obj.len(), 20);
        let mut lower_bound = if desired_obj[0] == 0 {
            0usize
        } else {
            read_fanout(data, fanout_start, desired_obj[0] - 1) as usize
        };

        let mut upper_bound = read_fanout(data, fanout_start, desired_obj[0]) as usize;
        assert!(upper_bound >= lower_bound);

        let mut index = (lower_bound + upper_bound) / 2;
        loop {
            let item_start = object_start + OBJECT_SIZE * index;

            let current_obj = &data[item_start..item_start + OBJECT_SIZE];
            match current_obj.cmp(desired_obj) {
                std::cmp::Ordering::Less => {
                    lower_bound = index;
                }
                std::cmp::Ordering::Greater => {
                    upper_bound = index;
                }
                _ => {
                    break;
                }
            }

            if lower_bound >= upper_bound {
                return None;
            }

            if (lower_bound + 1 == upper_bound) && index == lower_bound {
                lower_bound += 1;
            }

            index = (lower_bound + upper_bound) / 2;
        }

        Some(index)
    }

    pub(super) fn offset_from_index(
        data: &[u8],
        offset_table_offset: usize,
        index: usize,
    ) -> Result<usize> {
        let offset_start = offset_table_offset + index * OFFSET_ENTRY_SIZE;
        let offset_end = offset_start + OFFSET_ENTRY_SIZE;
        let offset = u32::from_be_bytes(data[offset_start..offset_end].try_into().unwrap());
        // 32 bit int, highest bit indicates a large file lookup
        if offset >= 0x80000000 {
            bail!("Large table lookup unimplemented");
        }
        Ok(offset as usize)
    }
}

mod pack_impl {
    use anyhow::{bail, Result};

    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum ObjectType {
        Commit,
        Tree,
        Blob,
        Tag,
        OffsetDelta,
        RefDelta,
    }

    impl TryFrom<u8> for ObjectType {
        type Error = anyhow::Error;

        fn try_from(typ: u8) -> Result<ObjectType> {
            match typ {
                1 => Ok(ObjectType::Commit),
                2 => Ok(ObjectType::Tree),
                3 => Ok(ObjectType::Blob),
                4 => Ok(ObjectType::Tag),
                6 => Ok(ObjectType::OffsetDelta),
                7 => Ok(ObjectType::RefDelta),
                _ => bail!("Unknown object type"),
            }
        }
    }

    impl std::fmt::Display for ObjectType {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                ObjectType::Commit => f.write_str("commit"),
                ObjectType::Tree => f.write_str("tree"),
                ObjectType::Blob => f.write_str("blob"),
                ObjectType::Tag => f.write_str("tag"),
                ObjectType::OffsetDelta => f.write_str("offset delta"),
                ObjectType::RefDelta => f.write_str("ref delta"),
            }
        }
    }

    #[derive(Debug)]
    pub(super) struct ObjHeader {
        pub(super) typ: ObjectType,
        pub(super) size: usize,
    }

    pub(super) fn read_pack_obj_header(data: &[u8]) -> Result<(ObjHeader, usize)> {
        // Header is the first
        let b0 = data[0];
        let mut continue_reading = b0 & 0x80 != 0;
        // Type is the first 3 bits after the continuation bit
        let typ = ((b0 >> 4) & 0x7).try_into()?;
        let mut size = (b0 & 0xf) as usize;
        // 4 bits initially read
        let mut shift = 4;
        let mut i = 1;

        while continue_reading {
            let b = data[i];
            continue_reading = b & 0x80 != 0;
            size |= ((b & 0x7f) as usize) << shift;
            shift += 7;
            i += 1;
        }

        let header = ObjHeader { typ, size };

        Ok((header, i))
    }

    pub(super) fn parse_offset_delta_base_obj_offset(data: &[u8]) -> (usize, usize) {
        // Stolen from packfile.c
        let mut i = 0;
        let mut b = data[i];
        let mut val = (b & 127) as usize;
        while (b & 128) != 0 {
            val += 1;
            i += 1;
            b = data[i];
            val = (val << 7) + (b & 127) as usize;
        }

        (val, i + 1)
    }

    pub(super) fn parse_size_encoded(data: &[u8]) -> (usize, usize) {
        let mut i = 0;
        let mut b = data[i];
        let mut val = (b & 0x7f) as usize;
        while (b & 0x80) != 0 {
            i += 1;
            b = data[i];
            val |= ((b & 0x7f) as usize) << (7 * i);
        }

        (val, i + 1)
    }

    pub(super) fn pack_apply_delta(source: &[u8], patch: &[u8]) -> Vec<u8> {
        let mut patch_dest: Vec<u8> = Vec::new();
        let mut patch_pos = 0usize;

        let (_base_size, read_bytes) = parse_size_encoded(patch);
        let patch = &patch[read_bytes..];
        let (_output_size, read_bytes) = parse_size_encoded(patch);
        let patch = &patch[read_bytes..];

        while patch_pos < patch.len() {
            let cmd = patch[patch_pos];
            patch_pos += 1;

            if cmd & 0x80 != 0 {
                let mut offset = 0;
                for i in 0..4 {
                    if cmd & (1 << i) != 0 {
                        offset |= (patch[patch_pos] as usize) << (i * 8);
                        patch_pos += 1;
                    }
                }

                let mut size = 0;
                for i in 0..3 {
                    if cmd & (1 << (i + 4)) != 0 {
                        size |= (patch[patch_pos] as usize) << (i * 8);
                        patch_pos += 1;
                    }
                }

                if size == 0 {
                    size = 0x10000;
                }

                let end = usize::min(offset + size, source.len());
                patch_dest.extend(source[offset..end].iter());
            } else {
                let data_size: usize = (cmd & 0x7f) as usize;
                patch_dest.extend(patch[patch_pos..patch_pos + data_size].iter());
                patch_pos += data_size;
            }
        }

        patch_dest
    }
}

trait PackIndex {
    fn object_offset(&self, obj: &ObjectId) -> Result<Option<usize>>;
}

use std::cell::RefCell;

struct PackData {
    data: Mmap,
    decompressor: RefCell<Decompress>,
}

impl PackData {
    fn new(path: &Path) -> Result<PackData> {
        let file = File::open(path).context("Failed to open pack file")?;
        let data = unsafe { Mmap::map(&file).context("Failed to mmap file") }?;
        let decompressor = RefCell::new(Decompress::new(true));

        Ok(PackData { data, decompressor })
    }

    fn get_commit_metadata(&self, pack_obj_location: usize) -> Result<CommitMetadataWithoutId> {
        use pack_impl::ObjectType;

        let mut decompressor = self.decompressor.borrow_mut();

        let (header, pack_obj_data_offset) =
            pack_impl::read_pack_obj_header(&self.data[pack_obj_location..])?;
        match header.typ {
            ObjectType::Commit => {
                let pack_obj_data_start = pack_obj_location + pack_obj_data_offset;
                // As far as I can tell, the size found in the header is not guaranteed to be
                // correct unless we are using it for delta patching. Reading through packfile.c in
                // git's repo it does not look like the size value is used for regular object
                // types.
                //
                // On commit 7b7abfe3dd81d659a0889f88965168f7eef8c5c6 in the linux kernel I see a
                // header size of 214, but that ends up truncating the commit and I cannot extract
                // the committer date.
                //
                // Just provide the full file and let the decompressor go wild I guess
                //
                // I may just be patching over a bug, but even if I copy paste the logic from
                // packfile.c into read_pack_obj_header I end up with the same results
                let pack_obj_data = &self.data[pack_obj_data_start..];
                decompress::decompress_commit_metadata(pack_obj_data, &mut decompressor, true)
            }
            ObjectType::OffsetDelta => {
                let base_ref_offset_start = pack_obj_location + pack_obj_data_offset;
                let (base_ref_offset, read_bytes) = pack_impl::parse_offset_delta_base_obj_offset(
                    &self.data[base_ref_offset_start..],
                );
                let base_ref_location = pack_obj_location - base_ref_offset;

                let mut curr_data_loc = base_ref_location;
                let mut patch_stack = vec![(base_ref_offset_start + read_bytes, header.size)];
                // May have to fix this later
                let mut patch_buf = Vec::new();
                loop {
                    let (base_header, header_read_bytes) =
                        pack_impl::read_pack_obj_header(&self.data[curr_data_loc..])?;
                    if base_header.typ != ObjectType::OffsetDelta {
                        assert!(base_header.typ == ObjectType::Commit);
                        // FIXME: We can probably merge code with parse_pack_commit somehow
                        let base_data_loc = curr_data_loc + header_read_bytes;
                        decompressor.reset(true);
                        // Annoyingly, there's no guarantee that the patch for a header is
                        // going to come from a header. This means that we _have_ to decompress
                        // the whole commit to be able to parse the whole header of the delta
                        // >:(
                        patch_buf.reserve(base_header.size);
                        decompressor
                            .decompress_vec(
                                &self.data[base_data_loc..],
                                &mut patch_buf,
                                flate2::FlushDecompress::None,
                            )
                            .context("Failed to decompress base of pack patch")?;
                        break;
                    } else {
                        let base_ref_offset_start = curr_data_loc + header_read_bytes;
                        let (base_ref_offset, read_bytes) =
                            pack_impl::parse_offset_delta_base_obj_offset(
                                &self.data[base_ref_offset_start..],
                            );
                        let base_ref_location = curr_data_loc - base_ref_offset;

                        curr_data_loc = base_ref_location;
                        patch_stack.push((base_ref_offset_start + read_bytes, base_header.size));
                    }
                }

                while let Some((patch_loc, patch_size)) = patch_stack.pop() {
                    decompressor.reset(true);
                    let mut patch_data = Vec::new();
                    patch_data.reserve(patch_size);
                    decompressor
                        .decompress_vec(
                            &self.data[patch_loc..],
                            &mut patch_data,
                            flate2::FlushDecompress::None,
                        )
                        .unwrap();
                    // FIXME: We could only decompress the parts of the patch that are relevant
                    // FIXME: We could cache patches
                    patch_buf = pack_impl::pack_apply_delta(&patch_buf, &patch_data);
                }

                assert!(patch_buf.starts_with(b"tree"));

                let mut parents: Vec<ObjectId> = Vec::new();
                let mut timestamp = None;
                let mut committer_timestamp = None;

                for line in patch_buf.split(|&x| x == b'\n') {
                    if line.is_empty() {
                        break;
                    }
                    if line.starts_with(b"parent") && line.len() >= 47 {
                        parents.push([0; 20].into());
                        // FIXME: Shouldn't just blindly  look for the strign parent
                        faster_hex::hex_decode(&line[7..47], parents.last_mut().unwrap()).unwrap()
                    } else if line.starts_with(b"author") {
                        timestamp = Some(decompress::extract_timestamp_from_buf(line)?);
                    } else if line.starts_with(b"committer") {
                        committer_timestamp = Some(decompress::extract_timestamp_from_buf(line)?);
                    }
                }

                let timestamp = timestamp.unwrap();
                let committer_timestamp = committer_timestamp.unwrap();
                Ok(CommitMetadataWithoutId {
                    parents,
                    author_timestamp: timestamp,
                    committer_timestamp,
                })
            }
            _ => bail!(format!("Unimplemented parser for {}", header.typ)),
        }
    }
}

pub(crate) struct Pack {
    index: Box<dyn PackIndex + Send + Sync>,
    pack: PackData,
}

impl Pack {
    pub(crate) fn new(pack_path: &Path) -> Result<Pack> {
        let index_path = pack_path.with_extension("idx");
        let index =
            index_impl::construct_index(&index_path).context("Failed to construct index")?;

        let pack = PackData::new(pack_path).context("Failed to construct pack")?;

        Ok(Pack { index, pack })
    }

    pub(crate) fn get_commit_metadata(&self, obj: ObjectId) -> Result<Option<CommitMetadata>> {
        let offset = self
            .index
            .object_offset(&obj)
            .with_context(|| format!("Failed to lookup object {}", obj))?;

        let offset = match offset {
            Some(v) => v,
            None => return Ok(None),
        };

        let ret = self
            .pack
            .get_commit_metadata(offset)
            .with_context(|| format!("Failed to read metadata for found commit: {}", obj))?;

        Ok(Some(ret.into_full_metadata(obj)))
    }
}
