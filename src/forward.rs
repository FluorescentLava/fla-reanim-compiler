//! Animate ZIP/XFL reader and the classic PopCap reanim compiler.
use crate::{Definition, MISSING, Track, Transform};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{Cursor, Read},
    path::Path,
};

const MAX_PACKAGE: u64 = 512 * 1024 * 1024;
const MAX_MEMBER: u64 = 128 * 1024 * 1024;
const IDENTITY: [f64; 6] = [1., 0., 0., 1., 0., 0.];

/// Read a ZIP FLA or an unpacked XFL directory. No archive member is extracted.
/// Animate can overcount the final central-directory record; repair only the
/// declared directory length after validating every central-directory record.
pub fn read_package(input: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut result = BTreeMap::new();
    let mut total = 0u64;
    if input.is_dir() {
        for entry in walkdir::WalkDir::new(input).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry
                .path()
                .strip_prefix(input)?
                .to_string_lossy()
                .replace('\\', "/");
            validate_member(&name)?;
            let size = entry.metadata()?.len();
            total = total
                .checked_add(size)
                .context("XFL package size overflow")?;
            ensure!(
                size <= MAX_MEMBER && total <= MAX_PACKAGE,
                "XFL package exceeds size limit"
            );
            result.insert(name, fs::read(entry.path())?);
        }
    } else {
        ensure!(
            fs::metadata(input)?.len() <= MAX_PACKAGE,
            "FLA exceeds package size limit"
        );
        let mut bytes = fs::read(input).with_context(|| format!("Read {}", input.display()))?;
        // Repair the known Animate defect even if the ZIP reader accepts it: a
        // successful open must still not trust a fabricated directory boundary.
        repair_directory(&mut bytes)?;
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .context("FLA must be an Animate ZIP/XFL file (legacy binary FLA is unsupported)")?;
        ensure!(archive.len() <= 100_000, "Too many FLA members");
        for index in 0..archive.len() {
            let mut member = archive.by_index(index)?;
            if member.is_dir() {
                continue;
            }
            let name = member.name().replace('\\', "/");
            validate_member(&name)?;
            total = total
                .checked_add(member.size())
                .context("FLA package size overflow")?;
            ensure!(
                member.size() <= MAX_MEMBER && total <= MAX_PACKAGE,
                "FLA package exceeds decompression limit"
            );
            let mut content = Vec::with_capacity(member.size() as usize);
            member
                .by_ref()
                .take(MAX_MEMBER + 1)
                .read_to_end(&mut content)?;
            ensure!(
                content.len() as u64 == member.size(),
                "FLA member size mismatch"
            );
            ensure!(
                result.insert(name, content).is_none(),
                "Duplicate FLA member"
            );
        }
    }
    ensure!(!result.is_empty(), "FLA/XFL package is empty");
    Ok(result)
}

fn validate_member(name: &str) -> Result<()> {
    ensure!(
        !name.starts_with('/')
            && !name.contains(':')
            && !name.split('/').any(|p| p == "..")
            && !name.contains('\0'),
        "Invalid FLA member path: {name}"
    );
    Ok(())
}
fn u16_at(b: &[u8], p: usize) -> Option<usize> {
    b.get(p..p + 2)
        .map(|v| u16::from_le_bytes([v[0], v[1]]) as usize)
}
fn u32_at(b: &[u8], p: usize) -> Option<usize> {
    b.get(p..p + 4)
        .map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]) as usize)
}
fn repair_directory(bytes: &mut [u8]) -> Result<()> {
    let lower = bytes.len().saturating_sub(65_557);
    let Some(end) = (lower..bytes.len().saturating_sub(21))
        .rev()
        .find(|&i| bytes.get(i..i + 4) == Some(b"PK\x05\x06"))
    else {
        bail!("FLA is not an Animate ZIP/XFL archive");
    };
    let comment = u16_at(bytes, end + 20).context("Truncated ZIP footer")?;
    ensure!(
        end + 22 + comment == bytes.len(),
        "Invalid ZIP footer/comment length"
    );
    ensure!(
        u16_at(bytes, end + 4) == Some(0) && u16_at(bytes, end + 6) == Some(0),
        "Split FLA archives are unsupported"
    );
    let count = u16_at(bytes, end + 10).context("Invalid ZIP entry count")?;
    if count == 0xffff {
        return Ok(());
    } // zip crate validates ZIP64 separately.
    ensure!(
        u16_at(bytes, end + 8) == Some(count),
        "Split FLA archive entry count"
    );
    let start = u32_at(bytes, end + 16).context("Invalid ZIP directory offset")?;
    let declared = u32_at(bytes, end + 12).context("Invalid ZIP directory size")?;
    ensure!(start <= end, "ZIP directory points beyond footer");
    let mut cursor = start;
    for _ in 0..count {
        ensure!(
            cursor.checked_add(46).is_some_and(|n| n <= end)
                && bytes.get(cursor..cursor + 4) == Some(b"PK\x01\x02"),
            "Invalid FLA central directory"
        );
        let length = 46
            + u16_at(bytes, cursor + 28).unwrap()
            + u16_at(bytes, cursor + 30).unwrap()
            + u16_at(bytes, cursor + 32).unwrap();
        cursor = cursor
            .checked_add(length)
            .context("ZIP directory overflow")?;
        ensure!(cursor <= end, "Truncated ZIP directory entry");
    }
    ensure!(
        cursor == end,
        "Unrecognized bytes after FLA central directory"
    );
    if declared != end - start {
        ensure!(
            declared == end - start + 54,
            "Unsupported corrupt ZIP directory size"
        );
        bytes[end + 12..end + 16].copy_from_slice(&((end - start) as u32).to_le_bytes());
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct Element {
    tag: String,
    attrs: HashMap<String, String>,
    children: Vec<Element>,
    text: String,
}
impl Element {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes)
            .context("XFL XML is not UTF-8")?
            .trim_start_matches('\u{feff}');
        let doc = roxmltree::Document::parse(text)?;
        fn own(node: roxmltree::Node<'_, '_>) -> Element {
            Element {
                tag: node.tag_name().name().into(),
                attrs: node
                    .attributes()
                    .map(|a| (a.name().into(), a.value().into()))
                    .collect(),
                children: node
                    .children()
                    .filter(|n| n.is_element())
                    .map(own)
                    .collect(),
                text: node
                    .children()
                    .filter(|n| n.is_text())
                    .filter_map(|n| n.text())
                    .collect(),
            }
        }
        Ok(own(doc.root_element()))
    }
    fn attr(&self, name: &str) -> &str {
        self.attrs.get(name).map(String::as_str).unwrap_or("")
    }
    fn number(&self, name: &str, default: f64) -> f64 {
        self.attr(name)
            .parse()
            .ok()
            .filter(|v: &f64| v.is_finite())
            .unwrap_or(default)
    }
    fn index(&self, name: &str, default: usize) -> usize {
        self.attr(name).parse().unwrap_or(default)
    }
    fn child(&self, tag: &str) -> Option<&Self> {
        self.children.iter().find(|c| c.tag == tag)
    }
    fn path(&self, path: &[&str]) -> Option<&Self> {
        path.iter().try_fold(self, |n, t| n.child(t))
    }
    fn list(&self, a: &str, b: &str) -> Vec<&Self> {
        self.child(a)
            .map(|n| n.children.iter().filter(|c| c.tag == b).collect())
            .unwrap_or_default()
    }
    fn descendants<'a>(&'a self, tag: &str, out: &mut Vec<&'a Self>) {
        for c in &self.children {
            if c.tag == tag {
                out.push(c)
            }
            c.descendants(tag, out)
        }
    }
    fn matrix(&self) -> [f64; 6] {
        let m = self
            .path(&["matrix", "Matrix"])
            .or_else(|| self.child("Matrix"));
        m.map(|n| {
            [
                n.number("a", 1.),
                n.number("b", 0.),
                n.number("c", 0.),
                n.number("d", 1.),
                n.number("tx", 0.),
                n.number("ty", 0.),
            ]
        })
        .unwrap_or(IDENTITY)
    }
    fn alpha(&self) -> f64 {
        self.path(&["color", "Color"])
            .or_else(|| self.child("Color"))
            .map(|n| n.number("alphaMultiplier", n.number("alphaPercent", 100.) / 100.))
            .unwrap_or(1.)
    }
    fn point(&self) -> [f64; 2] {
        self.path(&["transformationPoint", "Point"])
            .or_else(|| self.child("Point"))
            .map(|n| [n.number("x", 0.), n.number("y", 0.)])
            .unwrap_or([0., 0.])
    }
}
fn timelines(root: &Element) -> Vec<&Element> {
    let mut v = root.list("timelines", "DOMTimeline");
    v.extend(root.list("timeline", "DOMTimeline"));
    v
}
fn ignored(layer: &Element) -> bool {
    // FlashReanimExport.jsfl::WriteReanimLayer skips authoring helper names,
    // but exports _ground and does not use editor visibility as game state.
    let name = layer.attr("name");
    name.starts_with('_') && name != "_ground"
}
fn nonrendering_layer(name: &str) -> bool {
    name == "_ground" || name == "fullscreen" || name.starts_with("locator")
}
fn frames(layer: &Element) -> Vec<&Element> {
    layer.list("frames", "DOMFrame")
}
fn native_bitmap_symbol(name: &str) -> bool {
    name.starts_with("ReanimBitmap_") || name.starts_with("__reanim_bitmap_")
}
fn symbol_frame(root: &Element, index: usize) -> Option<&Element> {
    let timeline = timelines(root).into_iter().next()?;
    let layer = timeline.child("layers")?.child("DOMLayer")?;
    frames(layer).into_iter().find(|frame| {
        let start = frame.index("index", 0);
        start <= index && index < start.saturating_add(frame.index("duration", 1).max(1))
    })
}
fn blank() -> Transform {
    let mut values = [MISSING; 8];
    values[6] = -1.;
    Transform {
        values,
        ..Transform::default()
    }
}

struct Package {
    document: Element,
    libraries: HashMap<String, Element>,
    resources: Option<Catalog>,
    resource_ids: HashMap<String, String>,
    helper_tracks: HashSet<String>,
}
impl Package {
    fn open(input: &Path, resources: Option<&Path>) -> Result<Self> {
        Self::open_impl(input, resources, true)
    }
    fn open_impl(input: &Path, resources: Option<&Path>, load_catalog: bool) -> Result<Self> {
        let members = read_package(input)?;
        let (document_name, bytes) = members
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("DOMDocument.xml"))
            .or_else(|| {
                members
                    .iter()
                    .find(|(n, _)| n.to_ascii_lowercase().ends_with("/domdocument.xml"))
            })
            .context("FLA has no DOMDocument.xml")?;
        let prefix = document_name.strip_suffix("DOMDocument.xml").unwrap_or("");
        let document = Element::parse(bytes)?;
        let mut helper_tracks = HashSet::new();
        for property in document.list("persistentData", "PD") {
            if property.attr("n") == "fla-reanim.compiled-helper-tracks"
                && property.attr("t") == "s"
            {
                let names: Vec<String> = serde_json::from_str(property.attr("v"))
                    .context("Invalid fla-reanim.compiled-helper-tracks JSON")?;
                helper_tracks.extend(names);
            }
        }
        let mut resource_ids = HashMap::new();
        for item in document.list("media", "DOMBitmapItem") {
            for property in item.list("persistentData", "PD") {
                if property.attr("n") == "fla-reanim.resource-id"
                    && property.attr("t") == "s"
                    && !property.attr("v").is_empty()
                {
                    resource_ids.insert(item.attr("name").into(), property.attr("v").into());
                }
            }
        }
        let mut libraries = HashMap::new();
        for (name, bytes) in &members {
            let relative = name.strip_prefix(prefix).unwrap_or(name);
            if relative.to_ascii_lowercase().starts_with("library/")
                && relative.to_ascii_lowercase().ends_with(".xml")
            {
                let root =
                    Element::parse(bytes).with_context(|| format!("Invalid library XML {name}"))?;
                for candidate in [relative, root.attr("name")] {
                    for key in library_keys(candidate) {
                        libraries.entry(key).or_insert_with(|| root.clone());
                    }
                }
            }
        }
        Ok(Self {
            document,
            libraries,
            resource_ids,
            helper_tracks,
            resources: if load_catalog {
                Catalog::load_for(input, resources)?
            } else {
                None
            },
        })
    }
    fn library(&self, name: &str) -> Option<&Element> {
        library_keys(name)
            .iter()
            .find_map(|k| self.libraries.get(k))
    }
    fn image(&self, name: &str) -> String {
        if let Some(id) = self.resource_ids.get(name) {
            return id.clone();
        }
        self.resources
            .as_ref()
            .map(|c| c.resolve(name))
            .unwrap_or_else(|| image_id(name))
    }
    fn ignored(&self, layer: &Element) -> bool {
        ignored(layer) && !self.helper_tracks.contains(layer.attr("name"))
    }
    fn timeline(&self) -> Result<&Element> {
        // A saved document's main timeline is the export target. Library symbols
        // are dependencies, not alternate candidates ranked by their contents.
        // In particular a blank main scene must not silently export some other
        // animation from the library.
        timelines(&self.document)
            .into_iter()
            .next()
            .context("FLA has no main document timeline")
    }
}

/// Whether visible content requires the enhanced hierarchy/vector importer.
/// Locator artwork is only an authoring aid and never forces Plus mode.
pub fn needs_plus(input: &Path) -> Result<bool> {
    let p = Package::open_impl(input, None, false)?;
    requires_plus(&p, p.timeline()?, 0, false)
}
fn requires_plus(
    package: &Package,
    node: &Element,
    depth: usize,
    nonrendering: bool,
) -> Result<bool> {
    ensure!(depth <= 64, "Cyclic or excessive FLA symbol nesting");
    if node.tag == "DOMLayer" && package.ignored(node) {
        return Ok(false);
    }
    let nonrendering =
        nonrendering || (node.tag == "DOMLayer" && nonrendering_layer(node.attr("name")));
    if node.tag == "DOMLayer" {
        for frame in frames(node) {
            if frame
                .child("elements")
                .is_some_and(|e| e.children.len() == 1)
                && requires_plus(package, frame, depth, nonrendering)?
            {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if node.tag == "IKTree" || (node.tag == "DOMShape" && !nonrendering) {
        return Ok(true);
    }
    if matches!(
        node.tag.as_str(),
        "DOMSymbolInstance" | "DOMGraphicInstance"
    ) {
        let name = node.attr("libraryItemName");
        if basename(name).eq_ignore_ascii_case("locator") {
            return Ok(false);
        }
        if let Some(library) = package.library(name) {
            let index = if native_bitmap_symbol(name) {
                node.index("firstFrame", 0)
            } else {
                0
            };
            return symbol_frame(library, index)
                .map(|frame| requires_plus(package, frame, depth + 1, nonrendering))
                .unwrap_or(Ok(false));
        }
    }
    for child in &node.children {
        if requires_plus(package, child, depth, nonrendering)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn convert(input: &Path) -> Result<Definition> {
    convert_with_resources(input, None)
}
pub fn convert_with_resources(input: &Path, resources: Option<&Path>) -> Result<Definition> {
    let package = Package::open(input, resources)?;
    let timeline = package.timeline()?;
    let mut ik = Vec::new();
    timeline.descendants("IKTree", &mut ik);
    ensure!(
        ik.is_empty(),
        "Skeletal IK FLA requires enhanced --plus compilation"
    );
    let mut layers = timeline.list("layers", "DOMLayer");
    // The document frame count includes skipped helper layers as in JSFL's
    // gTimeLine.frameCount. Filter only after calculating it.
    let total = layers
        .iter()
        .flat_map(|l| frames(l))
        .map(|f| {
            f.index("index", 0)
                .saturating_add(f.index("duration", 1).max(1))
        })
        .max()
        .unwrap_or(1)
        .max(1);
    ensure!(total <= 20000, "FLA exceeds 20000-frame limit");
    layers.retain(|l| !package.ignored(l));
    layers.reverse();
    let fps = package.document.number("frameRate", 12.);
    ensure!(fps > 0. && fps <= f32::MAX as f64, "Invalid FLA frame rate");
    let mut result = Definition {
        fps: fps as f32,
        tracks: Vec::new(),
    };
    for (layer_index, layer) in layers.iter().enumerate() {
        let mut spans = frames(layer);
        spans.sort_by_key(|f| f.index("index", 0));
        let layer_name = layer.attr("name");
        let name = if layer_name.is_empty() {
            format!("track_{}", layer_index + 1)
        } else {
            layer_name.into()
        };
        let samples = spans
            .iter()
            .map(|s| collect_frame(&package, s, layer_name))
            .collect::<Result<Vec<_>>>()?;
        // JSFL exports every non-helper layer, including entirely blank ones.
        // anim_ names have no special handling: occupied frames carry f=0 and
        // empty frames f=-1, exactly like an ordinary artwork layer.
        let count = samples.iter().map(Vec::len).max().unwrap_or(0).max(1);
        ensure!(
            result.tracks.len().saturating_add(count) <= 100000,
            "Too many reanim tracks"
        );
        for render_index in 0..count {
            let track_name = if render_index == 0 {
                name.clone()
            } else {
                format!("{name}{}", render_index + 1)
            };
            let mut track = Track {
                name: track_name.clone(),
                transforms: vec![blank(); total],
            };
            for (key_index, span) in spans.iter().enumerate() {
                let start_frame = span.index("index", 0).min(total);
                let end_frame = start_frame
                    .saturating_add(span.index("duration", 1).max(1))
                    .min(total);
                let Some(start) = samples[key_index].get(render_index) else {
                    continue;
                };
                let a = build_transform(&package, start, layer_name);
                let next = samples.get(key_index + 1).and_then(|s| s.get(render_index));
                let endpoint = next.map(|next| build_transform(&package, next, layer_name));
                for frame in start_frame..end_frame {
                    track.transforms[frame] = if span
                        .attr("tweenType")
                        .eq_ignore_ascii_case("motion")
                    {
                        next.zip(endpoint.as_ref())
                            .map(|(next, endpoint)| {
                                let time = (frame - start_frame) as f32
                                    / (end_frame - start_frame).max(1) as f32;
                                // Animate stores the opposite sign of JSFL's
                                // tweenEasing in the XFL acceleration attribute.
                                let ease = if span.attrs.contains_key("acceleration") {
                                    -span.number("acceleration", 0.)
                                } else {
                                    span.number("tweenEasing", 0.)
                                }
                                .clamp(-100., 100.)
                                    as f32
                                    / 100.;
                                tween(start, next, &a, endpoint, time + ease * time * (1. - time))
                            })
                            .unwrap_or_else(|| a.clone())
                    } else {
                        a.clone()
                    };
                }
            }
            // JSFL uses '_' to clear text because an empty value means carry
            // the previous string forward in the reanim format.
            let mut previous_text = String::new();
            for transform in &mut track.transforms {
                if transform.values[6] < 0. {
                    continue;
                }
                if transform.text.is_empty() && !previous_text.is_empty() {
                    transform.text = "_".into();
                    previous_text.clear();
                } else if !transform.text.is_empty() {
                    previous_text.clone_from(&transform.text);
                }
            }
            normalize_angles(&mut track);
            result.tracks.push(track);
        }
    }
    ensure!(!result.tracks.is_empty(), "No reanim content found in FLA");
    Ok(result)
}

#[derive(Clone, Copy)]
struct Basis {
    outer: Option<[f64; 6]>,
    inner: [f64; 6],
}
impl Basis {
    fn origin() -> Self {
        Self {
            outer: None,
            inner: IDENTITY,
        }
    }
    fn child(self, matrix: [f64; 6]) -> Self {
        if self.outer.is_none() {
            Self {
                outer: Some(matrix),
                inner: IDENTITY,
            }
        } else {
            Self {
                outer: self.outer,
                inner: multiply(self.inner, matrix),
            }
        }
    }
    fn matrix(self) -> [f64; 6] {
        multiply(self.outer.unwrap_or(IDENTITY), self.inner)
    }
}

#[derive(Clone)]
struct Renderable {
    matrix: [f64; 6],
    basis: Basis,
    alpha: f32,
    point: [f64; 2],
    image: String,
    frame: f32,
    locator: bool,
    text: String,
    font: String,
    /// Deepest symbol directly containing the unwrapped base element.
    base_symbol: String,
}
fn multiply(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}
fn inverse(m: [f64; 6]) -> Option<[f64; 6]> {
    let determinant = m[0] * m[3] - m[1] * m[2];
    if determinant.abs() < 1e-12 {
        return None;
    }
    let linear = [
        m[3] / determinant,
        -m[1] / determinant,
        -m[2] / determinant,
        m[0] / determinant,
    ];
    Some([
        linear[0],
        linear[1],
        linear[2],
        linear[3],
        -linear[0] * m[4] - linear[2] * m[5],
        -linear[1] * m[4] - linear[3] * m[5],
    ])
}
fn point(m: [f64; 6], p: [f64; 2]) -> [f64; 2] {
    [
        m[0] * p[0] + m[2] * p[1] + m[4],
        m[1] * p[0] + m[3] * p[1] + m[5],
    ]
}
fn collect_frame(p: &Package, frame: &Element, layer_name: &str) -> Result<Vec<Renderable>> {
    let mut out = Vec::new();
    if let Some(elements) = frame.child("elements") {
        // WriteReanimFrame explicitly blanks a top-level frame containing zero
        // or multiple elements. Only an inner symbol can expand to tracks.
        if elements.children.len() == 1 {
            collect(
                p,
                elements,
                Basis::origin(),
                1.,
                0,
                nonrendering_layer(layer_name),
                &mut out,
            )?;
            for renderable in &mut out {
                renderable.point = elements.children[0].point();
            }
            if layer_name.starts_with("attacher__") {
                let root_symbol = elements.children[0].attr("libraryItemName");
                for renderable in &mut out {
                    let bitmap_name = renderable.image.split('.').next().unwrap_or("");
                    let command = if renderable.base_symbol.contains("__") {
                        renderable.base_symbol.as_str()
                    } else if root_symbol.contains("__") {
                        root_symbol
                    } else if bitmap_name.starts_with("attacher__") {
                        bitmap_name
                    } else {
                        ""
                    };
                    if !command.is_empty() {
                        renderable.text = command.into();
                        renderable.image.clear();
                        renderable.locator = true;
                    }
                }
            }
        }
    }
    Ok(out)
}
fn collect(
    p: &Package,
    container: &Element,
    parent: Basis,
    alpha: f32,
    depth: usize,
    nonrendering: bool,
    out: &mut Vec<Renderable>,
) -> Result<()> {
    ensure!(depth <= 64, "Cyclic or excessive FLA symbol nesting");
    for element in &container.children {
        let basis = parent.child(element.matrix());
        let matrix = basis.matrix();
        let alpha = alpha * element.alpha() as f32;
        match element.tag.as_str() {
            "DOMBitmapInstance" => out.push(Renderable {
                matrix,
                basis,
                alpha,
                point: element.point(),
                image: ["libraryItemName", "href", "bitmapDataHRef", "name"]
                    .iter()
                    .map(|k| element.attr(k))
                    .find(|v| !v.is_empty())
                    .unwrap_or("")
                    .into(),
                frame: element.number("firstFrame", 0.) as f32,
                locator: false,
                text: String::new(),
                font: String::new(),
                base_symbol: String::new(),
            }),
            "DOMSymbolInstance" | "DOMGraphicInstance" => {
                let name = element.attr("libraryItemName");
                // The crosshair drawing inside a locator symbol is an editor
                // aid. A locator always compiles to its transform only.
                if basename(name).eq_ignore_ascii_case("locator") {
                    out.push(Renderable {
                        matrix,
                        basis,
                        alpha,
                        point: element.point(),
                        image: String::new(),
                        frame: element.number("firstFrame", 0.) as f32,
                        locator: true,
                        text: String::new(),
                        font: String::new(),
                        base_symbol: name.into(),
                    });
                    continue;
                }
                if let Some(root) = p.library(name) {
                    // Original JSFL unwraps only layer 0 / frame 0 of a symbol.
                    // The native sprite wrapper is an explicit extension: its
                    // one-frame cels encode the reanim sprite-sheet frame.
                    let index = if native_bitmap_symbol(name) {
                        element.index("firstFrame", 0)
                    } else {
                        0
                    };
                    let elements = symbol_frame(root, index).and_then(|f| f.child("elements"));
                    if let Some(elements) = elements.filter(|e| !e.children.is_empty()) {
                        let before = out.len();
                        collect(p, elements, basis, alpha, depth + 1, nonrendering, out)?;
                        for renderable in &mut out[before..] {
                            renderable.point = element.point();
                            if renderable.base_symbol.is_empty() {
                                renderable.base_symbol = name.into();
                            }
                        }
                    } else {
                        // An empty symbol is still an occupied root frame. Its
                        // transform is meaningful for anim_/locator tracks.
                        out.push(Renderable {
                            matrix,
                            basis,
                            alpha,
                            point: element.point(),
                            image: String::new(),
                            frame: 0.,
                            locator: true,
                            text: String::new(),
                            font: String::new(),
                            base_symbol: name.into(),
                        });
                    }
                } else {
                    bail!("Missing FLA library symbol: {name}");
                }
            }
            "DOMShape" if nonrendering => out.push(Renderable {
                matrix,
                basis,
                alpha,
                point: element.point(),
                image: String::new(),
                frame: 0.,
                locator: true,
                text: String::new(),
                font: String::new(),
                base_symbol: String::new(),
            }),
            "DOMShape" => bail!(
                "Classic compilation cannot render {}; use --plus for vector parts",
                element.tag
            ),
            "DOMStaticText" => {
                let runs = element.list("textRuns", "DOMTextRun");
                ensure!(
                    runs.len() == 1,
                    "Classic text requires exactly one DOMTextRun"
                );
                let run = runs[0];
                let attrs = run
                    .path(&["textAttrs", "DOMTextAttrs"])
                    .context("DOMStaticText has no DOMTextAttrs")?;
                let override_name = attrs.attr("url");
                let name = if override_name.is_empty() {
                    let face = attrs.attr("face");
                    let size = attrs.number("size", f64::NAN);
                    ensure!(
                        !face.is_empty() && size.is_finite() && size > 0.,
                        "DOMStaticText requires a face and positive font size"
                    );
                    format!("{}{}", face.to_uppercase(), (size * 0.8).round() as u64)
                } else {
                    override_name.into()
                };
                let mut matrix = matrix;
                // Native editable text uses a local x=0. Respect an explicit
                // origin instead of inferring one from stage or text width.
                matrix[4] -= element.number("x", 0.);
                out.push(Renderable {
                    matrix,
                    basis,
                    alpha,
                    point: element.point(),
                    image: String::new(),
                    frame: 0.,
                    locator: true,
                    text: run
                        .child("characters")
                        .map(|n| n.text.clone())
                        .unwrap_or_default(),
                    font: format!("FONT_{name}"),
                    base_symbol: String::new(),
                });
            }
            "DOMDynamicText" | "DOMInputText" => bail!(
                "Classic compilation does not support {} font/text layout; convert the text explicitly before export",
                element.tag
            ),
            _ => collect(p, element, basis, alpha, depth, nonrendering, out)?,
        }
    }
    Ok(())
}
fn decompose(m: [f64; 6]) -> [f64; 4] {
    let sx = m[0].hypot(m[1]);
    let sy = m[2].hypot(m[3]);
    [
        sx,
        sy,
        if sx > 1e-6 {
            m[1].atan2(m[0]).to_degrees()
        } else {
            0.
        },
        if sy > 1e-6 {
            -m[2].atan2(m[3]).to_degrees()
        } else {
            0.
        },
    ]
}
fn quantize(mut t: Transform) -> Transform {
    for i in [0, 1, 2, 3, 4, 5, 7] {
        let scale = if i < 4 { 10. } else { 1000. };
        if t.values[i] != MISSING {
            t.values[i] = ((t.values[i] as f64 * scale).round() / scale) as f32;
            if t.values[i] == -0. {
                t.values[i] = 0.;
            }
        }
    }
    t
}
fn build_transform(p: &Package, r: &Renderable, name: &str) -> Transform {
    let [sx, sy, kx, ky] = decompose(r.matrix);
    quantize(Transform {
        values: [
            r.matrix[4] as f32,
            r.matrix[5] as f32,
            kx as f32,
            ky as f32,
            sx as f32,
            sy as f32,
            cel_library_id(&r.image)
                .map(|(_, frame)| frame as f32)
                .unwrap_or(r.frame),
            r.alpha,
        ],
        image: if nonrendering_layer(name) || r.locator {
            String::new()
        } else {
            p.image(&r.image)
        },
        font: r.font.clone(),
        text: r.text.clone(),
    })
}
fn near(mut value: f64, previous: f64) -> f64 {
    while value > previous + 180. {
        value -= 360.;
    }
    while value < previous - 180. {
        value += 360.;
    }
    value
}
fn tween(start: &Renderable, end: &Renderable, a: &Transform, b: &Transform, t: f32) -> Transform {
    let lerp = |a: f64, b: f64| a + (b - a) * t as f64;
    let mut out = a.clone();
    // Flash samples the outer instance's tween before the JSFL exporter
    // unwraps its static symbol contents. Interpolating flattened endpoints
    // incorrectly straightens the arc of every offset child bitmap.
    let start_outer = start.basis.outer.unwrap_or(IDENTITY);
    let original_end_outer = end.basis.outer.unwrap_or(IDENTITY);
    let mut end_outer = original_end_outer;
    let mut end_point = end.point;
    if a.image == b.image
        && start.basis.inner != end.basis.inner
        && let Some(start_inverse) = inverse(start.basis.inner)
        && let Some(end_inverse) = inverse(end.basis.inner)
    {
        // A following key may wrap the same bitmap in a different symbol
        // (stock Polevaulter's "Tween 1"). Express that endpoint and its pivot
        // in the starting symbol's registration space before interpolating.
        end_outer = multiply(original_end_outer, multiply(end.basis.inner, start_inverse));
        end_point = point(multiply(start.basis.inner, end_inverse), end.point);
    }
    let ac = decompose(start_outer);
    let bc = decompose(end_outer);
    let sx = lerp(ac[0], bc[0]);
    let sy = lerp(ac[1], bc[1]);
    let kx = lerp(ac[2], near(bc[2], ac[2]));
    let ky = lerp(ac[3], near(bc[3], ac[3]));
    let a0 = sx * kx.to_radians().cos();
    let b0 = sx * kx.to_radians().sin();
    let c0 = -sy * ky.to_radians().sin();
    let d0 = sy * ky.to_radians().cos();
    let ca = point(start_outer, start.point);
    let cb = point(original_end_outer, end.point);
    let pivot = [
        lerp(start.point[0], end_point[0]),
        lerp(start.point[1], end_point[1]),
    ];
    let outer = [
        a0,
        b0,
        c0,
        d0,
        lerp(ca[0], cb[0]) - a0 * pivot[0] - c0 * pivot[1],
        lerp(ca[1], cb[1]) - b0 * pivot[0] - d0 * pivot[1],
    ];
    let mut composed = multiply(outer, start.basis.inner);
    composed[4] += lerp(
        start.matrix[4] - start.basis.matrix()[4],
        end.matrix[4] - end.basis.matrix()[4],
    );
    let [sx, sy, kx, ky] = decompose(composed);
    out.values[0] = composed[4] as f32;
    out.values[1] = composed[5] as f32;
    out.values[2] = kx as f32;
    out.values[3] = ky as f32;
    out.values[4] = sx as f32;
    out.values[5] = sy as f32;
    out.values[7] = a.values[7] + (b.values[7] - a.values[7]) * t;
    quantize(out)
}
fn normalize_angles(track: &mut Track) {
    for index in [2, 3] {
        let mut previous = None;
        for t in &mut track.transforms {
            let v = t.values[index];
            if v == MISSING {
                continue;
            }
            let value = previous.map(|p| near(v as f64, p)).unwrap_or(v as f64);
            t.values[index] = value as f32;
            previous = Some(value);
        }
    }
}
fn library_keys(value: &str) -> Vec<String> {
    let mut key = value.trim().replace('\\', "/");
    if key.to_ascii_lowercase().starts_with("library/") {
        key = key[8..].into();
    }
    if key.to_ascii_lowercase().ends_with(".xml") {
        key.truncate(key.len() - 4);
    }
    let mut result = vec![key.to_ascii_lowercase()];
    if let Some(bare) = key.rsplit('/').next() {
        result.push(bare.to_ascii_lowercase());
    }
    result
}
fn basename(value: &str) -> String {
    let value = value.replace('\\', "/");
    let name = value.rsplit('/').next().unwrap_or("");
    name.rsplit_once('.').map(|(a, _)| a).unwrap_or(name).into()
}
fn path_key(value: &str) -> String {
    let value = value.trim().replace('\\', "/");
    let value = if value.to_ascii_lowercase().starts_with("library/") {
        &value[8..]
    } else {
        &value
    };
    value
        .rsplit_once('.')
        .map(|(v, _)| v)
        .unwrap_or(value)
        .to_uppercase()
}
fn packed_id(value: &str) -> String {
    if let Some((id, _)) = cel_library_id(value) {
        return id;
    }
    let lower = value.to_ascii_lowercase();
    for ext in [".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp"] {
        if lower.ends_with(ext) {
            return value[..value.len() - ext.len()].into();
        }
    }
    value.into()
}
fn cel_library_id(value: &str) -> Option<(String, u32)> {
    let stem = basename(value);
    let (id, frame) = stem.rsplit_once("__REANIM_CEL_")?;
    if !id.to_ascii_uppercase().starts_with("IMAGE_") {
        return None;
    }
    Some((id.to_owned(), frame.parse().ok()?))
}
fn image_id(value: &str) -> String {
    let bare = value.replace('\\', "/");
    let bare = bare.rsplit('/').next().unwrap_or("");
    if bare.is_empty() {
        String::new()
    } else if bare.to_ascii_uppercase().starts_with("IMAGE_") {
        packed_id(bare)
    } else {
        // JSFL uppercases the bitmap library name verbatim; punctuation,
        // repeated underscores and spaces can be part of a real resource ID.
        format!("IMAGE_REANIM_{}", basename(bare).to_uppercase())
    }
}
struct Catalog {
    ids: HashSet<String>,
    paths: HashMap<String, String>,
}
impl Catalog {
    fn load_for(input: &Path, explicit: Option<&Path>) -> Result<Option<Self>> {
        let root = explicit.map(Path::to_path_buf).or_else(|| {
            input.ancestors().find_map(|p| {
                if p.join("resources.xml").is_file() {
                    Some(p.to_path_buf())
                } else if p.join("Resources/resources.xml").is_file() {
                    Some(p.join("Resources"))
                } else {
                    None
                }
            })
        });
        let Some(root) = root else {
            return Ok(None);
        };
        let mut result = Self {
            ids: HashSet::new(),
            paths: HashMap::new(),
        };
        if root.join("resources.xml").is_file() {
            let source = fs::read_to_string(root.join("resources.xml"))?;
            let document = roxmltree::Document::parse(&source)?;
            let mut folder = String::new();
            let mut prefix = String::new();
            for node in document.descendants().filter(|n| n.is_element()) {
                match node.tag_name().name() {
                    "SetDefaults" => {
                        if let Some(p) = node.attribute("path") {
                            folder = p.trim().trim_end_matches(['/', '\\']).into();
                        }
                        if let Some(p) = node.attribute("idprefix") {
                            prefix = p.trim().into();
                        }
                    }
                    "Image" => {
                        let path = node.attribute("path").unwrap_or("");
                        if path.is_empty() {
                            continue;
                        }
                        let id = format!(
                            "{}{}",
                            prefix,
                            node.attribute("id")
                                .filter(|s| !s.is_empty())
                                .map(String::from)
                                .unwrap_or_else(|| basename(path))
                        );
                        result.ids.insert(id.to_uppercase());
                        for value in [format!("{folder}/{path}"), path.into()] {
                            let key = path_key(&value);
                            if key.contains('/') {
                                result.paths.entry(key).or_insert_with(|| id.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // Only the engine's reanim/ basename fallback proves that a raw
        // IMAGE_REANIM_ ID exists. images/ can contain a different picture
        // with exactly the same basename (stock TreeFood).
        for entry in walkdir::WalkDir::new(root.join("reanim"))
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let extension = entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !["png", "jpg", "jpeg", "bmp", "gif", "webp"].contains(&extension.as_str()) {
                continue;
            }
            let stem = entry
                .path()
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_uppercase();
            result.ids.insert(format!("IMAGE_REANIM_{stem}"));
        }
        Ok(Some(result))
    }
    fn resolve(&self, value: &str) -> String {
        let candidate = image_id(value);
        if self.ids.contains(&candidate.to_uppercase()) {
            return candidate;
        }
        // A basename is not proof of resource identity. Preserve JSFL's raw
        // ID unless a complete library path explicitly names a manifest entry
        // and that raw ID has neither a declaration nor a reanim/ fallback.
        let key = path_key(value);
        if key.contains('/') {
            self.paths.get(&key).cloned().unwrap_or(candidate)
        } else {
            candidate
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, path::PathBuf};
    fn fla(xml: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.fla");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        zip.start_file("DOMDocument.xml", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        zip.finish().unwrap();
        (dir, path)
    }
    #[test]
    fn labels_do_not_create_tracks_and_tween_keeps_alpha_and_id() {
        let (_dir, path) = fla(
            r#"<DOMDocument frameRate="10"><timelines><DOMTimeline><layers><DOMLayer name="art"><frames><DOMFrame index="0" duration="2" tweenType="motion"><elements><DOMBitmapInstance libraryItemName="IMAGE_REANIM_HEAD__A"><matrix><Matrix tx="0"/></matrix><color><Color alphaMultiplier="0.2"/></color></DOMBitmapInstance></elements></DOMFrame><DOMFrame index="2" duration="1"><elements><DOMBitmapInstance libraryItemName="IMAGE_REANIM_HEAD__A"><matrix><Matrix tx="10"/></matrix><color><Color alphaMultiplier="0.8"/></color></DOMBitmapInstance></elements></DOMFrame></frames></DOMLayer><DOMLayer name="labels"><frames><DOMFrame index="0" duration="3" name="anim_idle"/></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#,
        );
        let d = convert(&path).unwrap();
        assert_eq!(d.tracks[0].name, "labels");
        assert_eq!(d.tracks[0].transforms[0].values[6], -1.);
        assert_eq!(d.tracks.len(), 2);
        let t = &d.tracks[1];
        assert_eq!(t.transforms[1].values[0], 5.);
        assert_eq!(t.transforms[1].values[7], 0.5);
        assert_eq!(t.transforms[1].image, "IMAGE_REANIM_HEAD__A");
    }
    #[test]
    fn locator_without_library_survives() {
        let (_dir, path) = fla(
            r#"<DOMDocument><timelines><DOMTimeline><layers><DOMLayer name="locator_hand"><frames><DOMFrame index="0" duration="1"><elements><DOMSymbolInstance libraryItemName="locator"><matrix><Matrix tx="42" ty="7"/></matrix></DOMSymbolInstance></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#,
        );
        let d = convert(&path).unwrap();
        assert_eq!(d.tracks[0].transforms[0].values[0], 42.);
        assert!(d.tracks[0].transforms[0].image.is_empty());
    }
    #[test]
    fn animate_size_overcount_is_repaired() {
        let (_dir, path) = fla("<DOMDocument/>");
        let mut bytes = fs::read(&path).unwrap();
        let end = bytes.len() - 22;
        let size = u32_at(&bytes, end + 12).unwrap();
        bytes[end + 12..end + 16].copy_from_slice(&((size + 54) as u32).to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(read_package(&path).unwrap().contains_key("DOMDocument.xml"));
    }
    #[test]
    fn malformed_directory_is_rejected() {
        let (_dir, path) = fla("<DOMDocument/>");
        let mut bytes = fs::read(&path).unwrap();
        let end = bytes.len() - 22;
        bytes[end + 16..end + 20].copy_from_slice(&0u32.to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(read_package(&path).is_err());
    }
    #[test]
    fn visible_vectors_require_plus_but_helper_geometry_does_not() {
        let xml = |layer: &str| {
            format!(
                r#"<DOMDocument><timelines><DOMTimeline><layers><DOMLayer name="{layer}"><frames><DOMFrame index="0"><elements><DOMShape/></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#
            )
        };
        let (_dir, path) = fla(&xml("art"));
        assert!(needs_plus(&path).unwrap());
        assert!(convert(&path).is_err());
        let (_dir, path) = fla(&xml("_guide"));
        assert!(!needs_plus(&path).unwrap());
        assert!(convert(&path).is_err()); // no exported layers remain
    }
    #[test]
    fn nested_vector_detection_and_locator_authoring_art() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("LIBRARY")).unwrap();
        fs::write(directory.path().join("DOMDocument.xml"), r#"<DOMDocument><timelines><DOMTimeline><layers><DOMLayer name="body"><frames><DOMFrame index="0"><elements><DOMSymbolInstance libraryItemName="art"/></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#).unwrap();
        fs::write(directory.path().join("LIBRARY/art.xml"), r#"<DOMSymbolItem name="art"><timeline><DOMTimeline><layers><DOMLayer name="vector"><frames><DOMFrame index="0"><elements><DOMShape/></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timeline></DOMSymbolItem>"#).unwrap();
        assert!(needs_plus(directory.path()).unwrap());
        let document = fs::read_to_string(directory.path().join("DOMDocument.xml"))
            .unwrap()
            .replace("libraryItemName=\"art\"", "libraryItemName=\"locator\"");
        fs::write(directory.path().join("DOMDocument.xml"), document).unwrap();
        let symbol = fs::read_to_string(directory.path().join("LIBRARY/art.xml"))
            .unwrap()
            .replace("name=\"art\"", "name=\"locator\"");
        fs::write(directory.path().join("LIBRARY/locator.xml"), symbol).unwrap();
        assert!(!needs_plus(directory.path()).unwrap());
        assert!(convert(directory.path()).is_ok());
    }
    #[test]
    fn packed_resource_id_keeps_repeated_underscores() {
        assert_eq!(image_id("IMAGE_REANIM_HEAD__A.png"), "IMAGE_REANIM_HEAD__A");
        assert_eq!(
            image_id("Folder/Pea Shooter.png"),
            "IMAGE_REANIM_PEA SHOOTER"
        );
    }
    #[test]
    fn classic_motion_samples_acceleration_and_legacy_easing() {
        for (attributes, expected) in [
            (r#"acceleration="-100""#, [0., 4.4, 7.5, 9.4, 10.]),
            (r#"acceleration="100""#, [0., 0.6, 2.5, 5.6, 10.]),
            (r#"tweenEasing="100""#, [0., 4.4, 7.5, 9.4, 10.]),
            (r#"tweenEasing="-100""#, [0., 0.6, 2.5, 5.6, 10.]),
            (
                r#"acceleration="-100" tweenEasing="-100""#,
                [0., 4.4, 7.5, 9.4, 10.],
            ),
            (
                r#"acceleration="0" tweenEasing="100""#,
                [0., 2.5, 5., 7.5, 10.],
            ),
        ] {
            let (_dir, path) = fla(&format!(
                r#"<DOMDocument><timelines><DOMTimeline><layers><DOMLayer name="art"><frames><DOMFrame index="0" duration="4" tweenType="motion" {attributes}><elements><DOMBitmapInstance libraryItemName="TEST"><matrix><Matrix tx="0"/></matrix></DOMBitmapInstance></elements></DOMFrame><DOMFrame index="4" duration="1"><elements><DOMBitmapInstance libraryItemName="TEST"><matrix><Matrix tx="10"/></matrix></DOMBitmapInstance></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#
            ));
            let definition = convert(&path).unwrap();
            let track = &definition.tracks[0];
            assert_eq!(
                track
                    .transforms
                    .iter()
                    .map(|t| t.values[0])
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }
    #[test]
    fn sprite_cel_bitmap_restores_original_id_and_frame() {
        let (_dir, path) = fla(
            r#"<DOMDocument><timelines><DOMTimeline><layers><DOMLayer name="sprite"><frames><DOMFrame index="0"><elements><DOMBitmapInstance libraryItemName="IMAGE_REANIM_BOSS__PART__REANIM_CEL_3.png"/></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#,
        );
        let d = convert(&path).unwrap();
        let frame = &d.tracks[0].transforms[0];
        assert_eq!(frame.image, "IMAGE_REANIM_BOSS__PART");
        assert_eq!(frame.values[6], 3.);
    }
}
