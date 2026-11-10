//! Shared native bitmap decoding for ordinary PNG and JPEG assets.
//!
//! jpeg-decoder's integer IDCT/upsampling stays close to the game's libjpeg
//! backend. zune-jpeg produced channel deltas up to 13 on the real selector
//! background; this decoder keeps that regression within the declared tolerance.
use anyhow::{Context, Result, ensure};
use image::RgbaImage;
use jpeg_decoder::{Decoder, PixelFormat};
use std::{fs, io::Cursor, path::Path};

pub fn open(path: &Path) -> Result<RgbaImage> {
    decode(&fs::read(path)?).with_context(|| format!("Invalid image {}", path.display()))
}

pub fn decode(bytes: &[u8]) -> Result<RgbaImage> {
    if !bytes.starts_with(b"\xff\xd8") {
        return Ok(image::load_from_memory(bytes)?.to_rgba8());
    }
    let mut decoder = Decoder::new(Cursor::new(bytes));
    decoder.set_max_decoding_buffer_size(256 * 1024 * 1024);
    decoder.read_info().context("Invalid JPEG header")?;
    let info = decoder.info().context("JPEG dimensions missing")?;
    let width = u32::from(info.width);
    let height = u32::from(info.height);
    ensure!(
        width > 0 && height > 0 && u64::from(width) * u64::from(height) <= 64 * 1024 * 1024,
        "JPEG exceeds 64 megapixel decode limit"
    );
    let pixels = decoder.decode().context("Invalid JPEG pixels")?;
    let count = width as usize * height as usize;
    ensure!(
        pixels.len() == count * info.pixel_format.pixel_bytes(),
        "JPEG pixel count differs"
    );
    let mut rgba = Vec::with_capacity(count * 4);
    match info.pixel_format {
        PixelFormat::RGB24 => {
            for pixel in pixels.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
        }
        PixelFormat::L8 => {
            for value in pixels {
                rgba.extend_from_slice(&[value, value, value, 255]);
            }
        }
        PixelFormat::L16 => {
            for pixel in pixels.as_chunks::<2>().0 {
                let value = (u16::from_ne_bytes([pixel[0], pixel[1]]) >> 8) as u8;
                rgba.extend_from_slice(&[value, value, value, 255]);
            }
        }
        PixelFormat::CMYK32 => {
            for pixel in pixels.as_chunks::<4>().0 {
                let k = 255 - u16::from(pixel[3]);
                for channel in &pixel[..3] {
                    rgba.push((((255 - u16::from(*channel)) * k + 127) / 255) as u8);
                }
                rgba.push(255);
            }
        }
    }
    RgbaImage::from_raw(width, height, rgba).context("Invalid decoded JPEG dimensions")
}
