use fla_reanim::{MISSING, codec, tv};
use flate2::{Compression, write::ZlibEncoder};
use std::io::Write;

fn word(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn string(bytes: &mut Vec<u8>, value: &[u8]) {
    word(bytes, value.len() as u32);
    bytes.extend_from_slice(value);
}

fn wrapped(body: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    word(&mut result, codec::COOKIE);
    word(&mut result, body.len() as u32);
    let mut encoder = ZlibEncoder::new(result, Compression::best());
    encoder.write_all(body).unwrap();
    encoder.finish().unwrap()
}

struct Fixture {
    source: Vec<u8>,
    expected: Vec<u8>,
    track_marker: usize,
    discarded_string: usize,
}

fn fixture() -> Fixture {
    let values = [
        MISSING.to_bits(),
        (-0.0_f32).to_bits(),
        1, // Smallest positive subnormal, preserved without arithmetic.
        f32::MAX.to_bits(),
        f32::MIN_POSITIVE.to_bits(),
        (-2.25_f32).to_bits(),
        0,
        1.0_f32.to_bits(),
    ];
    let missing = [MISSING.to_bits(); 8];
    let name = b"track\xff\x80\0";
    let empty_name = b"empty\xfe";
    let image = b"IMAGE_\xe9\xff\0";
    let font = b"FONT_\x81";
    let text = b"text\x80\xff";
    let mut source = Vec::new();
    for value in [0, 0x1B6069B8, 2, 12.5_f32.to_bits(), 0, 20] {
        word(&mut source, value);
    }
    for count in [2, 0] {
        for value in [0x1234, 0x5678, 0x9ABC, count, 0xDEF0] {
            word(&mut source, value);
        }
    }
    string(&mut source, name);
    let track_marker = source.len();
    word(&mut source, 48);
    for frame in [values, missing] {
        for value in frame {
            word(&mut source, value);
        }
        for pointer in [0x1111, 0x2222, 0x3333, 0x4444] {
            word(&mut source, pointer);
        }
    }
    string(&mut source, image);
    let discarded_string = source.len();
    for value in [
        b"path\x81".as_slice(),
        b"second\xff",
        b"second-path\xfe",
        font,
        text,
    ] {
        string(&mut source, value);
    }
    for value in [
        b"".as_slice(),
        b"unused-path",
        b"unused-image",
        b"unused-image-path",
        b"",
        b"",
    ] {
        string(&mut source, value);
    }
    string(&mut source, empty_name);
    word(&mut source, 48);

    // Independent expected PC body: smaller struct headers, original float
    // words, zero pointer slots, and only image/font/text strings retained.
    let mut expected = Vec::new();
    for value in [
        codec::SCHEMA,
        0,
        2,
        12.5_f32.to_bits(),
        0,
        12,
        0,
        0,
        2,
        0,
        0,
        0,
    ] {
        word(&mut expected, value);
    }
    string(&mut expected, name);
    word(&mut expected, 44);
    for frame in [values, missing] {
        for value in frame {
            word(&mut expected, value);
        }
        expected.extend_from_slice(&[0; 12]);
    }
    for value in [image.as_slice(), font, text, b"", b"", b""] {
        string(&mut expected, value);
    }
    string(&mut expected, empty_name);
    word(&mut expected, 44);
    Fixture {
        source,
        expected,
        track_marker,
        discarded_string,
    }
}

#[test]
fn raw_tv_preserves_float_words_missing_and_non_utf8_bytes() {
    let data = fixture();
    let (output, summary) = tv::convert_with_summary(&data.source).unwrap();
    assert_eq!(codec::decompress(&output).unwrap(), data.expected);
    assert_eq!(
        summary,
        tv::Summary {
            tracks: 2,
            frames: 2,
            fps: 12.5
        }
    );
    assert_eq!(output, tv::convert(&data.source).unwrap());
    // The output intentionally retains Latin-1 bytes rather than rewriting
    // them as UTF-8 just to fit the ordinary author's String-based reader.
    assert!(codec::decode(&output).is_err());
}

#[test]
fn wrapped_and_raw_tv_produce_the_same_canonical_pc_container() {
    let data = fixture();
    let packed = wrapped(&data.source);
    let from_raw = tv::convert(&data.source).unwrap();
    let from_wrapped = tv::convert(&packed).unwrap();
    assert_eq!(from_raw, from_wrapped);
    assert_eq!(&from_wrapped[..4], &codec::COOKIE.to_le_bytes());
    assert_eq!(
        u32::from_le_bytes(from_wrapped[4..8].try_into().unwrap()) as usize,
        data.expected.len()
    );
    assert_eq!(codec::decompress(&from_wrapped).unwrap(), data.expected);
}

#[test]
fn empty_tv_definition_has_zero_tracks_and_frames() {
    let mut data = Vec::new();
    for value in [0, 0x12345678, 0, 24_f32.to_bits(), 0, 20] {
        word(&mut data, value);
    }
    let (output, summary) = tv::convert_with_summary(&data).unwrap();
    let decoded = codec::decode(&output).unwrap();
    assert!(decoded.tracks.is_empty());
    assert_eq!(decoded.fps, 24.0);
    assert_eq!(
        summary,
        tv::Summary {
            tracks: 0,
            frames: 0,
            fps: 24.0
        }
    );
}

#[test]
fn pc_input_is_explicitly_rejected_in_both_forms() {
    let data = fixture();
    for pc in [&data.expected, &wrapped(&data.expected)] {
        assert!(
            tv::convert(pc)
                .unwrap_err()
                .to_string()
                .contains("already PC")
        );
    }
}

#[test]
fn every_truncated_prefix_is_rejected() {
    let raw = fixture().source;
    let compressed = wrapped(&raw);
    for data in [&raw, &compressed] {
        for end in 0..data.len() {
            assert!(
                tv::convert(&data[..end]).is_err(),
                "accepted truncated prefix {end}/{}",
                data.len()
            );
        }
    }
}

#[test]
fn wrong_schema_markers_counts_and_frame_rate_are_rejected() {
    let data = fixture();
    for (position, invalid) in [
        (0, 1),
        (20, 12),
        (data.track_marker, 44),
        (8, u32::MAX),
        (24 + 12, u32::MAX),
        (12, 0),
        (12, f32::NAN.to_bits()),
        (12, (-12_f32).to_bits()),
    ] {
        let mut malformed = data.source.clone();
        malformed[position..position + 4].copy_from_slice(&invalid.to_le_bytes());
        assert!(
            tv::convert(&malformed).is_err(),
            "accepted invalid word at {position}"
        );
    }
}

#[test]
fn retained_and_discarded_string_lengths_are_both_checked() {
    let data = fixture();
    for position in [24 + 20 * 2, data.discarded_string] {
        for invalid in [100_001_u32, u32::MAX] {
            let mut malformed = data.source.clone();
            malformed[position..position + 4].copy_from_slice(&invalid.to_le_bytes());
            assert!(tv::convert(&malformed).is_err());
        }
    }
}

#[test]
fn container_size_checksum_and_trailing_bytes_are_strict() {
    let source = fixture().source;
    let valid = wrapped(&source);
    for declared in [
        source.len() as u32 - 1,
        source.len() as u32 + 1,
        0,
        u32::MAX,
    ] {
        let mut malformed = valid.clone();
        malformed[4..8].copy_from_slice(&declared.to_le_bytes());
        assert!(tv::convert(&malformed).is_err());
    }
    let mut corrupted = valid.clone();
    *corrupted.last_mut().unwrap() ^= 1;
    assert!(tv::convert(&corrupted).is_err());
    let mut raw_trailing = source;
    raw_trailing.push(0);
    assert!(tv::convert(&raw_trailing).is_err());
    let mut compressed_trailing = valid.clone();
    compressed_trailing.push(0);
    assert!(tv::convert(&compressed_trailing).is_err());
    let mut concatenated_streams = valid;
    concatenated_streams.extend_from_slice(&wrapped(&[])[8..]);
    assert!(tv::convert(&concatenated_streams).is_err());
}
