//! Reproduce keyframe compression and matrix-corner error measurements.
//! Run `cargo run --release --example tween_quality -- --help` for usage.
use anyhow::{Context, Result, bail, ensure};
use fla_reanim::{
    Definition, Transform, codec,
    tween::{self, Tolerance},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

const FIELDS: [usize; 7] = [0, 1, 2, 3, 4, 5, 7];

struct Options {
    root: PathBuf,
    inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
    subdivisions: usize,
    preset: String,
}
fn options() -> Result<Option<Options>> {
    let mut root = None;
    let mut inputs = Vec::new();
    let mut output = None;
    let mut subdivisions = 16;
    let mut preset = "both".to_owned();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => {
                println!(
                    "Usage: cargo run --release --example tween_quality -- --resource-root <Resources> [--output <report.jsonl>] [--subframes 16] [--preset both|strict|editable] <file.reanim.compiled|directory> ...\n\nReads compiled files and the actual source image dimensions. It measures the numerical reanim/classic-tween model; it does not automate Adobe or compare screenshots. Discrete artwork/frame switches and explicitly unfitted transitions are excluded from fitting error and counted separately."
                );
                return Ok(None);
            }
            Some("--resource-root") => {
                root = Some(PathBuf::from(
                    args.next()
                        .context("--resource-root requires a directory")?,
                ))
            }
            Some("--output") => {
                output = Some(PathBuf::from(
                    args.next().context("--output requires a filename")?,
                ))
            }
            Some("--subframes") => {
                subdivisions = args
                    .next()
                    .context("--subframes requires a number")?
                    .to_str()
                    .context("invalid --subframes")?
                    .parse()?
            }
            Some("--preset") => {
                preset = args
                    .next()
                    .context("--preset requires both, strict or editable")?
                    .to_str()
                    .context("invalid preset")?
                    .to_owned()
            }
            Some(value) if value.starts_with('-') => bail!("Unknown option: {value}"),
            _ => inputs.push(PathBuf::from(arg)),
        }
    }
    let root = root.context("Specify --resource-root <Resources>; use --help for an example")?;
    ensure!(
        root.is_dir(),
        "Resource root is not a directory: {}",
        root.display()
    );
    ensure!(
        (1..=1024).contains(&subdivisions),
        "--subframes must be in 1..=1024"
    );
    ensure!(
        ["both", "strict", "editable"].contains(&preset.as_str()),
        "Invalid preset: {preset}"
    );
    if inputs.is_empty() {
        inputs.push(root.join("compiled/reanim"));
    }
    let mut expanded = Vec::new();
    for input in inputs {
        if input.is_dir() {
            let mut files = fs::read_dir(&input)?
                .map(|entry| Ok(entry?.path()))
                .collect::<Result<Vec<_>>>()?;
            files.retain(|path| {
                path.is_file()
                    && path
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .ends_with(".reanim.compiled")
            });
            files.sort();
            expanded.extend(files);
        } else {
            expanded.push(input);
        }
    }
    ensure!(!expanded.is_empty(), "No compiled input files");
    Ok(Some(Options {
        root,
        inputs: expanded,
        output,
        subdivisions,
        preset,
    }))
}

fn normalize(value: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for c in value.to_uppercase().chars() {
        if c.is_ascii_alphanumeric() {
            if separator && !result.is_empty() {
                result.push('_');
            }
            result.push(c);
            separator = false;
        } else {
            separator = true;
        }
    }
    result
}
fn find_image(stem: &Path) -> Option<PathBuf> {
    let is_image = |path: &Path| {
        path.extension().is_some_and(|ext| {
            matches!(
                ext.to_string_lossy().to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg"
            )
        })
    };
    if stem.is_file() && is_image(stem) {
        return Some(stem.to_owned());
    }
    for extension in ["png", "jpg", "jpeg"] {
        let candidate = stem.with_extension(extension);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let name = stem.file_stem()?.to_string_lossy();
    fs::read_dir(stem.parent()?)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            is_image(p)
                && p.file_stem()
                    .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(&name))
        })
}
fn image_dimensions(path: &Path) -> Result<(u32, u32)> {
    let mut reader = BufReader::new(fs::File::open(path)?);
    if reader.fill_buf()?.starts_with(b"\xff\xd8") {
        let mut decoder = jpeg_decoder::Decoder::new(reader);
        decoder.read_info()?;
        let info = decoder.info().context("JPEG dimensions missing")?;
        Ok((u32::from(info.width), u32::from(info.height)))
    } else {
        Ok(image::ImageReader::new(reader)
            .with_guessed_format()?
            .into_dimensions()?)
    }
}
fn catalog(root: &Path) -> Result<HashMap<String, (u32, u32)>> {
    let mut paths = HashMap::new();
    let text = fs::read_to_string(root.join("resources.xml"))
        .context("Read resource-root/resources.xml")?;
    let document = roxmltree::Document::parse(&text)?;
    let mut folder = "";
    let mut prefix = "";
    for node in document.descendants().filter(|node| node.is_element()) {
        match node.tag_name().name() {
            "SetDefaults" => {
                folder = node.attribute("path").unwrap_or(folder);
                prefix = node.attribute("idprefix").unwrap_or(prefix);
            }
            "Image" => {
                if let Some(relative) = node.attribute("path") {
                    let stem = root
                        .join(folder.replace('\\', "/"))
                        .join(relative.replace('\\', "/"));
                    if let Some(path) = find_image(&stem) {
                        let fallback = stem.file_stem().unwrap_or_default().to_string_lossy();
                        let id = format!("{}{}", prefix, node.attribute("id").unwrap_or(&fallback));
                        paths.entry(id.to_uppercase()).or_insert(path);
                    }
                }
            }
            _ => {}
        }
    }
    for directory in ["reanim", "images", "particles"] {
        // Sorting makes a possible ambiguous fallback resource ID deterministic.
        let mut files = walkdir::WalkDir::new(root.join(directory))
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .collect::<Vec<_>>();
        files.sort();
        for path in files {
            if !path.extension().is_some_and(|ext| {
                matches!(
                    ext.to_string_lossy().to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg"
                )
            }) {
                continue;
            }
            let stem = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_uppercase();
            let normalized = normalize(&stem);
            for id in [
                stem,
                normalized.clone(),
                format!("IMAGE_REANIM_{normalized}"),
                format!("IMAGE_{normalized}"),
            ] {
                paths.entry(id).or_insert_with(|| path.clone());
            }
        }
    }
    paths
        .into_iter()
        .map(|(id, path)| {
            Ok((
                id,
                image_dimensions(&path)
                    .with_context(|| format!("Read image dimensions {}", path.display()))?,
            ))
        })
        .collect()
}

#[derive(Serialize)]
struct WorstSample {
    track: String,
    frame: usize,
    fraction: f64,
    image: String,
    width: u32,
    height: u32,
    corner: [f64; 2],
}
#[derive(Serialize)]
struct Quality {
    method: &'static str,
    crate_version: &'static str,
    file: String,
    input_sha256: String,
    preset: String,
    tolerance: Tolerance,
    subdivisions_per_frame: usize,
    samples: usize,
    keys: usize,
    tween_spans: usize,
    multiframe_tweens: usize,
    unfitted_intervals: usize,
    excluded_discrete_transitions: usize,
    excluded_nonrendering_intervals: usize,
    excluded_missing_image_intervals: usize,
    missing_image_ids: BTreeSet<String>,
    max_parameter_errors: [f64; 8],
    sampled_corner_max_px: f64,
    continuous_corner_bound_px: f64,
    sampled_subframe_positions: u64,
    worst_sample: Option<WorstSample>,
}
fn same_discrete(a: &Transform, b: &Transform) -> bool {
    a.image == b.image && a.font == b.font && a.text == b.text && a.values[6] == b.values[6]
}
fn matrix(v: [f64; 8]) -> [f64; 6] {
    let x = v[2].to_radians();
    let y = v[3].to_radians();
    [
        v[4] * x.cos(),
        v[4] * x.sin(),
        -v[5] * y.sin(),
        v[5] * y.cos(),
        v[0],
        v[1],
    ]
}
fn analyze(
    input: &Path,
    bytes: &[u8],
    definition: &Definition,
    dimensions: &HashMap<String, (u32, u32)>,
    preset: &str,
    tolerance: Tolerance,
    subdivisions: usize,
) -> Quality {
    let mut quality = Quality {
        method: "classic-transform-matrix-corners-v1",
        crate_version: env!("CARGO_PKG_VERSION"),
        file: input
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        input_sha256: format!("{:x}", Sha256::digest(bytes)),
        preset: preset.to_owned(),
        tolerance,
        subdivisions_per_frame: subdivisions,
        samples: 0,
        keys: 0,
        tween_spans: 0,
        multiframe_tweens: 0,
        unfitted_intervals: 0,
        excluded_discrete_transitions: 0,
        excluded_nonrendering_intervals: 0,
        excluded_missing_image_intervals: 0,
        missing_image_ids: BTreeSet::new(),
        max_parameter_errors: [0.; 8],
        sampled_corner_max_px: 0.,
        continuous_corner_bound_px: 0.,
        sampled_subframe_positions: 0,
        worst_sample: None,
    };
    for track in &definition.tracks {
        if track.name.to_ascii_lowercase().starts_with("anim_") {
            continue;
        }
        let plan = tween::plan(&track.transforms, tolerance);
        quality.samples += track.transforms.len();
        quality.keys += plan.spans.len();
        quality.tween_spans += plan.tween_count();
        quality.multiframe_tweens += plan.compressed_tween_count();
        quality.unfitted_intervals += plan.unfitted_intervals.len();
        for i in FIELDS {
            quality.max_parameter_errors[i] =
                quality.max_parameter_errors[i].max(plan.max_errors[i]);
        }
        let mut width = 0f64;
        let mut height = 0f64;
        let mut scale_x = 0f64;
        let mut scale_y = 0f64;
        for sample in &track.transforms {
            if let Some(&(w, h)) = dimensions.get(&sample.image.to_uppercase()) {
                width = width.max(f64::from(w));
                height = height.max(f64::from(h));
                scale_x = scale_x.max(f64::from(sample.values[4]).abs());
                scale_y = scale_y.max(f64::from(sample.values[5]).abs());
            }
        }
        let errors = plan.max_errors;
        let column_x = errors[4] + 2. * scale_x * (errors[2].to_radians() / 2.).sin();
        let column_y = errors[5] + 2. * scale_y * (errors[3].to_radians() / 2.).sin();
        quality.continuous_corner_bound_px = quality
            .continuous_corner_bound_px
            .max(errors[0].hypot(errors[1]) + width * column_x + height * column_y);
        for span in plan.spans {
            for offset in 0..span.duration {
                let at = span.start + offset;
                if at + 1 >= track.transforms.len() || plan.unfitted_intervals.contains(&at) {
                    continue;
                }
                let a = &track.transforms[at];
                let b = &track.transforms[at + 1];
                if !same_discrete(a, b) {
                    quality.excluded_discrete_transitions += 1;
                    continue;
                }
                if a.values[6] < 0. || a.image.is_empty() {
                    quality.excluded_nonrendering_intervals += 1;
                    continue;
                }
                let Some(&(width, height)) = dimensions.get(&a.image.to_uppercase()) else {
                    quality.excluded_missing_image_intervals += 1;
                    quality.missing_image_ids.insert(a.image.clone());
                    continue;
                };
                let w = f64::from(width);
                let h = f64::from(height);
                for step in 0..=subdivisions {
                    let fraction = step as f64 / subdivisions as f64;
                    let mut source = a.values.map(f64::from);
                    for i in FIELDS {
                        source[i] += (f64::from(b.values[i]) - source[i]) * fraction;
                    }
                    let target = tween::sample_span(&track.transforms, span, at as f64 + fraction);
                    let actual = matrix(source);
                    let predicted = matrix(target);
                    quality.sampled_subframe_positions += 1;
                    for (x, y) in [(0., 0.), (w, 0.), (0., h), (w, h)] {
                        let dx = (actual[0] - predicted[0]) * x
                            + (actual[2] - predicted[2]) * y
                            + actual[4]
                            - predicted[4];
                        let dy = (actual[1] - predicted[1]) * x
                            + (actual[3] - predicted[3]) * y
                            + actual[5]
                            - predicted[5];
                        let error = dx.hypot(dy);
                        if error > quality.sampled_corner_max_px {
                            quality.sampled_corner_max_px = error;
                            quality.worst_sample = Some(WorstSample {
                                track: track.name.clone(),
                                frame: at,
                                fraction,
                                image: a.image.clone(),
                                width,
                                height,
                                corner: [x, y],
                            });
                        }
                    }
                }
            }
        }
    }
    quality
}

fn main() -> Result<()> {
    let Some(options) = options()? else {
        return Ok(());
    };
    let dimensions = catalog(&options.root)?;
    let destination: Box<dyn Write> = if let Some(path) = &options.output {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        Box::new(fs::File::create(path)?)
    } else {
        Box::new(std::io::stdout())
    };
    let mut output = BufWriter::new(destination);
    for input in &options.inputs {
        let bytes = fs::read(input).with_context(|| format!("Read {}", input.display()))?;
        let definition = codec::decode(&bytes)
            .with_context(|| format!("Decode {}", input.display()))?
            .expanded();
        for (name, tolerance) in [
            ("strict", Tolerance::default()),
            ("editable", Tolerance::editable()),
        ] {
            if options.preset != "both" && options.preset != name {
                continue;
            }
            serde_json::to_writer(
                &mut output,
                &analyze(
                    input,
                    &bytes,
                    &definition,
                    &dimensions,
                    name,
                    tolerance,
                    options.subdivisions,
                ),
            )?;
            output.write_all(b"\n")?;
        }
    }
    output.flush()?;
    Ok(())
}
