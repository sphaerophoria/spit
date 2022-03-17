use crate::git::{CommitMetadataWithoutId, ObjectId};

use anyhow::{bail, Context, Result};
use flate2::Decompress;

pub(super) fn decompress_commit_metadata(
    commit: &[u8],
    decompressor: &mut Decompress,
    from_pack: bool,
) -> Result<CommitMetadataWithoutId> {
    // FIXME: Long function that should be split up
 
    // Long hashes are 20 bytes * 2 for string encoded
    const OBJ_HASH_LEN: usize = 40;
    // tree hash\n
    const TREE_LINE_LEN: usize = 6 + OBJ_HASH_LEN;
    // parent hash\n
    const PARENT_LINE_LEN: usize = 8 + OBJ_HASH_LEN;

    decompressor.reset(true);
    if from_pack {
        let mut tree_buf = [0; TREE_LINE_LEN];
        decompressor
            .decompress(commit, &mut tree_buf, flate2::FlushDecompress::None)
            .context("Failed to decompress tree line")?;
    } else {
        let mut tree_buf = [0; TREE_LINE_LEN];
        decompressor
            .decompress(commit, &mut tree_buf, flate2::FlushDecompress::None)
            .context("Failed to decompress start of line")?;
        let null_byte_pos = match tree_buf.iter().position(|x| *x == 0) {
            Some(v) => v,
            None => bail!("Failed to find the start of the commit data"),
        };

        let total_in = decompressor.total_in() as usize;
        decompressor
            .decompress(
                &commit[total_in..],
                &mut tree_buf[..null_byte_pos + 1],
                flate2::FlushDecompress::None,
            )
            .context("Failed to decompress end of tree line")?;
    }

    let mut parent_buf = [0; PARENT_LINE_LEN];
    let mut parents: Vec<ObjectId> = Vec::new();
    loop {
        let total_in = decompressor.total_in() as usize;
        // Read before breaking since the author parsing assumes that we have parsed the next line
        decompressor
            .decompress(
                &commit[total_in..],
                &mut parent_buf,
                flate2::FlushDecompress::None,
            )
            .context("Failed to decompress pack obj data")?;

        if !parent_buf.starts_with(b"parent ") {
            break;
        }

        parents.push([0; 20].into());

        // 7 bytes for parent
        // 2*20 character kex string
        faster_hex::hex_decode(&parent_buf[7..47], parents.last_mut().unwrap()).unwrap();
    }

    // To get the date is a little trickier
    // Author line should come next
    // Author line is of the form
    // author name <email> unix timestamp timezone
    // The above does not have a determined length, but what we can do is use a stack based
    // circular buffer and parse until we find the date.
    // If we assume that git commits happen in a reasonable future (e.g. before the year 3000), we
    // can assume that the number of digits in the timestamp is going to be less than 12
    // From git date.c it seems that the timestamp is always 5 digits
    // 	strbuf_addf(buf, "%"PRItime" %c%02d%02d", date, sign, offset / 60, offset % 60);
    //
    // With all that combined, we know we need a 17byte window to parse the date, we also know that
    // we can find the start of the date by navigating back 2 spaces from a newline. We also
    // already have a buffer that has been initialized with the first segment of data. For now lets
    // just ping pong back and forth between the first and second half of that buffer until we find
    // a newline. We will always have the last 48 bytes in this case which is more than sufficient
    // information to extract the date
    //
    // Note that we could also extract the author nearly for free here as well with 0 allocations
    // by just finding the ranges of the mapped data, but that seems difficult and unnecessary for
    // the time being
    let mut author_buf = parent_buf;
    const AUTHOR_RANGES: [std::ops::Range<usize>; 2] =
        [0..PARENT_LINE_LEN / 2, PARENT_LINE_LEN / 2..PARENT_LINE_LEN];

    assert!(author_buf.starts_with(b"author"));

    let mut author_buf_idx = 0usize;
    if !author_buf[AUTHOR_RANGES[author_buf_idx].clone()].contains(&b'\n') {
        author_buf_idx = 1;
        loop {
            if author_buf[AUTHOR_RANGES[author_buf_idx].clone()].contains(&b'\n') {
                break;
            }

            author_buf_idx = (author_buf_idx + 1) % 2;
            let total_in = decompressor.total_in() as usize;
            decompressor
                .decompress(
                    &commit[total_in..],
                    &mut author_buf[AUTHOR_RANGES[author_buf_idx].clone()],
                    flate2::FlushDecompress::None,
                )
                .context("Failed to decompress partial author line")?;
        }
    }

    // Put the buffer in the right order to make the rest of the parsing easier
    if author_buf_idx == 0 {
        assert!(PARENT_LINE_LEN % 2 == 0);
        for i in 0..(PARENT_LINE_LEN / 2) {
            author_buf.swap(i, i + PARENT_LINE_LEN / 2);
        }
    }

    let newline_pos = author_buf
        .iter()
        .position(|x| *x == b'\n')
        .context("Did not find newline in object data")?;
    let timestamp_buf = &author_buf[..newline_pos];
    let mut found_spaces = 0;
    let timestamp_start = timestamp_buf
        .iter()
        .rposition(|x| {
            if *x == b' ' {
                found_spaces += 1;
            }

            found_spaces == 2
        })
        .context("Could not find start of timestamp")?
        + 1;

    let timestamp_buf = &timestamp_buf[timestamp_start..];
    let timestamp_str = std::str::from_utf8(timestamp_buf).context("Invalid timestamp buf")?;
    let timestamp =
        chrono::DateTime::parse_from_str(timestamp_str, "%s %z")?.with_timezone(&chrono::Utc);

    Ok(CommitMetadataWithoutId { parents, timestamp })
}
