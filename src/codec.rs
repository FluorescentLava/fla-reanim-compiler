//! Canonical x86 TodLib DefMap layout, shared with the game's Definition.cpp.
use crate::{DEFAULTS, Definition, MISSING, Track, Transform};
use anyhow::{Context, Result, ensure};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use std::io::{Read, Write};

const MAX_BYTES: usize = 512 * 1024 * 1024;
pub const COOKIE: u32 = 0xDEADFED4;
pub const SCHEMA: u32 = 0xB393B4C0;

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(size)
            .context("compiled offset overflow")?;
        ensure!(
            end <= self.bytes.len(),
            "truncated compiled at byte {}",
            self.pos
        );
        let result = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(result)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn string(&mut self) -> Result<String> {
        let size = self.u32()? as usize;
        ensure!(size <= 100_000, "compiled string exceeds engine limit");
        Ok(std::str::from_utf8(self.take(size)?)
            .context("compiled string is not UTF-8")?
            .to_owned())
    }
}

pub fn decompress(bytes: &[u8]) -> Result<Vec<u8>> {
    ensure!(bytes.len() >= 8, "truncated compiled header");
    ensure!(
        u32::from_le_bytes(bytes[..4].try_into()?) == COOKIE,
        "not a compiled DefMap resource"
    );
    let size = u32::from_le_bytes(bytes[4..8].try_into()?) as usize;
    ensure!(size <= MAX_BYTES, "compiled payload exceeds 512 MiB limit");
    let core_end = if bytes.ends_with(b"RPLS") {
        ensure!(bytes.len() >= 16, "truncated RPLS trailer");
        let length =
            u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into()?) as usize;
        bytes
            .len()
            .checked_sub(8 + length)
            .context("invalid RPLS extension length")?
    } else {
        bytes.len()
    };
    ensure!(core_end >= 8, "invalid core length");
    let mut decoder = ZlibDecoder::new(&bytes[8..core_end]);
    let mut payload = Vec::new();
    decoder
        .by_ref()
        .take(size as u64 + 1)
        .read_to_end(&mut payload)
        .context("invalid compiled zlib stream")?;
    ensure!(
        payload.len() == size,
        "compiled declared size {size} differs from decoded {}",
        payload.len()
    );
    ensure!(
        decoder.total_in() as usize == core_end - 8,
        "unrecognized bytes after compiled zlib stream"
    );
    Ok(payload)
}

pub fn decode(bytes: &[u8]) -> Result<Definition> {
    let payload = decompress(bytes)?;
    let mut c = Cursor {
        bytes: &payload,
        pos: 0,
    };
    ensure!(
        c.u32()? == SCHEMA,
        "unsupported schema; expected reanim 0xB393B4C0 (particles and TV layouts are different)"
    );
    c.take(4)?;
    let count = c.u32()? as usize;
    let fps = f32::from_bits(c.u32()?);
    ensure!(fps.is_finite() && fps > 0., "invalid frame rate");
    c.take(4)?;
    ensure!(c.u32()? == 12, "unsupported track struct size");
    ensure!(
        count <= 100_000 && count <= payload.len() / 12,
        "invalid track count"
    );
    let mut counts = Vec::with_capacity(count);
    for _ in 0..count {
        c.take(8)?;
        let frames = c.u32()? as usize;
        ensure!(frames <= 1_000_000, "track frame count exceeds limit");
        counts.push(frames);
    }
    let mut tracks = Vec::with_capacity(count);
    for frames in counts {
        let name = c.string()?;
        ensure!(c.u32()? == 44, "unsupported transform struct size");
        ensure!(
            frames <= (payload.len() - c.pos) / 44,
            "truncated transform array"
        );
        let mut transforms = Vec::with_capacity(frames);
        for _ in 0..frames {
            let mut values = [0.; 8];
            for value in &mut values {
                *value = f32::from_bits(c.u32()?);
                ensure!(value.is_finite(), "non-finite transform in track {name}");
            }
            c.take(12)?;
            transforms.push(Transform {
                values,
                ..Default::default()
            });
        }
        for t in &mut transforms {
            t.image = c.string()?;
            t.font = c.string()?;
            t.text = c.string()?;
        }
        tracks.push(Track { name, transforms });
    }
    ensure!(
        c.pos == payload.len(),
        "{} unconsumed bytes in compiled payload",
        payload.len() - c.pos
    );
    Ok(Definition { fps, tracks })
}

fn u32_into(out: &mut Vec<u8>, value: u32) {
    out.extend(value.to_le_bytes());
}
fn string_into(out: &mut Vec<u8>, value: &str) -> Result<()> {
    ensure!(value.len() <= 100_000, "string exceeds engine limit");
    u32_into(out, value.len() as u32);
    out.extend(value.as_bytes());
    Ok(())
}

/// `compact=false` preserves raw -10000 and string placeholders exactly.
pub fn encode(definition: &Definition, compact: bool) -> Result<Vec<u8>> {
    ensure!(
        definition.fps.is_finite() && definition.fps > 0.,
        "invalid frame rate"
    );
    ensure!(definition.tracks.len() <= 100_000, "too many tracks");
    let mut raw = Vec::new();
    for value in [
        SCHEMA,
        0,
        definition.tracks.len() as u32,
        definition.fps.to_bits(),
        0,
        12,
    ] {
        u32_into(&mut raw, value);
    }
    for track in &definition.tracks {
        ensure!(track.transforms.len() <= 1_000_000, "too many frames");
        for value in [0, 0, track.transforms.len() as u32] {
            u32_into(&mut raw, value);
        }
    }
    for track in &definition.tracks {
        string_into(&mut raw, &track.name)?;
        u32_into(&mut raw, 44);
        let mut transforms = track.transforms.clone();
        if compact {
            let mut previous = Transform {
                values: DEFAULTS,
                ..Default::default()
            };
            for t in &mut transforms {
                for (value, prev) in t.values.iter_mut().zip(&mut previous.values) {
                    if *value == MISSING || *value == *prev {
                        *value = MISSING;
                    } else {
                        *prev = *value;
                    }
                }
                for (value, prev) in [
                    (&mut t.image, &mut previous.image),
                    (&mut t.font, &mut previous.font),
                    (&mut t.text, &mut previous.text),
                ] {
                    if value.is_empty() || value == prev {
                        value.clear();
                    } else {
                        prev.clone_from(value);
                    }
                }
            }
        }
        for t in &transforms {
            for value in t.values {
                ensure!(value.is_finite(), "non-finite transform");
                u32_into(&mut raw, value.to_bits());
            }
            raw.extend([0; 12]);
        }
        for t in &transforms {
            string_into(&mut raw, &t.image)?;
            string_into(&mut raw, &t.font)?;
            string_into(&mut raw, &t.text)?;
        }
        ensure!(
            raw.len() <= MAX_BYTES,
            "compiled payload exceeds size limit"
        );
    }
    let mut output = Vec::new();
    u32_into(&mut output, COOKIE);
    u32_into(&mut output, raw.len() as u32);
    let mut encoder = ZlibEncoder::new(output, Compression::best());
    encoder.write_all(&raw)?;
    Ok(encoder.finish()?)
}
