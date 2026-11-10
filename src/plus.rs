//! Native implementation of the Reanim Plus authoring pipeline.
//!
//! The JSON extension is the version-one format consumed by ReanimPlusAnimation;
//! image paths are relative to the compiled file. No authoring interpreter is used.
use crate::{Definition, Track, Transform};
use anyhow::{Context, Result, bail, ensure};
use flate2::{Compression, Decompress, FlushDecompress, write::ZlibEncoder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

type Matrix = [f64; 6];
const IDENTITY: Matrix = [1., 0., 0., 1., 0., 0.];
const MAX_DECODED: usize = 512 * 1024 * 1024;

#[derive(Clone, Debug)]
struct Element {
    name: String,
    attrs: BTreeMap<String, String>,
    children: Vec<Element>,
}
impl Element {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes)?.trim_start_matches('\u{feff}');
        let doc = roxmltree::Document::parse(text)?;
        fn own(n: roxmltree::Node<'_, '_>) -> Element {
            Element {
                name: n.tag_name().name().into(),
                attrs: n
                    .attributes()
                    .map(|a| (a.name().into(), a.value().into()))
                    .collect(),
                children: n.children().filter(|n| n.is_element()).map(own).collect(),
            }
        }
        Ok(own(doc.root_element()))
    }
    fn attr(&self, name: &str) -> &str {
        self.attrs.get(name).map(String::as_str).unwrap_or("")
    }
    fn default<'a>(&'a self, name: &str, default: &'a str) -> &'a str {
        self.attrs.get(name).map(String::as_str).unwrap_or(default)
    }
    fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|n| n.name == name)
    }
    fn path(&self, path: &str) -> Option<&Element> {
        path.split('/').try_fold(self, |n, part| n.child(part))
    }
    fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |n| n.name == name)
    }
    fn descendants<'a>(&'a self, name: &str, output: &mut Vec<&'a Element>) {
        for child in &self.children {
            if child.name == name {
                output.push(child);
            }
            child.descendants(name, output);
        }
    }
    fn num(&self, name: &str, default: f64) -> Result<f64> {
        let value = self
            .attrs
            .get(name)
            .map(|v| v.parse::<f64>())
            .transpose()?
            .unwrap_or(default);
        ensure!(value.is_finite(), "Non-finite XFL {name}");
        Ok(value)
    }
    fn integer(&self, name: &str, default: usize) -> Result<usize> {
        Ok(self
            .attrs
            .get(name)
            .map(|v| v.parse::<usize>())
            .transpose()?
            .unwrap_or(default))
    }
}

fn matrix(e: &Element) -> Result<Matrix> {
    let Some(m) = e.path("matrix/Matrix") else {
        return Ok(IDENTITY);
    };
    matrix_attrs(m)
}
fn matrix_attrs(m: &Element) -> Result<Matrix> {
    Ok([
        m.num("a", 1.)?,
        m.num("b", 0.)?,
        m.num("c", 0.)?,
        m.num("d", 1.)?,
        m.num("tx", 0.)?,
        m.num("ty", 0.)?,
    ])
}
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
fn inverse(m: Matrix) -> Result<Matrix> {
    let det = m[0] * m[3] - m[1] * m[2];
    ensure!(det.abs() >= 1e-10, "Singular FLA bone parent transform");
    Ok([
        m[3] / det,
        -m[1] / det,
        -m[2] / det,
        m[0] / det,
        (m[2] * m[5] - m[3] * m[4]) / det,
        (m[1] * m[4] - m[0] * m[5]) / det,
    ])
}
fn duration(t: &Element) -> Result<usize> {
    let mut end = 1;
    if let Some(layers) = t.child("layers") {
        for layer in layers.children_named("DOMLayer") {
            if let Some(frames) = layer.child("frames") {
                for f in frames.children_named("DOMFrame") {
                    let frame_end = f
                        .integer("index", 0)?
                        .checked_add(f.integer("duration", 1)?)
                        .context("XFL frame overflow")?;
                    end = end.max(frame_end);
                }
            }
        }
    }
    Ok(end)
}
fn safe_member(name: &str) -> Result<String> {
    let name = name.replace('\\', "/");
    ensure!(
        !name.starts_with('/')
            && !name.contains(':')
            && !Path::new(&name).components().any(|p| matches!(
                p,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )),
        "Invalid FLA member path: {name}"
    );
    Ok(name)
}
fn rgba_png(width: u32, height: u32, data: &[u8]) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut result, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(data)?;
    }
    Ok(result)
}

/// Decode Animate's raw or chunked, single-zlib-stream ARGB bitmap data.
/// The final header byte selects compression; raw pixels have no chunk framing.
/// `Z_SYNC_FLUSH` terminated streams are accepted because Animate writes those.
pub fn bitmap_dat(data: &[u8]) -> Result<Vec<u8>> {
    if data.starts_with(b"\x89PNG") || data.starts_with(b"\xff\xd8") {
        return Ok(data.to_vec());
    }
    ensure!(
        data.len() >= 26 && data[..2] == [3, 5],
        "Unsupported Animate bitmap DAT encoding; export linked PNG assets"
    );
    let u16_at = |i| u16::from_le_bytes([data[i], data[i + 1]]) as usize;
    let (stride, width, height) = (u16_at(2), u16_at(4), u16_at(6));
    ensure!(
        width > 0 && height > 0 && width <= 16384 && height <= 16384 && stride >= width * 4,
        "Invalid Animate bitmap dimensions"
    );
    let size = stride
        .checked_mul(height)
        .context("Bitmap allocation overflow")?;
    ensure!(size <= MAX_DECODED, "Animate bitmap exceeds decode limit");
    let pixels = match data[25] {
        0 => {
            ensure!(
                data.len() - 26 == size,
                "Invalid uncompressed Animate bitmap size"
            );
            data[26..].to_vec()
        }
        1 => {
            let (mut position, mut compressed) = (26, Vec::new());
            loop {
                ensure!(
                    position + 2 <= data.len(),
                    "Missing Animate bitmap chunk terminator"
                );
                let length = u16_at(position);
                position += 2;
                if length == 0 {
                    ensure!(position == data.len(), "Trailing Animate bitmap chunk data");
                    break;
                }
                ensure!(
                    position + length <= data.len(),
                    "Truncated Animate bitmap chunk"
                );
                compressed.extend_from_slice(&data[position..position + length]);
                position += length;
            }
            let mut pixels = vec![0; size + 1];
            let mut decoder = Decompress::new(true);
            decoder
                .decompress(&compressed, &mut pixels, FlushDecompress::Sync)
                .context("Invalid Animate bitmap zlib stream")?;
            ensure!(
                decoder.total_out() as usize == size
                    && decoder.total_in() as usize == compressed.len(),
                "Invalid decompressed Animate bitmap size or trailing stream"
            );
            pixels
        }
        flag => bail!("Unsupported Animate bitmap compression flag: {flag}"),
    };
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in pixels[..size].chunks_exact(stride) {
        for argb in row[..width * 4].as_chunks::<4>().0 {
            let a = argb[0] as u32;
            // Adobe DAT stores premultiplied ARGB, unlike PNG's straight RGBA.
            for channel in &argb[1..] {
                rgba.push(
                    ((*channel as u32 * 255 + a / 2).checked_div(a).unwrap_or(0)).min(255) as u8,
                );
            }
            rgba.push(a as u8);
        }
    }
    rgba_png(width as u32, height as u32, &rgba)
}

#[derive(Clone, Debug, PartialEq)]
struct Pose {
    matrix: Matrix,
    alpha: f64,
    visible: bool,
    image: i64,
    order: Option<usize>,
}
impl Pose {
    fn missing() -> Self {
        Self {
            matrix: IDENTITY,
            alpha: 1.,
            visible: false,
            image: -1,
            order: None,
        }
    }
    fn value(&self, time: f64) -> Value {
        let mut key = json!({"time":time, "matrix":self.matrix, "alpha":self.alpha, "visible":self.visible, "image":self.image});
        if let Some(order) = self.order {
            key["order"] = json!(order);
        }
        key
    }
}
struct Node {
    name: String,
    parent: String,
    kind: String,
    keys: Vec<Value>,
    previous: Option<Pose>,
    bone: bool,
    pivot: Option<Vec<f64>>,
}
struct Importer {
    source: PathBuf,
    output: PathBuf,
    package: BTreeMap<String, Vec<u8>>,
    media: HashMap<String, Element>,
    symbols: HashMap<String, Element>,
    nodes: Vec<Node>,
    node_ids: HashMap<String, usize>,
    images: Vec<String>,
    image_ids: HashMap<String, i64>,
    vectors: HashMap<String, (i64, f64, f64, f64)>,
    source_parents: HashMap<String, String>,
    current: HashMap<String, Pose>,
    pending_bones: Vec<(String, String)>,
    external_images: HashMap<String, PathBuf>,
    baked_ik: usize,
    fps: f64,
}
impl Importer {
    fn read(&self, name: &str) -> Result<&[u8]> {
        let name = safe_member(name)?;
        self.package
            .get(&name)
            .map(Vec::as_slice)
            .with_context(|| format!("Missing FLA member: {name}"))
    }
    fn store_image(&mut self, bytes: Vec<u8>) -> Result<i64> {
        let index = self.images.len();
        let extension = if bytes.starts_with(b"\xff\xd8") {
            "jpg"
        } else {
            "png"
        };
        // Validate the result now, before emitting a compiled resource with broken art.
        let image = crate::image_io::decode(&bytes).context("Invalid FLA bitmap")?;
        ensure!(image.width() > 0 && image.height() > 0, "Empty FLA bitmap");
        let stem = self
            .output
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        let filename = format!("{stem}.images/{index:04}.{extension}");
        let target = self
            .output
            .parent()
            .unwrap_or(Path::new("."))
            .join(&filename);
        std::fs::create_dir_all(target.parent().unwrap())?;
        std::fs::write(target, bytes)?;
        self.images.push(filename);
        Ok(index as i64)
    }
    fn image(&mut self, name: &str) -> Result<i64> {
        if let Some(id) = self.image_ids.get(name) {
            return Ok(*id);
        }
        let media = [
            name.to_string(),
            format!("{name}.png"),
            format!("{name}.jpg"),
        ]
        .iter()
        .find_map(|n| self.media.get(n))
        .cloned();
        let content = if let Some(media) = media {
            let href = media.default("href", name);
            let mut bytes = None;
            // Animate edits embedded bitmap DAT in place; an old linked PNG can
            // remain beside it. Read the editable source before any stale copy.
            let dat = media.attr("bitmapDataHRef");
            if !dat.is_empty() {
                let member = safe_member(&format!("bin/{dat}"))?;
                if let Some(data) = self.package.get(&member) {
                    bytes = Some(bitmap_dat(data)?);
                }
            }
            for member in [href.to_string(), format!("LIBRARY/{href}")] {
                // Invalid paths must never fall through to a DAT or external lookup.
                let member = safe_member(&member)?;
                if bytes.is_some() {
                    break;
                }
                if let Some(data) = self.package.get(&member) {
                    bytes = Some(data.clone());
                    break;
                }
            }
            if let Some(bytes) = bytes {
                bytes
            } else {
                let dat = media.attr("bitmapDataHRef");
                ensure!(
                    !dat.is_empty(),
                    "FLA bitmap has neither linked image nor DAT: {name}"
                );
                bitmap_dat(self.read(&format!("bin/{dat}"))?)?
            }
        } else {
            let key = name
                .strip_prefix("IMAGE_REANIM_")
                .or_else(|| name.strip_prefix("IMAGE_"))
                .unwrap_or(name)
                .to_uppercase();
            let path = self.external_images.get(&key).with_context(|| {
                format!(
                    "Unknown FLA bitmap: {name}; use --resource-root for linked PvZ resource IDs"
                )
            })?;
            std::fs::read(path)?
        };
        let index = self.store_image(content)?;
        self.image_ids.insert(name.to_string(), index);
        Ok(index)
    }
    fn put(
        &mut self,
        name: String,
        parent: &str,
        transform: Matrix,
        alpha: f64,
        kind: &str,
        image: i64,
    ) {
        if !self.node_ids.contains_key(&name) {
            self.node_ids.insert(name.clone(), self.nodes.len());
            self.nodes.push(Node {
                name: name.clone(),
                parent: parent.into(),
                kind: kind.into(),
                keys: Vec::new(),
                previous: None,
                bone: false,
                pivot: None,
            });
        }
        let order = self.current.len();
        self.current.insert(
            name.clone(),
            Pose {
                matrix: transform,
                alpha,
                visible: true,
                image,
                order: Some(order),
            },
        );
        self.source_parents.insert(name, parent.into());
    }
    #[allow(clippy::too_many_arguments)]
    fn element(
        &mut self,
        element: &Element,
        parent: &str,
        frame: usize,
        span_start: usize,
        path: &str,
        depth: usize,
        override_matrix: Option<Matrix>,
        group_basis: Option<Matrix>,
        nonrendering: bool,
    ) -> Result<String> {
        ensure!(depth <= 64, "Cyclic or excessive FLA symbol nesting");
        let kind = element.name.as_str();
        ensure!(
            [
                "DOMBitmapInstance",
                "DOMSymbolInstance",
                "DOMGroup",
                "DOMShape"
            ]
            .contains(&kind),
            "Unsupported FLA element {kind}; export vector/text content as bitmap symbols"
        );
        let mut transform = override_matrix.unwrap_or(matrix(element)?);
        if let Some(basis) = group_basis {
            transform = multiply(inverse(basis)?, transform);
        }
        let alpha = element
            .path("color/Color")
            .map(|e| e.num("alphaMultiplier", 1.))
            .transpose()?
            .unwrap_or(1.);
        let suffix = if !element.attr("name").is_empty() {
            element.attr("name")
        } else if !element.attr("libraryItemName").is_empty() {
            element.attr("libraryItemName")
        } else {
            kind
        };
        let name = format!("{path}/{suffix}");
        let mut image = if kind == "DOMBitmapInstance" && !nonrendering {
            self.image(element.attr("libraryItemName"))?
        } else {
            -1
        };
        if kind == "DOMShape" && !nonrendering {
            let fingerprint = format!("{:x}", Sha256::digest(format!("{element:?}").as_bytes()));
            let (id, left, top, scale) = if let Some(cached) = self.vectors.get(&fingerprint) {
                *cached
            } else {
                let (png, left, top, scale) = rasterize(element)?;
                let item = (self.store_image(png)?, left, top, scale);
                self.vectors.insert(fingerprint, item);
                item
            };
            image = id;
            transform = multiply(transform, [scale, 0., 0., scale, left, top]);
        }
        self.put(
            name.clone(),
            parent,
            transform,
            alpha,
            if image >= 0 { "sprite" } else { "group" },
            image,
        );
        if kind == "DOMGroup" {
            if let Some(members) = element.child("members") {
                let basis = matrix(element)?;
                for (index, child) in members.children.iter().enumerate() {
                    self.element(
                        child,
                        &name,
                        frame,
                        span_start,
                        &format!("{name}/member{index}"),
                        depth + 1,
                        None,
                        Some(basis),
                        nonrendering,
                    )?;
                }
            }
        } else if kind == "DOMSymbolInstance" {
            let library = element.attr("libraryItemName");
            if !library.eq_ignore_ascii_case("locator") {
                let timeline = self
                    .symbols
                    .get(library)
                    .with_context(|| format!("Unknown FLA symbol: {library}"))?
                    .clone();
                let total = duration(&timeline)?;
                let mut local = element.integer("firstFrame", 0)?;
                let loop_mode = element.default("loop", "loop");
                if loop_mode != "single frame" {
                    local = local
                        .checked_add(frame.saturating_sub(span_start))
                        .context("Symbol time overflow")?;
                    local = if loop_mode == "play once" {
                        local.min(total - 1)
                    } else {
                        local % total
                    };
                }
                self.timeline(&timeline, local, &name, &name, depth + 1, nonrendering)?;
            }
        }
        Ok(name)
    }
    fn collect_bone(
        &mut self,
        bone: &Element,
        parent_reference: Option<&str>,
        by_reference: &HashMap<String, String>,
    ) -> Result<()> {
        let reference = bone.attr("referenceID");
        let mut owner = parent_reference;
        if !reference.is_empty() && Some(reference) != parent_reference {
            if let Some(name) = by_reference.get(reference) {
                let id = self.node_ids[name];
                self.nodes[id].bone = true;
                let pivot = bone
                    .default("location", "0,0")
                    .split(',')
                    .map(|v| number(v.trim()))
                    .collect::<Result<Vec<_>>>()?;
                ensure!(pivot.len() == 2, "Invalid FLA bone pivot");
                self.nodes[id].pivot = Some(pivot);
                if let Some(parent_name) = parent_reference.and_then(|r| by_reference.get(r)) {
                    self.pending_bones.push((name.clone(), parent_name.clone()));
                }
            }
            owner = Some(reference);
        }
        if let Some(children) = bone.child("childNodes") {
            for child in &children.children {
                self.collect_bone(child, owner, by_reference)?;
            }
        }
        Ok(())
    }
    fn timeline(
        &mut self,
        timeline: &Element,
        frame: usize,
        parent: &str,
        path: &str,
        depth: usize,
        nonrendering: bool,
    ) -> Result<()> {
        ensure!(depth <= 64, "Cyclic or excessive FLA symbol nesting");
        let layers = timeline
            .child("layers")
            .map(|e| e.children_named("DOMLayer").collect::<Vec<_>>())
            .unwrap_or_default();
        let names: Vec<_> = layers
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{path}/layer{i}:{}", l.attr("name")))
            .collect();
        let exported = layers
            .iter()
            .map(|layer| {
                (depth == 0 || layer.attr("layerType") != "guide")
                    && !(depth == 0
                        && layer.attr("name").starts_with('_')
                        && layer.attr("name") != "_ground")
            })
            .collect::<Vec<_>>();
        for (index, layer) in layers.iter().enumerate().rev() {
            let layer_name = layer.attr("name");
            // FlashReanimExport omits authoring-only root layers, but retains
            // _ground as an image-free transform track. Stage dimensions never
            // participate in these coordinates.
            if !exported[index] {
                continue;
            }
            let nonrendering = nonrendering
                || (depth == 0
                    && (layer_name == "_ground"
                        || layer_name == "fullscreen"
                        || layer_name.starts_with("locator")));
            match layer.attr("layerType") {
                "mask" | "masked" => bail!("FLA mask layers are not supported by Reanim Plus"),
                _ => {}
            }
            let owner_index = layer.attr("parentLayerIndex").parse::<usize>().ok();
            ensure!(owner_index != Some(index), "FLA layer cannot parent itself");
            let owner = owner_index
                .filter(|i| exported.get(*i) == Some(&true))
                .and_then(|i| names.get(i))
                .map(String::as_str)
                .unwrap_or(parent);
            let name = &names[index];
            self.put(name.clone(), owner, IDENTITY, 1., "group", -1);
            // Root visibility and guide flags are editing conveniences, not
            // export filters in FlashReanimExport. Nested enhanced timelines
            // still follow their own authored display visibility.
            if depth > 0 && layer.attr("visible") == "false" {
                self.current.get_mut(name).unwrap().visible = false;
                continue;
            }
            let spans = layer
                .child("frames")
                .map(|e| e.children_named("DOMFrame").collect::<Vec<_>>())
                .unwrap_or_default();
            let mut selected = None;
            for span in &spans {
                let start = span.integer("index", 0)?;
                let end = start
                    .checked_add(span.integer("duration", 1)?)
                    .context("Frame span overflow")?;
                if start <= frame && frame < end {
                    selected = Some(*span);
                    break;
                }
            }
            let Some(span) = selected else { continue };
            let (start, span_duration) = (span.integer("index", 0)?, span.integer("duration", 1)?);
            let elements = span
                .child("elements")
                .map(|e| e.children.iter().collect::<Vec<_>>())
                .unwrap_or_default();
            let mut baked = Vec::new();
            if let Some(list) = span.child("betweenFrameMatrixList") {
                list.descendants("Matrix", &mut baked);
            }
            let mut trees = Vec::new();
            span.descendants("IKTree", &mut trees);
            let is_ik = layer.attr("animationType").contains("IK")
                || span.attr("tweenType").contains("IK")
                || !trees.is_empty();
            ensure!(
                !is_ik || span_duration <= 1 || !baked.is_empty(),
                "IK pose spans require baked betweenFrameMatrixList data or one keyframe per pose"
            );
            let mut by_reference = HashMap::new();
            for (ei, element) in elements.iter().enumerate() {
                let mut override_matrix = None;
                if !baked.is_empty() {
                    let m = if baked.len() == elements.len() * span_duration {
                        Some(baked[ei * span_duration + frame - start])
                    } else if baked.len() == elements.len() * span_duration.saturating_sub(1)
                        && frame > start
                    {
                        Some(baked[ei * (span_duration - 1) + frame - start - 1])
                    } else if frame == start {
                        None
                    } else {
                        bail!("Unrecognized baked FLA IK matrix layout")
                    };
                    if let Some(m) = m {
                        override_matrix = Some(multiply(matrix_attrs(m)?, matrix(element)?));
                        self.baked_ik += 1;
                    }
                } else if span.attr("tweenType") == "motion" && frame > start {
                    let following = spans
                        .iter()
                        .find(|f| f.integer("index", 0).ok() == start.checked_add(span_duration));
                    if let Some(next) = following
                        .and_then(|f| f.child("elements"))
                        .and_then(|e| e.children.get(ei))
                    {
                        let (a, b) = (matrix(element)?, matrix(next)?);
                        let mut t = (frame - start) as f64 / span_duration as f64;
                        // XFL acceleration has the opposite sign of the
                        // authoring API's tweenEasing percentage.
                        let ease = if span.attrs.contains_key("acceleration") {
                            -span.num("acceleration", 0.)?
                        } else {
                            span.num("tweenEasing", 0.)?
                        }
                        .clamp(-100., 100.)
                            / 100.;
                        t = if ease >= 0. {
                            t + (t * (2. - t) - t) * ease
                        } else {
                            t + (t - t * t) * ease
                        };
                        override_matrix = Some(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t));
                    }
                }
                let element_name = self.element(
                    element,
                    name,
                    frame,
                    start,
                    &format!("{name}/element{ei}"),
                    depth,
                    override_matrix,
                    None,
                    nonrendering,
                )?;
                if !element.attr("referenceID").is_empty() {
                    by_reference.insert(element.attr("referenceID").into(), element_name);
                }
            }
            if let Some(tree) = trees.first() {
                for bone in &tree.children {
                    self.collect_bone(bone, None, &by_reference)?;
                }
            }
        }
        Ok(())
    }
    fn world(
        &self,
        name: &str,
        cache: &mut HashMap<String, Matrix>,
        visiting: &mut HashSet<String>,
    ) -> Result<Matrix> {
        if let Some(m) = cache.get(name) {
            return Ok(*m);
        }
        ensure!(visiting.insert(name.into()), "Cyclic FLA parent hierarchy");
        let item = &self.current[name];
        let parent = &self.source_parents[name];
        let transform = if self.current.contains_key(parent) {
            multiply(self.world(parent, cache, visiting)?, item.matrix)
        } else {
            item.matrix
        };
        visiting.remove(name);
        cache.insert(name.into(), transform);
        Ok(transform)
    }
    fn preserve_bones(&mut self) -> Result<()> {
        let (mut cache, mut visiting) = (HashMap::new(), HashSet::new());
        let poses = self
            .pending_bones
            .iter()
            .map(|(n, p)| {
                Ok((
                    n.clone(),
                    p.clone(),
                    self.world(n, &mut cache, &mut visiting)?,
                    self.world(p, &mut cache, &mut visiting)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        for (name, parent, child_world, parent_world) in poses {
            self.current.get_mut(&name).unwrap().matrix =
                multiply(inverse(parent_world)?, child_world);
            self.nodes[self.node_ids[&name]].parent = parent;
        }
        Ok(())
    }
    fn run(&mut self, timeline: &Element) -> Result<Value> {
        let total = duration(timeline)?;
        ensure!(total <= 20000, "FLA exceeds 20000-frame import limit");
        for frame in 0..total {
            self.current.clear();
            self.pending_bones.clear();
            self.timeline(timeline, frame, "", "", 0, false)?;
            self.preserve_bones()?;
            for node in &mut self.nodes {
                let pose = self
                    .current
                    .get(&node.name)
                    .cloned()
                    .unwrap_or_else(Pose::missing);
                if node.keys.is_empty() && frame != 0 {
                    node.keys
                        .push(json!({"time":0., "matrix":IDENTITY,"visible":false}));
                }
                if node.previous.as_ref() != Some(&pose) {
                    if let Some(previous) = &node.previous {
                        let previous_time = node
                            .keys
                            .last()
                            .and_then(|k| k["time"].as_f64())
                            .unwrap_or(0.);
                        if frame > 0 && previous_time < (frame - 1) as f64 / self.fps {
                            node.keys
                                .push(previous.value((frame - 1) as f64 / self.fps));
                        }
                    }
                    node.keys.push(pose.value(frame as f64 / self.fps));
                    node.previous = Some(pose);
                }
            }
        }
        let mut clips = serde_json::Map::new();
        if let Some(layers) = timeline.child("layers") {
            // Original export writes layer names verbatim and makes f=0 only
            // when a sampled frame contains exactly one element. The engine
            // selects the first through last such frame, including any holes.
            // Frame labels and particular names such as anim_open have no
            // special meaning. An entirely empty track uses the engine's
            // documented first=0/count=1 fallback.
            for layer in layers
                .children_named("DOMLayer")
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                let name = layer.attr("name");
                if !name.to_ascii_lowercase().starts_with("anim_") {
                    continue;
                }
                let mut range: Option<(usize, usize)> = None;
                if let Some(frames) = layer.child("frames") {
                    for frame in frames.children_named("DOMFrame") {
                        if frame.child("elements").map(|e| e.children.len()) != Some(1) {
                            continue;
                        }
                        let start = frame.integer("index", 0)?.min(total);
                        let end = start
                            .saturating_add(frame.integer("duration", 1)?)
                            .min(total);
                        if start < end {
                            range = Some(
                                range.map_or((start, end), |(a, b)| (a.min(start), b.max(end))),
                            );
                        }
                    }
                }
                let (start, end) = range.unwrap_or((0, 1));
                clips.entry(name.to_owned()).or_insert_with(
                    || json!({"start":start as f64/self.fps,"end":end as f64/self.fps}),
                );
            }
        }
        clips.insert(
            "default".into(),
            json!({"start":0.,"end":total as f64/self.fps}),
        );
        let nodes = self
            .nodes
            .iter()
            .map(|n| {
                let mut v = json!({"name":n.name,"parent":n.parent,"type":n.kind,"keys":n.keys});
                if n.bone {
                    v["bone"] = json!(true);
                }
                if let Some(pivot) = &n.pivot {
                    v["pivot"] = json!(pivot);
                }
                v
            })
            .collect::<Vec<_>>();
        Ok(
            json!({"format":"pvz-reanim-plus","version":1,"fps":self.fps,"images":self.images,"nodes":nodes,"clips":clips,"source":{"file":self.source.file_name().unwrap_or_default().to_string_lossy(),"bakedIKSamples":self.baked_ik}}),
        )
    }
}

/// Import FLA/XFL into the engine-compatible enhanced graph, extracting images.
/// The caller decides whether to write JSON or `encode` the compiled container.
pub fn import(input: &Path, output: &Path, resource_root: Option<&Path>) -> Result<Value> {
    let package = crate::forward::read_package(input)?;
    let document = Element::parse(
        package
            .get("DOMDocument.xml")
            .context("FLA has no DOMDocument.xml")?,
    )?;
    let fps = document.num("frameRate", 24.)?;
    ensure!(fps > 0. && fps <= 1000., "Invalid FLA frame rate");
    let mut media = HashMap::new();
    if let Some(items) = document.child("media") {
        for item in items.children_named("DOMBitmapItem") {
            media.insert(item.attr("name").into(), item.clone());
        }
    }
    let mut symbols = HashMap::new();
    if let Some(items) = document.child("symbols") {
        for item in items.children_named("Include") {
            let href = safe_member(&format!("LIBRARY/{}", item.attr("href")))?;
            let root = Element::parse(
                package
                    .get(&href)
                    .with_context(|| format!("Missing symbol {href}"))?,
            )?;
            let timeline = root
                .path("timeline/DOMTimeline")
                .context("Symbol has no timeline")?
                .clone();
            symbols.insert(root.attr("name").into(), timeline);
        }
    }
    let mut external_images = HashMap::new();
    if let Some(root) = resource_root {
        for folder in ["reanim", "images"] {
            for entry in walkdir::WalkDir::new(root.join(folder))
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
            {
                if entry.path().extension().is_some_and(|e| {
                    matches!(
                        e.to_string_lossy().to_lowercase().as_str(),
                        "png" | "jpg" | "jpeg"
                    )
                }) {
                    external_images.insert(
                        entry
                            .path()
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_uppercase(),
                        entry.path().to_path_buf(),
                    );
                }
            }
        }
    }
    let timelines = document
        .child("timelines")
        .context("FLA has no main timeline")?;
    let timeline = timelines
        .children_named("DOMTimeline")
        .next()
        .context("FLA has no main timeline")?;
    let mut importer = Importer {
        source: input.into(),
        output: output.into(),
        package,
        media,
        symbols,
        nodes: Vec::new(),
        node_ids: HashMap::new(),
        images: Vec::new(),
        image_ids: HashMap::new(),
        vectors: HashMap::new(),
        source_parents: HashMap::new(),
        current: HashMap::new(),
        pending_bones: Vec::new(),
        external_images,
        baked_ik: 0,
        fps,
    };
    importer.run(timeline)
}

fn zlib(data: &[u8]) -> Result<Vec<u8>> {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::new(6));
    e.write_all(data)?;
    Ok(e.finish()?)
}
fn graph_matrix(key: &Value) -> Result<Matrix> {
    let Some(array) = key.get("matrix") else {
        return Ok(IDENTITY);
    };
    let array = array.as_array().context("Invalid graph matrix")?;
    ensure!(array.len() == 6, "Invalid graph matrix size");
    let mut m = IDENTITY;
    for (i, v) in array.iter().enumerate() {
        m[i] = v
            .as_f64()
            .filter(|v| v.is_finite())
            .context("Invalid graph matrix value")?;
    }
    Ok(m)
}

/// Encode the classic inspection core followed by the compressed RPLS v1 graph.
pub fn encode(graph: &Value) -> Result<Vec<u8>> {
    ensure!(
        graph["format"] == "pvz-reanim-plus" && graph["version"] == 1,
        "Unsupported Reanim Plus graph version"
    );
    let fps = graph["fps"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.)
        .context("Invalid graph FPS")?;
    let clips = graph["clips"].as_object().context("Missing graph clips")?;
    let end = clips
        .values()
        .map(|v| v["end"].as_f64().context("Invalid graph clip end"))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .fold(0., f64::max);
    let frames = (end * fps).round_ties_even().max(1.) as usize;
    ensure!(frames <= 20000, "Enhanced compiled exceeds frame limit");
    let nodes = graph["nodes"].as_array().context("Missing graph nodes")?;
    ensure!(nodes.len() <= 100000, "Too many enhanced nodes");
    let mut tracks = Vec::with_capacity(nodes.len());
    for node in nodes {
        let keys = node["keys"]
            .as_array()
            .filter(|k| !k.is_empty())
            .context("Enhanced node has no keys")?;
        let times = keys
            .iter()
            .map(|k| {
                k["time"]
                    .as_f64()
                    .filter(|v| v.is_finite())
                    .context("Invalid graph key time")
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            times.windows(2).all(|w| w[0] <= w[1]),
            "Enhanced keys are not sorted"
        );
        let mut transforms = Vec::with_capacity(frames);
        for frame in 0..frames {
            let i = times
                .partition_point(|t| *t <= frame as f64 / fps)
                .saturating_sub(1);
            let key = &keys[i];
            let m = graph_matrix(key)?;
            transforms.push(Transform {
                values: [
                    m[4] as f32,
                    m[5] as f32,
                    m[1].atan2(m[0]).to_degrees() as f32,
                    (-m[2]).atan2(m[3]).to_degrees() as f32,
                    m[0].hypot(m[1]) as f32,
                    m[2].hypot(m[3]) as f32,
                    if key["visible"].as_bool().unwrap_or(true) {
                        0.
                    } else {
                        -1.
                    },
                    key["alpha"].as_f64().unwrap_or(1.) as f32,
                ],
                ..Transform::default()
            });
        }
        tracks.push(Track {
            name: node["name"]
                .as_str()
                .context("Enhanced node has no name")?
                .into(),
            transforms,
        });
    }
    let mut result = crate::codec::encode(
        &Definition {
            fps: fps as f32,
            tracks,
        },
        false,
    )?;
    let payload = serde_json::to_vec(graph)?;
    ensure!(payload.len() <= MAX_DECODED, "RPLS graph exceeds limit");
    let mut extension = (payload.len() as u32).to_le_bytes().to_vec();
    extension.extend(zlib(&payload)?);
    result.extend(&extension);
    result.extend((extension.len() as u32).to_le_bytes());
    result.extend(b"RPLS");
    Ok(result)
}

/// Read and validate a graph extension without consuming an unbounded allocation.
pub fn read_graph(bytes: &[u8]) -> Result<Option<Value>> {
    if !bytes.ends_with(b"RPLS") {
        return Ok(None);
    }
    ensure!(bytes.len() >= 16, "Truncated RPLS trailer");
    let length = u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into()?) as usize;
    ensure!(
        length >= 4 && length <= bytes.len() - 16,
        "Invalid RPLS extension length"
    );
    let block = &bytes[bytes.len() - 8 - length..bytes.len() - 8];
    let size = u32::from_le_bytes(block[..4].try_into()?) as usize;
    ensure!(size <= MAX_DECODED, "RPLS graph exceeds limit");
    let mut decoder = flate2::bufread::ZlibDecoder::new(&block[4..]);
    let mut decoded = Vec::new();
    decoder
        .by_ref()
        .take(size as u64 + 1)
        .read_to_end(&mut decoded)?;
    ensure!(
        decoded.len() == size && decoder.total_in() as usize == block.len() - 4,
        "RPLS extension size mismatch or trailing stream"
    );
    let graph: Value = serde_json::from_slice(&decoded)?;
    ensure!(
        graph["format"] == "pvz-reanim-plus" && graph["version"] == 1,
        "Unsupported RPLS graph version"
    );
    Ok(Some(graph))
}

type Point = (f64, f64);
type Segment = (Point, Point, Option<Point>);
fn number(text: &str) -> Result<f64> {
    let result = if let Some(hex) = text.strip_prefix('#') {
        let (whole, fraction) = hex.split_once('.').unwrap_or((hex, ""));
        ensure!(
            whole.len() <= 12 && fraction.len() <= 12,
            "XFL hex number overflow"
        );
        let bits = whole.len() * 4;
        let mut value = i64::from_str_radix(whole, 16)?;
        if whole.len() >= 6 && value >= (1i64 << (bits - 1)) {
            value -= 1i64 << bits;
        }
        value as f64
            + if fraction.is_empty() {
                0.
            } else {
                u64::from_str_radix(fraction, 16)? as f64 / 16f64.powi(fraction.len() as i32)
            }
    } else {
        text.parse()?
    };
    ensure!(result.is_finite(), "Non-finite XFL vector number");
    Ok(result)
}
fn segments(edges: &str) -> Result<Vec<Segment>> {
    let styles = regex::Regex::new(r"S\d+")?;
    let text = styles.replace_all(edges, "");
    let pattern =
        regex::Regex::new(r"[!|\[]|#[\da-fA-F]+(?:\.[\da-fA-F]+)?|[-+]?(?:\d*\.\d+|\d+)")?;
    let tokens = pattern
        .find_iter(&text)
        .map(|m| m.as_str())
        .collect::<Vec<_>>();
    let (mut index, mut current, mut command) = (0, (0., 0.), None);
    let mut result = Vec::new();
    while index < tokens.len() {
        if matches!(tokens[index], "!" | "|" | "[") {
            command = Some(tokens[index]);
            index += 1;
        }
        let size = if command == Some("[") { 4 } else { 2 };
        ensure!(
            command.is_some() && index + size <= tokens.len(),
            "Unsupported/truncated XFL vector edge"
        );
        let values = tokens[index..index + size]
            .iter()
            .map(|t| Ok(number(t)? / 20.))
            .collect::<Result<Vec<_>>>()?;
        index += size;
        let end = (values[size - 2], values[size - 1]);
        if command == Some("!") {
            current = end;
            command = Some("|");
            continue;
        }
        let control = if command == Some("[") {
            Some((values[0], values[1]))
        } else {
            None
        };
        result.push((current, end, control));
        current = end;
    }
    Ok(result)
}
fn vector_path(parts: &[Segment]) -> String {
    let mut pending = parts.to_vec();
    let mut result = String::new();
    while !pending.is_empty() {
        let start = pending[0].0;
        let mut current = start;
        result.push_str(&format!("M {} {} ", start.0, start.1));
        while let Some(index) = pending.iter().rposition(|p| p.0 == current) {
            let (_, end, control) = pending.remove(index);
            if let Some(c) = control {
                result.push_str(&format!("Q {} {} {} {} ", c.0, c.1, end.0, end.1));
            } else {
                result.push_str(&format!("L {} {} ", end.0, end.1));
            }
            current = end;
            if current == start {
                result.push_str("Z ");
                break;
            }
        }
    }
    result
}
fn paint(e: &Element) -> Result<(&str, f64)> {
    let mut solids = Vec::new();
    e.descendants("SolidColor", &mut solids);
    let solid = solids
        .first()
        .context("Vector gradient/bitmap fills require an exported bitmap")?;
    let color = solid.default("color", "#000000");
    ensure!(
        color.len() == 7
            && color.starts_with('#')
            && color[1..].bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid XFL vector color"
    );
    Ok((color, solid.num("alpha", 1.)?))
}
fn rasterize(shape: &Element) -> Result<(Vec<u8>, f64, f64, f64)> {
    let fills = shape
        .child("fills")
        .map(|n| {
            n.children_named("FillStyle")
                .map(|e| Ok((e.integer("index", 0)?, e)))
                .collect::<Result<BTreeMap<_, _>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let strokes = shape
        .child("strokes")
        .map(|n| {
            n.children_named("StrokeStyle")
                .map(|e| Ok((e.integer("index", 0)?, e)))
                .collect::<Result<BTreeMap<_, _>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let (mut fill_parts, mut stroke_parts): (
        BTreeMap<usize, Vec<Segment>>,
        BTreeMap<usize, Vec<Segment>>,
    ) = (BTreeMap::new(), BTreeMap::new());
    let mut points = Vec::new();
    if let Some(edges) = shape.child("edges") {
        for edge in edges.children_named("Edge") {
            let parts = segments(edge.attr("edges"))?;
            for (a, b, c) in &parts {
                points.extend([*a, *b]);
                if let Some(c) = c {
                    points.push(*c);
                }
            }
            for (field, reverse) in [("fillStyle0", true), ("fillStyle1", false)] {
                let style = edge.integer(field, 0)?;
                if style != 0 {
                    fill_parts.entry(style).or_default().extend(
                        parts
                            .iter()
                            .map(|(a, b, c)| if reverse { (*b, *a, *c) } else { (*a, *b, *c) }),
                    );
                }
            }
            let stroke = edge.integer("strokeStyle", 0)?;
            if stroke != 0 {
                stroke_parts.entry(stroke).or_default().extend(parts);
            }
        }
    }
    ensure!(
        !points.is_empty(),
        "FLA vector shape contains no supported edges"
    );
    let mut padding = 1f64;
    for stroke in strokes.values() {
        if let Some(s) = stroke.child("SolidStroke") {
            padding = padding.max(s.num("weight", 1.)?);
        }
    }
    padding += 1.;
    let left = (points.iter().map(|p| p.0).fold(f64::INFINITY, f64::min) - padding).floor();
    let top = (points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min) - padding).floor();
    let width =
        (points.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max) + padding - left).ceil();
    let height =
        (points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max) + padding - top).ceil();
    ensure!(
        width > 0. && height > 0. && width * height <= 16. * 1024. * 1024.,
        "FLA vector part exceeds rasterization limit"
    );
    let mut content = String::new();
    for (style, parts) in fill_parts {
        let fill = fills.get(&style).context("Missing vector fill style")?;
        let (color, alpha) = paint(fill)?;
        content.push_str(&format!(
            "<path d=\"{}\" fill=\"{color}\" fill-opacity=\"{alpha}\" fill-rule=\"evenodd\"/>",
            vector_path(&parts)
        ));
    }
    for (style, parts) in stroke_parts {
        let stroke = strokes
            .get(&style)
            .and_then(|s| s.child("SolidStroke"))
            .context("Unsupported XFL stroke type")?;
        let (color, alpha) = paint(stroke)?;
        let weight = stroke.num("weight", 1.)?;
        content.push_str(&format!("<path d=\"{}\" fill=\"none\" stroke=\"{color}\" stroke-opacity=\"{alpha}\" stroke-width=\"{weight}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>",vector_path(&parts)));
    }
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"{left} {top} {width} {height}\">{content}</svg>"
    );
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32 * 2, height as u32 * 2)
        .context("Cannot allocate vector bitmap")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(2., 2.),
        &mut pixmap.as_mut(),
    );
    Ok((pixmap.encode_png()?, left, top, 0.5))
}
