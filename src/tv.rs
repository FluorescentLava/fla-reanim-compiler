//! Byte-preserving conversion from the Android TV reanim layout to PC DefMap.
//!
//! TV adds three strings to each frame and uses larger pointer-bearing structs.
//! Pointer values are discarded, while the eight float words and retained
//! length-prefixed strings are copied verbatim, including non-UTF-8 bytes.
use crate::codec::{COOKIE, SCHEMA};
use anyhow::{Context, Result, ensure};
use flate2::{Compression, Decompress, FlushDecompress, Status, write::ZlibEncoder};
use std::{borrow::Cow, io::Write};

const MAX_BYTES: usize = 512 * 1024 * 1024;
const MAX_TRACKS: usize = 100_000;
const MAX_FRAMES: usize = 1_000_000;
const MAX_STRING: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub tracks: usize,
    pub frames: usize,
    pub fps: f32,
}

struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(size)
            .context("TV offset overflow")?;
        ensure!(
            end <= self.data.len(),
            "Truncated TV data at byte {}",
            self.position
        );
        let result = &self.data[self.position..end];
        self.position = end;
        Ok(result)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }

    fn string(&mut self) -> Result<&'a [u8]> {
        let start = self.position;
        let length = self.u32()? as usize;
        ensure!(
            length <= MAX_STRING,
            "TV string exceeds the PC engine limit"
        );
        self.take(length)?;
        Ok(&self.data[start..self.position])
    }
}

fn payload(bytes: &[u8]) -> Result<Cow<'_, [u8]>> {
    ensure!(
        bytes.len() <= MAX_BYTES,
        "TV input exceeds the 512 MiB limit"
    );
    if bytes.get(..4) != Some(COOKIE.to_le_bytes().as_slice()) {
        return Ok(Cow::Borrowed(bytes));
    }
    ensure!(bytes.len() >= 8, "Truncated TV container header");
    let size = u32::from_le_bytes(bytes[4..8].try_into()?) as usize;
    ensure!(
        (24..=MAX_BYTES).contains(&size),
        "Invalid TV declared payload size"
    );
    let mut raw = vec![0; size + 1];
    let mut decoder = Decompress::new(true);
    let status = decoder
        .decompress(&bytes[8..], &mut raw, FlushDecompress::Finish)
        .context("Invalid TV zlib stream")?;
    ensure!(
        status == Status::StreamEnd,
        "Truncated or oversized TV zlib stream"
    );
    ensure!(
        decoder.total_out() as usize == size,
        "TV declared size differs from decoded size"
    );
    ensure!(
        decoder.total_in() as usize == bytes.len() - 8,
        "Trailing bytes after TV zlib stream"
    );
    raw.truncate(size);
    Ok(Cow::Owned(raw))
}

fn word(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

/// Accept a complete wrapped or raw TV resource and emit canonical PC compiled.
/// Already-PC resources, malformed lengths and trailing data are rejected.
pub fn convert(bytes: &[u8]) -> Result<Vec<u8>> {
    convert_with_summary(bytes).map(|(output, _)| output)
}

/// Convert and collect UI statistics in the same parse, without decoding raw
/// resource-name bytes as UTF-8 or expanding MISSING float/string placeholders.
pub fn convert_with_summary(bytes: &[u8]) -> Result<(Vec<u8>, Summary)> {
    let raw = payload(bytes)?;
    let mut input = Cursor {
        data: &raw,
        position: 0,
    };
    let schema = input.u32()?;
    ensure!(schema != SCHEMA, "Input is already PC reanim compiled");
    ensure!(schema == 0, "Unsupported TV reanim schema: 0x{schema:08X}");
    input.take(4)?; // Serialized TV pointers are not portable, and can be nonzero.
    let tracks = input.u32()? as usize;
    let fps_bits = input.u32()?;
    let fps = f32::from_bits(fps_bits);
    ensure!(fps.is_finite() && fps > 0., "Invalid TV frame rate");
    input.take(4)?;
    ensure!(input.u32()? == 20, "Invalid TV RDef marker; expected 0x14");
    ensure!(
        tracks <= MAX_TRACKS && tracks <= (raw.len() - input.position) / 20,
        "Invalid TV track count"
    );
    let mut counts = Vec::with_capacity(tracks);
    for _ in 0..tracks {
        input.take(12)?;
        let frames = input.u32()? as usize;
        ensure!(
            frames <= MAX_FRAMES,
            "TV track frame count exceeds the engine limit"
        );
        input.take(4)?;
        counts.push(frames);
    }
    let summary = Summary {
        tracks,
        frames: counts.iter().copied().max().unwrap_or(0),
        fps,
    };
    let mut output = Vec::with_capacity(raw.len());
    for value in [SCHEMA, 0, tracks as u32, fps_bits, 0, 12] {
        word(&mut output, value);
    }
    for &frames in &counts {
        for value in [0, 0, frames as u32] {
            word(&mut output, value);
        }
    }
    for frames in counts {
        output.extend_from_slice(input.string()?);
        ensure!(
            input.u32()? == 48,
            "Invalid TV transform marker; expected 0x30"
        );
        word(&mut output, 44);
        // Each TV frame needs 48 fixed bytes and six string-length words.
        ensure!(
            frames <= (raw.len() - input.position) / 72,
            "Truncated TV transform/string array"
        );
        let transforms = input.take(frames * 48)?;
        for transform in transforms.as_chunks::<48>().0 {
            output.extend_from_slice(&transform[..32]);
            output.extend_from_slice(&[0; 12]);
        }
        for _ in 0..frames {
            output.extend_from_slice(input.string()?); // i
            input.string()?; // iPath
            input.string()?; // i2
            input.string()?; // i2Path
            output.extend_from_slice(input.string()?); // font
            output.extend_from_slice(input.string()?); // text
        }
    }
    ensure!(
        input.position == raw.len(),
        "Trailing bytes after TV reanim payload"
    );
    let mut wrapped = Vec::new();
    word(&mut wrapped, COOKIE);
    word(&mut wrapped, output.len() as u32);
    let mut encoder = ZlibEncoder::new(wrapped, Compression::best());
    encoder.write_all(&output)?;
    Ok((encoder.finish()?, summary))
}
