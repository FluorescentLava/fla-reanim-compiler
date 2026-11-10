//! Compiled definitions to editable Animate ZIP/XFL documents.
//!
//! The DAT container uses one zlib stream, split into length-prefixed fragments.
//! Compressing each fragment independently corrupts every image larger than 32 KiB.
use crate::{Definition, MISSING, Transform};
use anyhow::{Context, Result, bail, ensure};
use flate2::{Compression, write::ZlibEncoder};
use image::{ImageBuffer, ImageFormat, RgbaImage};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const ORIGINAL: &str = "META-INF/reanim-source.compiled";
const HASH: &str = "META-INF/reanim-source.sha256";
const ASSETS: &str = "META-INF/reanim-source.assets.json";
const SPRITES: &str = "META-INF/reanim-source.sprites.json";
const NS: &str = "http://ns.adobe.com/xfl/2008/";

#[derive(Debug, Serialize)]
pub struct Report {
    pub stage: [u32; 2],
    pub tracks: usize,
    pub frames: usize,
    pub bitmaps: usize,
    pub source_samples: usize,
    pub keyframes: usize,
    pub motion_tweens: usize,
    pub multi_frame_tweens: usize,
    pub tween_tolerance: crate::tween::Tolerance,
    pub warnings: Vec<String>,
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn fingerprint(entries: &BTreeMap<String, Vec<u8>>) -> String {
    let mut hash = Sha256::new();
    for (name, data) in entries {
        if name.starts_with("META-INF/reanim-source.") {
            continue;
        }
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update(data);
    }
    format!("{:x}", hash.finalize())
}

/// Recover the exact original only when every editable ZIP member is unchanged.
/// Editing a matrix, symbol, DAT or linked PNG invalidates the recovery record.
pub fn original_if_unchanged(input: &Path) -> Result<Option<Vec<u8>>> {
    if !input.is_file() {
        return Ok(None);
    }
    let file = fs::File::open(input)?;
    let mut archive = match ZipArchive::new(file) {
        Ok(a) => a,
        Err(_) => return Ok(None),
    };
    if archive.index_for_name(ORIGINAL).is_none() || archive.index_for_name(HASH).is_none() {
        return Ok(None);
    }
    let mut entries = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        ensure!(
            entry.size() <= 512 * 1024 * 1024,
            "FLA member exceeds 512 MiB limit"
        );
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        ensure!(
            entries.insert(entry.name().to_owned(), data).is_none(),
            "Duplicate ZIP member"
        );
    }
    let expected = String::from_utf8_lossy(entries.get(HASH).unwrap());
    if fingerprint(&entries) == expected.trim() {
        Ok(entries.remove(ORIGINAL))
    } else {
        Ok(None)
    }
}

/// Restore the external images referenced by an unchanged RPLS definition.
/// All output paths are constrained to the staged compiled file's directory.
pub fn restore_assets(input: &Path, output: &Path) -> Result<()> {
    let mut archive = ZipArchive::new(fs::File::open(input)?)?;
    let mapping: BTreeMap<String, String> = match archive.by_name(ASSETS) {
        Ok(mut member) => {
            let mut data = Vec::new();
            member.read_to_end(&mut data)?;
            serde_json::from_slice(&data)?
        }
        Err(zip::result::ZipError::FileNotFound) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let parent = output
        .parent()
        .context("invalid compiled output directory")?;
    for (relative, member) in mapping {
        let path = Path::new(&relative);
        ensure!(
            !relative.contains('\\')
                && !relative.contains(':')
                && !relative.starts_with('/')
                && path
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
            "Unsafe RPLS asset path: {relative}"
        );
        ensure!(
            member.starts_with("META-INF/reanim-assets/"),
            "Invalid RPLS asset member"
        );
        let mut source = archive.by_name(&member)?;
        ensure!(
            source.size() <= 128 * 1024 * 1024,
            "Asset exceeds 128 MiB limit"
        );
        let target = parent.join(path);
        fs::create_dir_all(target.parent().context("invalid asset target")?)?;
        let mut file = fs::File::create(target)?;
        std::io::copy(&mut source, &mut file)?;
    }
    Ok(())
}

/// Export editable library pixels beside a newly compiled classic resource.
/// Discover nearby resources, or use the historical reanim-directory fallback.
pub fn export_classic_assets(input: &Path, output: &Path) -> Result<usize> {
    export_classic_assets_with_resources(input, output, None)
}

/// Export edited pixels to the same resource paths the game actually loads.
/// This uses the editable library itself, including Adobe-saved files without
/// the converter's source-recovery metadata.
pub fn export_classic_assets_with_resources(
    input: &Path,
    output: &Path,
    resource_root: Option<&Path>,
) -> Result<usize> {
    let members = crate::forward::read_package(input)?;
    let (document_name, document) = members
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("DOMDocument.xml"))
        .or_else(|| {
            members
                .iter()
                .find(|(name, _)| name.to_ascii_lowercase().ends_with("/domdocument.xml"))
        })
        .context("FLA has no DOMDocument.xml")?;
    let prefix = &document_name[..document_name.len() - "DOMDocument.xml".len()];
    let member = |name: &str| {
        let name = format!("{prefix}{}", name.replace('\\', "/"));
        members.get(&name).or_else(|| {
            members
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(&name))
                .map(|(_, value)| value)
        })
    };
    let xml = std::str::from_utf8(document)?;
    let doc = roxmltree::Document::parse(xml)?;
    let catalog = Some(Catalog::new(input, resource_root));
    let parent = output
        .parent()
        .context("invalid compiled output directory")?;
    let mut mapping = BTreeMap::new();
    let mut sprites: BTreeMap<String, Vec<(usize, RgbaImage)>> = BTreeMap::new();
    for bitmap in doc
        .descendants()
        .filter(|n| n.has_tag_name("DOMBitmapItem"))
    {
        let name = bitmap.attribute("name").context("bitmap has no name")?;
        let name = name.replace('\\', "/");
        let bare = Path::new(&name)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        let cel = bare
            .rsplit_once("__REANIM_CEL_")
            .and_then(|(name, index)| Some((name, index.parse::<usize>().ok()?)));
        let resource = cel.map_or(bare.as_ref(), |(name, _)| name);
        // JSFL preserves spaces, hyphens and repeated underscores in names.
        let resource = resource.to_ascii_uppercase();
        let original_id = bitmap
            .descendants()
            .find(|node| {
                node.has_tag_name("PD")
                    && node.attribute("n") == Some("fla-reanim.resource-id")
                    && node.attribute("t") == Some("s")
            })
            .and_then(|node| node.attribute("v"))
            .filter(|id| !id.is_empty());
        let id = if let Some(original_id) = original_id {
            original_id.to_owned()
        } else if resource.starts_with("IMAGE_") {
            resource
        } else {
            format!("IMAGE_REANIM_{resource}")
        };
        let dat = bitmap
            .attribute("bitmapDataHRef")
            .and_then(|path| member(&format!("bin/{path}")));
        let data = if let Some(dat) = dat {
            crate::plus::bitmap_dat(dat)?
        } else {
            let href = bitmap.attribute("href").unwrap_or(&name);
            let png = member(href).or_else(|| member(&format!("LIBRARY/{href}")));
            let Some(png) = png else {
                continue;
            };
            let mut target = Cursor::new(Vec::new());
            crate::image_io::decode(png)?.write_to(&mut target, ImageFormat::Png)?;
            target.into_inner()
        };
        if let Some((_, index)) = cel {
            sprites
                .entry(id)
                .or_default()
                .push((index, crate::image_io::decode(&data)?));
            continue;
        }
        save_classic_asset(
            parent,
            &id,
            &crate::image_io::decode(&data)?,
            catalog.as_ref(),
            &mut mapping,
        )?;
    }
    let metadata: Value = members
        .get(SPRITES)
        .map(|v| serde_json::from_slice(v))
        .transpose()?
        .unwrap_or(Value::Null);
    for (id, cells) in sprites {
        let location = catalog.as_ref().and_then(|c| c.asset_location(&id));
        let canonical_id = location.as_ref().map_or(id.as_str(), |l| l.id.as_str());
        let meta = metadata.get(canonical_id).or_else(|| metadata.get(&id));
        let layout = catalog
            .as_ref()
            .and_then(|c| c.cells.get(&canonical_id.to_uppercase()));
        let width = cells[0].1.width();
        let height = cells[0].1.height();
        let columns = meta
            .and_then(|m| m["columns"].as_u64())
            .or_else(|| layout.map(|l| u64::from(l.0)))
            .unwrap_or_else(|| cells.iter().map(|(i, _)| *i as u64 + 1).max().unwrap_or(1))
            as u32;
        let rows = meta
            .and_then(|m| m["rows"].as_u64())
            .or_else(|| layout.map(|l| u64::from(l.1)))
            .unwrap_or(1) as u32;
        ensure!(
            columns > 0
                && rows > 0
                && columns <= 16384
                && rows <= 16384
                && u64::from(width) * u64::from(height) * u64::from(columns) * u64::from(rows)
                    <= 64 * 1024 * 1024,
            "Sprite allocation too large"
        );
        let mut sheet = if let Some(original) = meta
            .and_then(|m| m["member"].as_str())
            .and_then(|n| members.get(n))
        {
            crate::image_io::decode(original)?
        } else if let Some(source) = location.as_ref().and_then(|l| l.source.as_ref()) {
            load_image(source)?
        } else {
            ImageBuffer::from_pixel(width * columns, height * rows, image::Rgba([0, 0, 0, 0]))
        };
        for (index, cell) in cells {
            ensure!(
                index < (columns as usize) && cell.dimensions() == (width, height),
                "Inconsistent sprite cells"
            );
            image::imageops::replace(&mut sheet, &cell, index as i64 * i64::from(width), 0);
        }
        ensure!(
            sheet.dimensions() == (width * columns, height * rows),
            "Original sprite sheet dimensions differ"
        );
        save_classic_asset(parent, &id, &sheet, catalog.as_ref(), &mut mapping)?;
    }
    if !mapping.is_empty() {
        fs::write(
            output.with_extension("assets.json"),
            serde_json::to_vec_pretty(&mapping)?,
        )?;
    }
    Ok(mapping.len())
}

fn safe_asset_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.to_string_lossy().contains(':')
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

fn save_classic_asset(
    parent: &Path,
    id: &str,
    image: &RgbaImage,
    catalog: Option<&Catalog>,
    mapping: &mut BTreeMap<String, String>,
) -> Result<()> {
    let location = catalog.and_then(|c| c.asset_location(id));
    let stem = id
        .strip_prefix("IMAGE_REANIM_")
        .or_else(|| id.strip_prefix("IMAGE_"))
        .unwrap_or(id);
    ensure!(!stem.is_empty(), "Empty classic image ID {id}");
    let relative = location
        .as_ref()
        .map(|l| l.relative.clone())
        .unwrap_or_else(|| PathBuf::from(format!("reanim/{stem}.png")));
    ensure!(safe_asset_relative(&relative), "Unsafe asset destination");
    let path = parent.join(&relative);
    fs::create_dir_all(path.parent().context("invalid asset path")?)?;
    image.save(&path)?;
    if let Some(location) = &location
        && let Some(source) = &location.source
        && let Some(catalog) = catalog
        && let Some(mask) = independent_alpha_mask(source)
        && let Ok(mask_relative) = mask.strip_prefix(&catalog.roots[0])
    {
        let mask_relative = mask_relative.with_extension("png");
        ensure!(
            safe_asset_relative(&mask_relative),
            "Unsafe alpha destination"
        );
        let mask_path = parent.join(mask_relative);
        fs::create_dir_all(mask_path.parent().context("invalid alpha path")?)?;
        // The game replaces PNG alpha with this mask's blue channel. A new
        // same-stem PNG also takes precedence over the original JPEG mask.
        ImageBuffer::from_fn(image.width(), image.height(), |x, y| {
            let alpha = image.get_pixel(x, y).0[3];
            image::Rgba([alpha, alpha, alpha, 255])
        })
        .save(mask_path)?;
    }
    mapping.insert(id.to_owned(), relative.to_string_lossy().replace('\\', "/"));
    Ok(())
}

/// Encode straight RGBA as Animate's premultiplied ARGB DAT pixels.
/// Adobe-authored DATs use alpha first; treating them as RGBA swaps colours and
/// interprets blue as opacity. Chunk framing contains one continuous zlib stream.
pub fn bitmap_dat(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    ensure!(
        width > 0 && width <= 16383 && height > 0 && height <= 16383,
        "Invalid DAT image dimensions"
    );
    ensure!(
        rgba.len() == width as usize * height as usize * 4,
        "Invalid RGBA length"
    );
    let mut data = Vec::with_capacity(rgba.len() / 2);
    data.extend_from_slice(&[3, 5]);
    data.extend_from_slice(&((width * 4) as u16).to_le_bytes());
    data.extend_from_slice(&(width as u16).to_le_bytes());
    data.extend_from_slice(&(height as u16).to_le_bytes());
    for value in [0, width * 20, 0, height * 20] {
        data.extend_from_slice(&value.to_le_bytes());
    }
    data.extend_from_slice(&[1, 1]);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    let mut argb = Vec::with_capacity(rgba.len());
    for pixel in rgba.as_chunks::<4>().0.iter() {
        let a = u16::from(pixel[3]);
        argb.push(pixel[3]);
        for channel in &pixel[..3] {
            argb.push(((u16::from(*channel) * a + 127) / 255) as u8);
        }
    }
    encoder.write_all(&argb)?;
    let compressed = encoder.finish()?;
    for chunk in compressed.chunks(65535) {
        data.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        data.extend_from_slice(chunk);
    }
    data.extend_from_slice(&[0, 0]);
    Ok(data)
}

fn normalize(s: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
            separator = false;
        } else if !out.is_empty() && !separator {
            out.push('_');
            separator = true;
        }
    }
    out.trim_end_matches('_').to_owned()
}

struct AssetLocation {
    id: String,
    relative: PathBuf,
    source: Option<PathBuf>,
}

struct Catalog {
    roots: Vec<PathBuf>,
    paths: HashMap<String, PathBuf>,
    normalized_paths: HashMap<String, PathBuf>,
    manifest_paths: HashMap<String, PathBuf>,
    cells: HashMap<String, (u32, u32)>,
    destinations: HashMap<String, PathBuf>,
    resource_ids: HashMap<PathBuf, String>,
}
impl Catalog {
    fn new(input: &Path, explicit: Option<&Path>) -> Self {
        let mut roots = Vec::new();
        if let Some(root) = explicit {
            roots.push(root.to_owned());
        }
        for ancestor in input.parent().unwrap_or(Path::new(".")).ancestors().take(8) {
            if ancestor.join("Resources").is_dir() {
                roots.push(ancestor.join("Resources"));
                break;
            }
            if ancestor.join("resources.xml").is_file() {
                roots.push(ancestor.to_owned());
                break;
            }
        }
        let parent = input.parent().unwrap_or(Path::new("."));
        roots.push(parent.to_owned());
        for ancestor in parent.ancestors().take(4) {
            for folder in ["reanim", "images", "particles"] {
                let path = ancestor.join(folder);
                if path.is_dir() {
                    roots.push(path);
                }
            }
        }
        roots.dedup();
        let mut catalog = Self {
            roots,
            paths: HashMap::new(),
            normalized_paths: HashMap::new(),
            manifest_paths: HashMap::new(),
            cells: HashMap::new(),
            destinations: HashMap::new(),
            resource_ids: HashMap::new(),
        };
        for root in catalog.roots.clone() {
            let xml_path = root.join("resources.xml");
            if let Ok(text) = fs::read_to_string(xml_path)
                && let Ok(doc) = roxmltree::Document::parse(&text)
            {
                let mut prefix = "";
                let mut folder = "";
                for e in doc.descendants().filter(|n| n.is_element()) {
                    match e.tag_name().name() {
                        "SetDefaults" => {
                            prefix = e.attribute("idprefix").unwrap_or(prefix);
                            folder = e.attribute("path").unwrap_or(folder);
                        }
                        "Image" => {
                            if let Some(path) = e.attribute("path") {
                                let id = format!(
                                    "{}{}",
                                    prefix,
                                    e.attribute("id")
                                        .unwrap_or(path.rsplit(['/', '\\']).next().unwrap_or(path))
                                );
                                let stem = root
                                    .join(folder.replace('\\', "/"))
                                    .join(path.replace('\\', "/"));
                                let relative = PathBuf::from(folder.replace('\\', "/"))
                                    .join(path.replace('\\', "/"));
                                if safe_asset_relative(&relative) {
                                    catalog
                                        .destinations
                                        .entry(id.to_uppercase())
                                        .or_insert_with(|| relative.with_extension("png"));
                                }
                                let columns = e
                                    .attribute("cols")
                                    .and_then(|v| v.parse().ok())
                                    .unwrap_or(1);
                                let rows = e
                                    .attribute("rows")
                                    .and_then(|v| v.parse().ok())
                                    .unwrap_or(1);
                                catalog
                                    .cells
                                    .entry(id.to_uppercase())
                                    .or_insert((columns, rows));
                                if let Some(file) = find_image(&stem) {
                                    catalog
                                        .manifest_paths
                                        .entry(id.to_uppercase())
                                        .or_insert_with(|| file.clone());
                                    catalog
                                        .resource_ids
                                        .entry(file.clone())
                                        .or_insert_with(|| id.clone());
                                    catalog.paths.entry(id.to_uppercase()).or_insert(file);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            let folders: Vec<_> = if root.join("resources.xml").exists() {
                ["reanim", "images", "particles"]
                    .into_iter()
                    .map(|s| root.join(s))
                    .collect()
            } else {
                vec![root]
            };
            for folder in folders {
                for e in walkdir::WalkDir::new(&folder)
                    .max_depth(8)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                {
                    let p = e.path();
                    if !is_image(p) {
                        continue;
                    }
                    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
                    for id in [
                        stem.to_uppercase(),
                        format!("IMAGE_REANIM_{}", stem.to_uppercase()),
                        format!("IMAGE_{}", stem.to_uppercase()),
                    ] {
                        catalog.paths.entry(id).or_insert_with(|| p.to_owned());
                    }
                    for id in [
                        normalize(&stem),
                        format!("IMAGE_REANIM_{}", normalize(&stem)),
                        format!("IMAGE_{}", normalize(&stem)),
                    ] {
                        catalog
                            .normalized_paths
                            .entry(id)
                            .or_insert_with(|| p.to_owned());
                    }
                }
            }
        }
        // DefinitionLoadImage checks IMAGE_<suffix> in ResourceManager before
        // scanning reanim/<suffix>. Mirror its sprite layout for that alias.
        for id in catalog.manifest_paths.keys() {
            if let Some(suffix) = id.strip_prefix("IMAGE_") {
                let alias = format!("IMAGE_REANIM_{suffix}");
                if !catalog.manifest_paths.contains_key(&alias)
                    && let Some(layout) = catalog.cells.get(id).copied()
                {
                    catalog.cells.insert(alias, layout);
                }
            }
        }
        catalog
    }
    fn resolve(&self, id: &str) -> Option<PathBuf> {
        let key = id.to_uppercase();
        let manifest = self.manifest_paths.get(&key).or_else(|| {
            key.strip_prefix("IMAGE_REANIM_")
                .and_then(|suffix| self.manifest_paths.get(&format!("IMAGE_{suffix}")))
        });
        if let Some(path) = manifest {
            return Some(path.clone());
        }
        // DefinitionLoadImage tries these directories and substring offsets
        // in order. Resolve literal filenames before any normalized alias:
        // "Zamboni6" and "Zamboni6 " are distinct legal source stems.
        for root in &self.roots {
            for (offset, folder) in [(6, ""), (6, "particles"), (13, "reanim"), (13, "images")] {
                let Some(suffix) = id.get(offset..).filter(|suffix| !suffix.is_empty()) else {
                    continue;
                };
                let relative = Path::new(folder).join(suffix.replace('\\', "/"));
                if safe_asset_relative(&relative)
                    && let Some(path) = find_image(&root.join(relative))
                {
                    return Some(path);
                }
            }
        }
        self.paths
            .get(&key)
            .or_else(|| self.normalized_paths.get(&normalize(id)))
            .cloned()
    }

    fn asset_location(&self, id: &str) -> Option<AssetLocation> {
        let source = self.resolve(id);
        let id = source
            .as_ref()
            .and_then(|path| self.resource_ids.get(path))
            .cloned()
            .unwrap_or_else(|| id.to_owned());
        let relative = self
            .destinations
            .get(&id.to_uppercase())
            .cloned()
            .or_else(|| {
                source
                    .as_ref()?
                    .strip_prefix(self.roots.first()?)
                    .ok()
                    .map(|path| path.with_extension("png"))
            })?;
        let relative = if let Some(source) = &source {
            let stem = source.file_stem()?.to_string_lossy();
            if (stem.starts_with('_') || stem.ends_with('_'))
                && find_image(&source.with_file_name(stem.trim_matches('_'))).is_none()
            {
                relative.with_file_name(format!("{}.png", stem.trim_matches('_')))
            } else {
                relative
            }
        } else {
            relative
        };
        safe_asset_relative(&relative).then_some(AssetLocation {
            id,
            relative,
            source,
        })
    }
}
fn is_image(path: &Path) -> bool {
    path.extension().is_some_and(|s| {
        matches!(
            s.to_string_lossy().to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg"
        )
    })
}
fn find_image(path: &Path) -> Option<PathBuf> {
    if path.is_file() && is_image(path) {
        return Some(path.to_owned());
    }
    for ext in ["png", "jpg", "jpeg"] {
        let file = path.with_extension(ext);
        if file.is_file() {
            return Some(file);
        }
    }
    // Windows is case insensitive; on other hosts fall back to a directory lookup.
    let stem = path.file_stem()?.to_string_lossy();
    fs::read_dir(path.parent()?)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            is_image(p)
                && p.file_stem()
                    .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(&stem))
        })
}

fn independent_alpha_mask(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy();
    find_image(&path.with_file_name(format!("_{stem}")))
        .or_else(|| find_image(&path.with_file_name(format!("{stem}_"))))
        .or_else(|| {
            ((stem.starts_with('_') || stem.ends_with('_'))
                && find_image(&path.with_file_name(stem.trim_matches('_'))).is_none())
            .then(|| path.to_owned())
        })
}

fn load_image(path: &Path) -> Result<RgbaImage> {
    let base = find_image(path);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let alpha = find_image(&path.with_file_name(format!("_{stem}")))
        .or_else(|| find_image(&path.with_file_name(format!("{stem}_"))));
    let mut image = if let Some(ref base) = base {
        crate::image_io::open(base)?
    } else if let Some(ref alpha) = alpha {
        let a = crate::image_io::open(alpha)?;
        ImageBuffer::from_pixel(a.width(), a.height(), image::Rgba([255, 255, 255, 255]))
    } else {
        bail!("Missing image {}", path.display())
    };
    if alpha.is_none() && (stem.starts_with('_') || stem.ends_with('_')) {
        // The engine also permits a mask without an RGB companion: the colour
        // defaults to white, and the mask's blue channel supplies opacity.
        let plain = stem.trim_matches('_');
        if find_image(&path.with_file_name(plain)).is_none() {
            for pixel in image.pixels_mut() {
                let a = pixel.0[2];
                pixel.0 = [255, 255, 255, a];
            }
        }
    }
    if let Some(alpha) = alpha {
        let mask = crate::image_io::open(&alpha)?;
        ensure!(
            mask.dimensions() == image.dimensions(),
            "Alpha mask dimensions differ: {}",
            path.display()
        );
        for (pixel, mask) in image.pixels_mut().zip(mask.pixels()) {
            pixel.0[3] = mask.0[2];
        }
    }
    Ok(image)
}

#[derive(Clone)]
struct Cell {
    library: String,
    width: u32,
    height: u32,
    href: String,
    dat: String,
}
#[derive(Clone)]
struct Art {
    symbol: String,
    cells: Vec<Cell>,
    original: Option<(String, (u32, u32))>,
}
fn add_art(
    entries: &mut BTreeMap<String, Vec<u8>>,
    arts: &mut BTreeMap<String, Art>,
    id: &str,
    path: Option<&Path>,
    layout: (u32, u32),
    link: bool,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if arts.contains_key(id) {
        return Ok(());
    }
    let index = arts.len();
    let image = if let Some(path) = path {
        match load_image(path) {
            Ok(img) => img,
            Err(e) => {
                warnings.push(format!("贴图 {id} 无法读取：{e}；使用透明占位图"));
                ImageBuffer::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]))
            }
        }
    } else {
        warnings.push(format!("找不到贴图 {id}；使用透明占位图，原始ID仍保留"));
        ImageBuffer::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]))
    };
    let columns = layout.0.max(1).min(image.width());
    let rows = layout.1.max(1).min(image.height());
    ensure!(columns <= 16384, "Sprite has too many columns");
    let width = image.width() / columns;
    let height = image.height() / rows;
    // Stock attacher__ layers interpret any parent symbol containing "__" as
    // an attachment command, so ordinary bitmap wrappers must not contain it.
    let symbol = format!("ReanimBitmap_{index}");
    let original = if columns > 1 || rows > 1 {
        let member = format!("META-INF/reanim-sprites/{index:04}.png");
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, ImageFormat::Png)?;
        entries.insert(member.clone(), png.into_inner());
        Some((member, (columns, rows)))
    } else {
        None
    };
    let mut cells = Vec::new();
    let mut frames = String::new();
    for column in 0..columns {
        // Reanimation::DrawTrack samples only the first row and wraps f by cols.
        let cel = image::imageops::crop_imm(&image, column * width, 0, width, height).to_image();
        // The stock JSFL exporter adds IMAGE_REANIM_ itself. A plain bitmap
        // must therefore use the original asset basename in the FLA library.
        let basename = id.strip_prefix("IMAGE_REANIM_").unwrap_or(id);
        let library = if columns > 1 || rows > 1 {
            format!("{id}__REANIM_CEL_{column}.png")
        } else {
            format!("{basename}.png")
        };
        let href = format!("{index:04}_{column:04}.png");
        let dat = format!("M {} 1.dat", index * 16384 + column as usize + 1);
        if link {
            let mut png = Cursor::new(Vec::new());
            cel.write_to(&mut png, ImageFormat::Png)?;
            entries.insert(format!("LIBRARY/{href}"), png.into_inner());
        } else {
            entries.insert(
                format!("bin/{dat}"),
                bitmap_dat(cel.as_raw(), width, height)?,
            );
        }
        frames.push_str(&format!("<DOMFrame index=\"{column}\" duration=\"1\" keyMode=\"9728\"><elements><DOMBitmapInstance libraryItemName=\"{}\"/></elements></DOMFrame>",escape(&library)));
        cells.push(Cell {
            library,
            width,
            height,
            href,
            dat,
        });
    }
    let symbol_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><DOMSymbolItem xmlns=\"{NS}\" name=\"{symbol}\" symbolType=\"graphic\"><timeline><DOMTimeline name=\"{symbol}\"><layers><DOMLayer name=\"bitmap\"><frames>{frames}</frames></DOMLayer></layers></DOMTimeline></timeline></DOMSymbolItem>"
    );
    entries.insert(format!("LIBRARY/{symbol}.xml"), symbol_xml.into_bytes());
    arts.insert(
        id.to_owned(),
        Art {
            symbol,
            cells,
            original,
        },
    );
    Ok(())
}

fn matrix(t: &Transform) -> [f64; 6] {
    let v = t.values;
    let kx = f64::from(v[2]).to_radians();
    let ky = f64::from(v[3]).to_radians();
    [
        f64::from(v[4]) * kx.cos(),
        f64::from(v[4]) * kx.sin(),
        -f64::from(v[5]) * ky.sin(),
        f64::from(v[5]) * ky.cos(),
        f64::from(v[0]),
        f64::from(v[1]),
    ]
}
fn matrix_xml(m: [f64; 6]) -> String {
    format!(
        "<matrix><Matrix a=\"{}\" b=\"{}\" c=\"{}\" d=\"{}\" tx=\"{}\" ty=\"{}\"/></matrix>",
        m[0], m[1], m[2], m[3], m[4], m[5]
    )
}

fn frame_xml(
    index: usize,
    duration: usize,
    t: &Transform,
    art: Option<&Art>,
    command: Option<&str>,
    tween: bool,
    easing: i32,
) -> String {
    let timing = if tween {
        format!(
            " keyMode=\"24065\" tweenType=\"motion\" acceleration=\"{}\" motionTweenSync=\"true\" motionTweenSnap=\"true\" motionTweenScale=\"true\" motionTweenOrientToPath=\"false\"",
            -easing
        )
    } else {
        " keyMode=\"9728\"".to_owned()
    };
    let mut out = format!("<DOMFrame index=\"{index}\" duration=\"{duration}\"{timing}><elements>");
    // anim_ ranges are ordinary layer visibility, not frame labels. An active
    // image-less track contains a real empty symbol instance, just like GOTY.
    if t.values[6] >= 0. {
        let symbol = command.unwrap_or_else(|| art.map(|a| a.symbol.as_str()).unwrap_or("locator"));
        let cel = art
            .map(|a| ((t.values[6] + 0.5).floor() as usize) % a.cells.len())
            .unwrap_or(0);
        let first = if cel > 0 {
            format!(" firstFrame=\"{cel}\"")
        } else {
            String::new()
        };
        out.push_str(&format!("<DOMSymbolInstance libraryItemName=\"{}\" symbolType=\"graphic\" loop=\"single frame\"{first}>{}<transformationPoint><Point x=\"0\" y=\"0\"/></transformationPoint><color><Color alphaMultiplier=\"{}\"/></color></DOMSymbolInstance>",escape(symbol),matrix_xml(matrix(t)),t.values[7]));
    }
    out.push_str("</elements></DOMFrame>");
    out
}

pub fn write_xml(definition: &Definition) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Definition>\n");
    for track in &definition.tracks {
        out.push_str(&format!("<track><name>{}</name>\n", escape(&track.name)));
        for t in &track.transforms {
            out.push_str("<t>");
            for (name, value) in ["x", "y", "kx", "ky", "sx", "sy", "f", "a"]
                .iter()
                .zip(t.values)
            {
                if value != MISSING {
                    out.push_str(&format!("<{name}>{value}</{name}>"));
                }
            }
            for (name, value) in [("i", &t.image), ("font", &t.font), ("text", &t.text)] {
                if !value.is_empty() {
                    out.push_str(&format!("<{name}>{}</{name}>", escape(value)));
                }
            }
            out.push_str("</t>\n");
        }
        out.push_str("</track>\n");
    }
    out.push_str(&format!("<fps>{}</fps>\n</Definition>\n", definition.fps));
    out
}

fn locator_xml() -> Vec<u8> {
    empty_symbol_xml("locator")
}
fn empty_symbol_xml(name: &str) -> Vec<u8> {
    let name = escape(name);
    format!("<DOMSymbolItem xmlns=\"{NS}\" name=\"{name}\" symbolType=\"graphic\"><timeline><DOMTimeline name=\"{name}\"><layers><DOMLayer name=\"locator\"><frames><DOMFrame index=\"0\" duration=\"1\" keyMode=\"9728\"><elements/></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timeline></DOMSymbolItem>").into_bytes()
}
fn text_symbol_xml(name: &str, font: &str, text: &str) -> Vec<u8> {
    let font = escape(font.strip_prefix("FONT_").unwrap_or(font));
    let text = escape(text);
    // The outer graphic instance carries the alpha and transform. Static text
    // itself has no colorAlphaPercent in the original JSFL exporter.
    format!("<DOMSymbolItem xmlns=\"{NS}\" name=\"{name}\" symbolType=\"graphic\"><timeline><DOMTimeline name=\"{name}\"><layers><DOMLayer name=\"text\"><frames><DOMFrame index=\"0\" duration=\"1\" keyMode=\"9728\"><elements><DOMStaticText autoExpand=\"true\" width=\"0\" height=\"0\" isSelectable=\"false\"><textRuns><DOMTextRun><characters>{text}</characters><textAttrs><DOMTextAttrs face=\"Arial\" size=\"40\" url=\"{font}\"/></textAttrs></DOMTextRun></textRuns></DOMStaticText></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timeline></DOMSymbolItem>").into_bytes()
}

/// Write a self-contained FLA and lossless `.reanim.xml` beside it.
pub fn convert(
    input: &Path,
    output: &Path,
    resource_root: Option<&Path>,
    link: bool,
) -> Result<Report> {
    convert_with_tolerance(
        input,
        output,
        resource_root,
        link,
        crate::tween::Tolerance::editable(),
    )
}

pub fn convert_with_tolerance(
    input: &Path,
    output: &Path,
    resource_root: Option<&Path>,
    link: bool,
    tolerance: crate::tween::Tolerance,
) -> Result<Report> {
    convert_with_stage(input, output, resource_root, link, tolerance, None)
}

/// Stage dimensions are authoring metadata absent from the compiled DefMap.
/// Stock documents use dimensions measured from the corresponding GOTY FLA;
/// custom documents can explicitly supply their authoring canvas size.
pub fn convert_with_stage(
    input: &Path,
    output: &Path,
    resource_root: Option<&Path>,
    link: bool,
    tolerance: crate::tween::Tolerance,
    stage: Option<[u32; 2]>,
) -> Result<Report> {
    ensure!(
        !link,
        "--link 尚不能生成已验证可供 Adobe Animate 使用的链接位图；请使用默认的内嵌 DAT 模式"
    );
    let original = fs::read(input)?;
    let raw = crate::codec::decode(&original)?;
    let mut definition = raw.expanded();
    let mut warnings = Vec::new();
    let stage = if let Some(stage) = stage {
        ensure!(
            stage.iter().all(|v| (1..=16384).contains(v)),
            "舞台尺寸必须在 1–16384 像素内"
        );
        stage
    } else {
        crate::stage::recorded(input).or_else(|| crate::stage::stock(input)).unwrap_or_else(|| {
            warnings.push("compiled 不保存舞台尺寸；此名称没有原版 FLA 尺寸记录，使用 800×600。可用 --stage WIDTHxHEIGHT 指定；游戏坐标原点保持 (0,0)。".into());
            [800, 600]
        })
    };
    let graph = crate::plus::read_graph(&original)?;
    let mut graph_paths = HashMap::new();
    if let Some(graph) = &graph {
        let (baked, paths) = bake_graph(graph, input)?;
        definition = baked;
        graph_paths = paths;
        warnings.push("增强动画已按逐帧世界矩阵烘焙为可编辑图层；原始骨骼、枢轴和JSON完整保存在FLA元数据中，未编辑回编可精确恢复。".to_owned());
    }
    let catalog = Catalog::new(input, resource_root);
    let mut entries = BTreeMap::new();
    let mut arts = BTreeMap::new();
    let mut commands = BTreeMap::new();
    let mut texts = BTreeMap::new();
    for track in &definition.tracks {
        for t in &track.transforms {
            if !t.font.is_empty() && !texts.contains_key(&(t.font.clone(), t.text.clone())) {
                let symbol = format!("__reanim_text_{}", texts.len());
                entries.insert(
                    format!("LIBRARY/{symbol}.xml"),
                    text_symbol_xml(&symbol, &t.font, &t.text),
                );
                texts.insert((t.font.clone(), t.text.clone()), symbol);
            }
            if track.name.starts_with("attacher__")
                && t.font.is_empty()
                && !t.text.is_empty()
                && t.text != "_"
                && !commands.contains_key(&t.text)
            {
                let file = format!("__reanim_command_{}.xml", commands.len());
                entries.insert(format!("LIBRARY/{file}"), empty_symbol_xml(&t.text));
                commands.insert(t.text.clone(), file);
            }
            if !t.image.is_empty() && !arts.contains_key(&t.image) {
                let path = graph_paths
                    .get(&t.image)
                    .cloned()
                    .or_else(|| catalog.resolve(&t.image));
                add_art(
                    &mut entries,
                    &mut arts,
                    &t.image,
                    path.as_deref(),
                    catalog
                        .cells
                        .get(&t.image.to_uppercase())
                        .copied()
                        .unwrap_or((1, 1)),
                    link,
                    &mut warnings,
                )?;
            }
        }
    }
    entries.insert("LIBRARY/locator.xml".to_owned(), locator_xml());
    let mut document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DOMDocument xmlns=\"{NS}\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" width=\"{}\" height=\"{}\" frameRate=\"{}\" currentTimeline=\"1\" xflVersion=\"2.97\"><media>",
        stage[0], stage[1], definition.fps
    );
    for (index, (id, cell)) in arts
        .iter()
        .flat_map(|(id, art)| art.cells.iter().map(move |cell| (id, cell)))
        .enumerate()
    {
        let data = if link {
            String::new()
        } else {
            format!(" bitmapDataHRef=\"{}\"", escape(&cell.dat))
        };
        document.push_str(&format!("<DOMBitmapItem name=\"{}\" itemID=\"00000001-{:08x}\" href=\"{}\" sourceLastImported=\"0\" allowSmoothing=\"true\" compressionType=\"lossless\" originalCompressionType=\"lossless\" quality=\"50\" frameRight=\"{}\" frameBottom=\"{}\"{data}><persistentData><PD n=\"fla-reanim.resource-id\" t=\"s\" v=\"{}\"/></persistentData></DOMBitmapItem>",escape(&cell.library),index+1,escape(&cell.href),cell.width*20,cell.height*20,escape(id)));
    }
    document.push_str("</media><symbols><Include href=\"locator.xml\"/>");
    for art in arts.values() {
        document.push_str(&format!("<Include href=\"{}.xml\"/>", escape(&art.symbol)));
    }
    for file in commands.values() {
        document.push_str(&format!("<Include href=\"{}\"/>", escape(file)));
    }
    for symbol in texts.values() {
        document.push_str(&format!("<Include href=\"{symbol}.xml\"/>"));
    }
    document.push_str("</symbols><timelines><DOMTimeline name=\"Scene 1\"><layers>");
    let (mut keyframes, mut motion_tweens, mut multi_frame_tweens) = (0, 0, 0);
    let source_samples = definition
        .tracks
        .iter()
        .map(|track| track.transforms.len())
        .sum();
    for track in definition.tracks.iter().rev() {
        document.push_str(&format!(
            "<DOMLayer name=\"{}\" color=\"#4F81BD\"><frames>",
            escape(&track.name)
        ));
        let plan = crate::tween::plan(&track.transforms, tolerance);
        if !plan.unfitted_intervals.is_empty() {
            warnings.push(format!(
                "轨道 {} 有 {} 处不可用标准补间表示的跳变，保留必要关键帧",
                track.name,
                plan.unfitted_intervals.len()
            ));
        }
        for span in plan.spans {
            let t = &track.transforms[span.start];
            let tween = span.tween;
            keyframes += 1;
            motion_tweens += usize::from(tween);
            multi_frame_tweens += usize::from(tween && span.duration > 1);
            document.push_str(&frame_xml(
                span.start,
                span.duration,
                t,
                arts.get(&t.image),
                commands
                    .get_key_value(&t.text)
                    .map(|(command, _)| command.as_str())
                    .or_else(|| {
                        texts
                            .get(&(t.font.clone(), t.text.clone()))
                            .map(String::as_str)
                    }),
                tween,
                span.easing,
            ));
        }
        document.push_str("</frames></DOMLayer>");
    }
    document.push_str("</layers></DOMTimeline></timelines>");
    let compiled_helpers: Vec<_> = definition
        .tracks
        .iter()
        .filter(|t| t.name.starts_with('_') && t.name != "_ground")
        .map(|t| t.name.as_str())
        .collect();
    if !compiled_helpers.is_empty() {
        document.push_str(&format!("<persistentData><PD n=\"fla-reanim.compiled-helper-tracks\" t=\"s\" v=\"{}\"/></persistentData>", escape(&serde_json::to_string(&compiled_helpers)?)));
    }
    document.push_str("<PrinterSettings/><publishHistory/></DOMDocument>");
    entries.insert("DOMDocument.xml".to_owned(), document.into_bytes());
    entries.insert("mimetype".to_owned(), b"application/vnd.adobe.xfl".to_vec());
    let sprites: serde_json::Map<String, Value> = arts
        .iter()
        .filter_map(|(id, art)| {
            let (member, (columns, rows)) = art.original.as_ref()?;
            Some((
                id.to_owned(),
                serde_json::json!({"columns":columns,"rows":rows,"member":member}),
            ))
        })
        .collect();
    if !sprites.is_empty() {
        entries.insert(SPRITES.to_owned(), serde_json::to_vec(&sprites)?);
    }
    if let Some(graph) = &graph {
        let mut mapping = BTreeMap::new();
        for (index, name) in graph["images"]
            .as_array()
            .context("RPLS images missing")?
            .iter()
            .enumerate()
        {
            let relative = name.as_str().context("RPLS image path is not a string")?;
            let path = Path::new(relative);
            ensure!(
                !relative.contains('\\')
                    && !relative.contains(':')
                    && !relative.starts_with('/')
                    && path
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))),
                "Unsafe RPLS asset path: {relative}"
            );
            let member = format!("META-INF/reanim-assets/{index:04}.bin");
            let source = input.parent().unwrap_or(Path::new(".")).join(path);
            let data = fs::read(&source)
                .with_context(|| format!("RPLS external image missing: {}", source.display()))?;
            entries.insert(member.clone(), data);
            mapping.insert(relative.to_owned(), member);
        }
        entries.insert(ASSETS.to_owned(), serde_json::to_vec(&mapping)?);
    }
    entries.insert(HASH.to_owned(), fingerprint(&entries).into_bytes());
    entries.insert(ORIGINAL.to_owned(), original);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(output).with_context(|| format!("Create {}", output.display()))?;
    let mut zip = ZipWriter::new(file);
    // Mimetype is stored uncompressed for interoperability; order itself is not
    // required by Animate (Adobe's own documents frequently put it last).
    zip.start_file(
        "mimetype",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )?;
    zip.write_all(entries.get("mimetype").unwrap())?;
    for (name, data) in &entries {
        if name == "mimetype" {
            continue;
        }
        zip.start_file(
            name,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )?;
        zip.write_all(data)?;
    }
    zip.finish()?;
    fs::write(output.with_extension("reanim.xml"), write_xml(&raw))?;
    if let Some(graph) = graph {
        fs::write(
            output.with_extension("reanim-plus.json"),
            serde_json::to_vec_pretty(&graph)?,
        )?;
        warnings.push(
            "增强文件的XML侧车仅包含经典核心；完整层级与枢轴另存于.reanim-plus.json。".to_owned(),
        );
    }
    if link {
        warnings.push(
            "链接模式将PNG保存在FLA的LIBRARY目录中，未生成bin DAT；文件仍可移动。".to_owned(),
        );
    }
    Ok(Report {
        stage,
        tween_tolerance: tolerance,
        source_samples,
        keyframes,
        motion_tweens,
        multi_frame_tweens,
        tracks: definition.tracks.len(),
        frames: definition.frame_count(),
        bitmaps: arts.len(),
        warnings,
    })
}

type Matrix = [f64; 6];
fn multiply(a: Matrix, b: Matrix) -> Matrix {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}
#[derive(Clone)]
struct Pose {
    matrix: Matrix,
    alpha: f64,
    visible: bool,
    image: i64,
}
fn bake_graph(graph: &Value, input: &Path) -> Result<(Definition, HashMap<String, PathBuf>)> {
    let fps = graph["fps"].as_f64().context("RPLS fps missing")?;
    ensure!(fps.is_finite() && fps > 0., "Invalid RPLS fps");
    let nodes = graph["nodes"].as_array().context("RPLS nodes missing")?;
    let images = graph["images"].as_array().context("RPLS images missing")?;
    let end = graph["clips"]
        .as_object()
        .context("RPLS clips missing")?
        .values()
        .filter_map(|c| c["end"].as_f64())
        .fold(0., f64::max);
    let frames = (end * fps).round().max(1.) as usize;
    ensure!(frames <= 20000, "RPLS exceeds frame limit");
    let mut paths = HashMap::new();
    for (index, image) in images.iter().enumerate() {
        if let Some(path) = image.as_str() {
            paths.insert(
                format!("RPLS_IMAGE_{index}"),
                input.parent().unwrap_or(Path::new(".")).join(path),
            );
        }
    }
    let indices: HashMap<_, _> = nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| Some((n["name"].as_str()?.to_owned(), i)))
        .collect();
    let mut definition = Definition {
        fps: fps as f32,
        tracks: nodes
            .iter()
            .map(|n| crate::Track {
                name: n["name"].as_str().unwrap_or("node").to_owned(),
                transforms: Vec::with_capacity(frames),
            })
            .collect(),
    };
    for frame in 0..frames {
        let local: Vec<_> = nodes
            .iter()
            .map(|node| {
                let key = node["keys"].as_array().and_then(|keys| {
                    keys.iter()
                        .rev()
                        .find(|k| k["time"].as_f64().unwrap_or(0.) <= frame as f64 / fps)
                });
                let mut m = [1., 0., 0., 1., 0., 0.];
                if let Some(a) = key.and_then(|k| k["matrix"].as_array()) {
                    for (i, v) in a.iter().take(6).enumerate() {
                        m[i] = v.as_f64().unwrap_or(m[i]);
                    }
                }
                Pose {
                    matrix: m,
                    alpha: key.and_then(|k| k["alpha"].as_f64()).unwrap_or(1.),
                    visible: key.is_some_and(|k| k["visible"].as_bool().unwrap_or(true)),
                    image: key.and_then(|k| k["image"].as_i64()).unwrap_or(-1),
                }
            })
            .collect();
        let mut cache = vec![None; nodes.len()];
        fn world(
            i: usize,
            nodes: &[Value],
            indices: &HashMap<String, usize>,
            local: &[Pose],
            cache: &mut [Option<Pose>],
            visiting: &mut HashSet<usize>,
        ) -> Result<Pose> {
            if let Some(p) = &cache[i] {
                return Ok(p.clone());
            }
            ensure!(visiting.insert(i), "Cyclic RPLS parents");
            let mut p = local[i].clone();
            if let Some(parent) = nodes[i]["parent"].as_str().and_then(|n| indices.get(n)) {
                let a = world(*parent, nodes, indices, local, cache, visiting)?;
                p.matrix = multiply(a.matrix, p.matrix);
                p.alpha *= a.alpha;
                p.visible &= a.visible;
            }
            visiting.remove(&i);
            cache[i] = Some(p.clone());
            Ok(p)
        }
        for i in 0..nodes.len() {
            let p = world(i, nodes, &indices, &local, &mut cache, &mut HashSet::new())?;
            let m = p.matrix;
            ensure!(
                m.iter().all(|v| v.is_finite()) && p.alpha.is_finite(),
                "Nonfinite RPLS pose"
            );
            let values = [
                m[4] as f32,
                m[5] as f32,
                m[1].atan2(m[0]).to_degrees() as f32,
                (-m[2]).atan2(m[3]).to_degrees() as f32,
                m[0].hypot(m[1]) as f32,
                m[2].hypot(m[3]) as f32,
                if p.visible { 0. } else { -1. },
                p.alpha as f32,
            ];
            definition.tracks[i].transforms.push(Transform {
                values,
                image: if p.image >= 0 {
                    format!("RPLS_IMAGE_{}", p.image)
                } else {
                    String::new()
                },
                font: String::new(),
                text: String::new(),
            });
        }
    }
    Ok((definition, paths))
}
