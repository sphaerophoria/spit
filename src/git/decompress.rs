use crate::git::{CommitMetadataWithoutId, ObjectId};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
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
    let author_buf = &mut parent_buf;
    assert!(author_buf.starts_with(b"author"));
    continue_extraction_until_newline(author_buf, 0, commit, decompressor)
        .context("Failed to author newline")?;

    let newline_pos = author_buf
        .iter()
        .position(|x| *x == b'\n')
        .context("Did not find newline in object data")?;
    let timestamp_buf = &author_buf[..newline_pos];

    let timestamp =
        extract_timestamp_from_buf(timestamp_buf).context("Failed to get author timestamp")?;

    let committer_buf = author_buf;

    let line_start = newline_pos + 1;
    let start_len = committer_buf.len() - line_start;
    assert!(committer_buf[line_start..].starts_with(&b"committer"[..usize::min(start_len, 9)]));
    continue_extraction_until_newline(committer_buf, line_start, commit, decompressor)
        .context("Failed to find committer newline")?;

    let newline_pos = committer_buf
        .iter()
        .position(|x| *x == b'\n')
        .unwrap_or(committer_buf.len());

    let timestamp_buf = &committer_buf[..newline_pos];
    let committer_timestamp =
        extract_timestamp_from_buf(timestamp_buf).context("Failed to get committer timestamp")?;
    Ok(CommitMetadataWithoutId {
        parents,
        author_timestamp: timestamp,
        committer_timestamp,
    })
}

fn continue_extraction_until_newline(
    buf: &mut [u8],
    buf_start_pos: usize,
    commit: &[u8],
    decompressor: &mut Decompress,
) -> Result<()> {
    if buf[buf_start_pos..].contains(&b'\n') {
        return Ok(());
    }

    let half_buf_len = buf.len() / 2;
    let buf_ranges: [std::ops::Range<usize>; 2] = [0..half_buf_len, half_buf_len..buf.len()];

    let mut buf_idx = 0;
    loop {
        let total_in = decompressor.total_in() as usize;
        decompressor
            .decompress(
                &commit[total_in..],
                &mut buf[buf_ranges[buf_idx].clone()],
                flate2::FlushDecompress::None,
            )
            .context("Failed to decompress partial author line")?;

        if buf[buf_ranges[buf_idx].clone()].contains(&b'\n')
            || decompressor.total_in() as usize == commit.len()
        {
            break;
        }

        buf_idx = (buf_idx + 1) % 2;
    }

    // Swap buffers if they're in the wrong order to simplify upstream processing
    if buf_idx == 0 {
        assert!(buf.len() % 2 == 0);
        for i in 0..(buf.len() / 2) {
            buf.swap(i, i + buf.len() / 2);
        }
    }

    Ok(())
}

pub(crate) fn extract_timestamp_from_buf(timestamp_buf: &[u8]) -> Result<DateTime<Utc>> {
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
    Ok(chrono::DateTime::parse_from_str(timestamp_str, "%s %z")?.with_timezone(&chrono::Utc))
}
