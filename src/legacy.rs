//! Flash binary FLA reader. The CFB streams are decoded as MFC objects, then
//! passed directly to both compilers without XML generation or reparsing.
//! Wire-layout reference: https://www.phoenixanimate.com/binary-fla-format.html
use crate::document::{Element, Source};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, HashMap},
    io::{Cursor, Read},
    rc::Rc,
};

const LIMIT: usize = 128 * 1024 * 1024;
type Matrix = [f64; 6];

fn matrix_node(m: Matrix) -> Element {
    let mut node = Element::new("Matrix");
    for (key, value) in ["a", "b", "c", "d", "tx", "ty"].into_iter().zip(m) {
        node.attrs.insert(key.into(), value.to_string());
    }
    Element::wrap("matrix", vec![node])
}

#[derive(Clone, Default)]
struct Item {
    stream: String,
    name: String,
    number: u32,
    kind: u8,
}
#[derive(Clone, Default)]
struct Object {
    node: Element,
    item: Option<Item>,
}
enum Slot {
    Empty,
    Class(String),
    Pending,
    Object(Rc<Object>),
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    version: u16,
    slots: Vec<Slot>,
}
impl<'a> Reader<'a> {
    fn new(data: &'a [u8], version: u16) -> Self {
        Self {
            data,
            pos: 0,
            version,
            slots: vec![Slot::Empty],
        }
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .context("Binary FLA length overflow")?;
        let bytes = self
            .data
            .get(self.pos..end)
            .with_context(|| format!("Truncated binary FLA at byte {} (need {len})", self.pos))?;
        self.pos = end;
        Ok(bytes)
    }
    fn skip(&mut self, len: usize) -> Result<()> {
        self.take(len)?;
        Ok(())
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn count(&mut self) -> Result<usize> {
        let n = self.u32()? as usize;
        ensure!(n <= 1_000_000, "Excessive binary FLA array");
        Ok(n)
    }
    fn string_len(&mut self) -> Result<(bool, usize)> {
        let n = self.u8()?;
        if n != 255 {
            return Ok((false, n as usize));
        }
        let n = self.u16()?;
        if n == 0xfffe {
            let (_, len) = self.string_len()?;
            return Ok((true, len));
        }
        Ok((
            false,
            if n == 0xffff {
                self.count()?
            } else {
                n as usize
            },
        ))
    }
    fn chars(&mut self, unicode: bool, len: usize) -> Result<String> {
        ensure!(len <= 1_000_000, "Excessive binary FLA string");
        let text = if unicode {
            let words = self
                .take(len * 2)?
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>();
            String::from_utf16(&words).context("Invalid UTF-16 in binary FLA")?
        } else {
            encoding_rs::WINDOWS_1252
                .decode(self.take(len)?)
                .0
                .into_owned()
        };
        Ok(text.trim_end_matches('\0').to_string())
    }
    fn string(&mut self) -> Result<String> {
        let (u, n) = self.string_len()?;
        self.chars(u, n)
    }
    fn stream_name(&mut self, unicode: bool) -> Result<String> {
        let n = self.u8()? as usize;
        self.chars(unicode, n)
    }
    fn matrix(&mut self) -> Result<Matrix> {
        let mut m = [0.; 6];
        for (i, value) in m.iter_mut().enumerate() {
            *value = self.i32()? as f64 / if i < 4 { 65536. } else { 20. };
        }
        Ok(m)
    }
    fn color(&mut self) -> Result<Element> {
        let b = self.take(4)?;
        Ok(Element::new("SolidColor")
            .with_attr("color", format!("#{:02X}{:02X}{:02X}", b[0], b[1], b[2]))
            .with_attr("alpha", b[3] as f64 / 255.))
    }
    fn object(&mut self, depth: usize) -> Result<Option<Rc<Object>>> {
        ensure!(
            depth <= 64 && self.slots.len() < 1_000_000,
            "Excessive/cyclic binary FLA objects"
        );
        let offset = self.pos;
        let tag = self.u16()?;
        if tag == 0 {
            return Ok(None);
        }
        let class = if tag == 0xffff {
            let _schema = self.u16()?;
            let len = self.u16()? as usize;
            ensure!(len <= 256, "Invalid binary FLA class name");
            let class = std::str::from_utf8(self.take(len)?)?.to_string();
            self.slots.push(Slot::Class(class.clone()));
            class
        } else {
            let (index, new_object) = if tag == 0x7fff {
                let n = self.u32()?;
                ((n & 0x7fff_ffff) as usize, n & 0x8000_0000 != 0)
            } else {
                ((tag & 0x7fff) as usize, tag & 0x8000 != 0)
            };
            match (self.slots.get(index), new_object) {
                (Some(Slot::Class(name)), true) => name.clone(),
                (Some(Slot::Object(object)), false) => return Ok(Some(object.clone())),
                _ => bail!("Invalid MFC reference {tag:#x} at byte {offset}"),
            }
        };
        let id = self.slots.len();
        self.slots.push(Slot::Pending);
        let mut object = self
            .body(&class, depth + 1)
            .with_context(|| format!("{class} at byte {offset}"))?;
        object.node.attrs.insert("_id".into(), id.to_string());
        let object = Rc::new(object);
        self.slots[id] = Slot::Object(object.clone());
        Ok(Some(object))
    }
    fn base(&mut self, depth: usize) -> Result<(Vec<Element>, Option<[f64; 2]>, u8)> {
        let schema = self.u8()?;
        let flags = self.u8()?;
        ensure!(schema <= 5, "Unsupported CPicObj schema {schema}");
        let mut children = Vec::new();
        while let Some(child) = self.object(depth)? {
            children.push(child.node.clone());
        }
        let point = if schema >= 1 {
            let p = [self.i32()?, self.i32()?];
            if p.contains(&i32::MIN) {
                None
            } else {
                Some([p[0] as f64 / 20., p[1] as f64 / 20.])
            }
        } else {
            None
        };
        if schema >= 4 {
            self.skip(2)?;
        }
        Ok((children, point, flags))
    }
    fn transform(&self, node: &mut Element, m: Matrix, point: Option<[f64; 2]>) {
        node.push(matrix_node(m));
        if let Some(p) = point {
            let det = m[0] * m[3] - m[1] * m[2];
            if det.abs() > 1e-12 {
                let (x, y) = (p[0] - m[4], p[1] - m[5]);
                node.push(Element::wrap(
                    "transformationPoint",
                    vec![
                        Element::new("Point")
                            .with_attr("x", (m[3] * x - m[2] * y) / det)
                            .with_attr("y", (m[0] * y - m[1] * x) / det),
                    ],
                ));
            }
        }
    }
    fn body(&mut self, class: &str, depth: usize) -> Result<Object> {
        if class == "CDocumentPage" {
            return self.document_item();
        }
        if class.starts_with("CMedia") {
            return self.media_item(class);
        }
        if class == "CObList" {
            let n = self.u16()?;
            let n = if n == 0xffff {
                self.count()?
            } else {
                n as usize
            };
            for _ in 0..n {
                self.object(depth)?;
            }
            return Ok(Object::default());
        }
        if class == "CMorphHintItem" {
            self.skip(20)?;
            return Ok(Object::default());
        }
        ensure!(
            class.starts_with("CPic"),
            "Unsupported binary FLA object {class}"
        );
        let (children, point, flags) = self.base(depth)?;
        let mut node = match class {
            "CPicPage" => {
                let schema = self.u8()?;
                if schema != 4 {
                    self.skip(2)?;
                }
                if schema >= 5 {
                    self.skip(2)?;
                }
                if schema >= 7 {
                    self.skip(4)?;
                }
                if schema >= 3 {
                    let n = self.count()?;
                    self.skip(n * 8)?;
                }
                let mut layers = children;
                layers.reverse();
                let positions = layers
                    .iter()
                    .enumerate()
                    .filter_map(|(i, l)| Some((l.attrs.get("_id")?.clone(), i)))
                    .collect::<HashMap<_, _>>();
                for layer in &mut layers {
                    if let Some(parent) = layer.attrs.get("_parent") {
                        let index = positions
                            .get(parent)
                            .context("Binary FLA parent layer is absent from its timeline")?;
                        layer
                            .attrs
                            .insert("parentLayerIndex".into(), index.to_string());
                    }
                }
                Element::wrap("DOMTimeline", vec![Element::wrap("layers", layers)])
            }
            "CPicLayer" => {
                let schema = self.u8()?;
                ensure!(
                    (9..=13).contains(&schema),
                    "Unsupported binary FLA layer schema {schema}"
                );
                let name = self.string()?;
                self.skip(1)?;
                let hidden = self.u8()? != 0;
                let locked = self.u8()? != 0;
                self.skip(16)?;
                let kind = self.u8()?;
                let parent = self.object(depth)?;
                self.skip(1)?;
                let auto = self.u8()? != 0;
                if schema >= 13 {
                    self.skip(1)?;
                }
                let mut index = 0usize;
                let mut frames = Vec::new();
                for mut frame in children {
                    ensure!(
                        frame.name == "DOMFrame",
                        "Unexpected binary FLA layer child"
                    );
                    let duration: usize = frame
                        .attrs
                        .get("duration")
                        .context("Missing frame duration")?
                        .parse()?;
                    frame.attrs.insert("index".into(), index.to_string());
                    index += duration;
                    ensure!(index <= 20_000, "Binary FLA exceeds 20000-frame limit");
                    frames.push(frame);
                }
                let mut layer = Element::new("DOMLayer")
                    .with_attr("name", name)
                    .with_attr("visible", !hidden)
                    .with_attr("locked", locked)
                    .with_attr("autoNamed", auto);
                if let Some(parent) = parent {
                    layer
                        .attrs
                        .insert("_parent".into(), parent.node.attrs["_id"].clone());
                }
                match kind {
                    0 => {}
                    1 => {
                        layer.attrs.insert("layerType".into(), "guide".into());
                    }
                    3 => {
                        layer.attrs.insert("layerType".into(), "folder".into());
                    }
                    _ => bail!("Unsupported binary FLA layer type {kind}"),
                }
                layer.push(Element::wrap("frames", frames));
                layer
            }
            "CPicFrame" => {
                let shape = self.shape()?;
                let schema = self.u8()?;
                ensure!(
                    (21..=26).contains(&schema),
                    "Unsupported binary FLA frame schema {schema}"
                );
                let duration = self.u16()?;
                ensure!(duration > 0, "Zero binary FLA frame duration");
                let key_mode = self.u16()?;
                let ease = self.i16()?;
                let sound = self.u16()?;
                ensure!(
                    sound == 0,
                    "Binary FLA sound frames cannot be exported as reanim"
                );
                let n = self.u16()? as usize;
                self.skip(n * 8 + 13)?;
                let label = self.string()?;
                self.actions()?;
                let rotate = self.u32()?;
                let turns = self.u32()?;
                self.skip(4)?;
                ensure!(
                    self.object(depth)?.is_none(),
                    "Binary FLA shape tweens are unsupported"
                );
                self.skip(4)?;
                self.object(depth)?;
                self.string()?;
                self.skip(if schema >= 22 { 12 } else { 8 })?;
                if schema >= 24 {
                    self.skip(4)?;
                    ensure!(
                        self.u32()? == 0,
                        "Binary FLA custom easing curves are unsupported"
                    );
                }
                let mut elements = Vec::new();
                if !shape.children.is_empty() {
                    elements.push(shape);
                }
                elements.extend(children);
                let mut frame = Element::new("DOMFrame")
                    .with_attr("duration", duration)
                    .with_attr(
                        "keyMode",
                        key_mode | if key_mode & 1 != 0 { 0x4000 } else { 0x2000 },
                    );
                if key_mode & 1 != 0 {
                    frame.attrs.insert("tweenType".into(), "motion".into());
                    frame.attrs.insert("acceleration".into(), ease.to_string());
                    if rotate != 0 {
                        frame.attrs.insert(
                            "motionTweenRotate".into(),
                            if rotate == 1 {
                                "clockwise"
                            } else {
                                "counter-clockwise"
                            }
                            .into(),
                        );
                    }
                    frame
                        .attrs
                        .insert("motionTweenRotateTimes".into(), turns.to_string());
                }
                if !label.is_empty() {
                    frame.attrs.insert("name".into(), label);
                }
                frame.push(Element::wrap("elements", elements));
                frame
            }
            "CPicShape" => {
                let shape = self.shape()?;
                if flags & 1 != 0 {
                    ensure!(children.is_empty(), "Loose binary FLA shape has children");
                    shape
                } else {
                    let mut members = Vec::new();
                    if !shape.children.is_empty() {
                        members.push(shape);
                    }
                    members.extend(children);
                    Element::wrap("DOMGroup", vec![Element::wrap("members", members)])
                }
            }
            "CPicSymbol" | "CPicSprite" | "CPicButton" => {
                ensure!(children.is_empty(), "Binary FLA instance has children");
                let schema = self.u8()?;
                ensure!(
                    (10..=22).contains(&schema),
                    "Unsupported binary FLA instance schema {schema}"
                );
                let m = self.matrix()?;
                let first = self.u16()?;
                let looping = self.u16()?;
                self.skip(1)?;
                let mut mult = [0.; 4];
                let mut add = [0i16; 4];
                for i in 0..4 {
                    mult[i] = self.u16()? as f64 / 256.;
                    add[i] = self.i16()?;
                }
                self.skip(8)?;
                self.string()?;
                let reference = self.u32()?;
                if schema >= 22 {
                    self.skip(7)?;
                    let raw = self.take(64)?;
                    for (i, b) in raw.chunks_exact(4).enumerate() {
                        let f = f32::from_le_bytes(b.try_into()?);
                        ensure!(
                            f == if i % 5 == 0 { 1. } else { 0. },
                            "3D binary FLA transforms cannot be exported as reanim"
                        );
                    }
                    self.skip(38)?;
                } else if schema >= 19 {
                    self.skip(7)?;
                } else if schema >= 14 {
                    self.skip(3)?;
                }
                let mut instance = Element::new("DOMSymbolInstance")
                    .with_attr("_symbol", reference)
                    .with_attr("firstFrame", first)
                    .with_attr(
                        "loop",
                        match looping {
                            0 => "loop",
                            1 => "play once",
                            2 => "single frame",
                            _ => bail!("Invalid binary FLA loop mode"),
                        },
                    )
                    .with_attr(
                        "symbolType",
                        match class {
                            "CPicSprite" => "movie clip",
                            "CPicButton" => "button",
                            _ => "graphic",
                        },
                    );
                self.transform(&mut instance, m, point);
                if mult != [1.; 4] || add != [0; 4] {
                    let mut color = Element::new("Color");
                    for (i, key) in ["alpha", "red", "green", "blue"].iter().enumerate() {
                        color
                            .attrs
                            .insert(format!("{key}Multiplier"), mult[i].to_string());
                        color
                            .attrs
                            .insert(format!("{key}Offset"), add[i].to_string());
                    }
                    instance.push(Element::wrap("color", vec![color]));
                }
                if class == "CPicSprite" {
                    let s = self.u8()?;
                    ensure!(
                        (2..=8).contains(&s),
                        "Unsupported binary FLA sprite schema {s}"
                    );
                    self.actions()?;
                    let name = self.string()?;
                    instance.attrs.insert("name".into(), name);
                    self.skip(5)?;
                    if s >= 4 {
                        self.skip(12)?;
                    }
                    if s >= 8 {
                        self.skip(5)?;
                        self.string()?;
                    }
                } else if class == "CPicButton" {
                    let s = self.u8()?;
                    ensure!(
                        s >= 5 && s <= 11,
                        "Unsupported binary FLA button schema {s}"
                    );
                    self.actions()?;
                    self.skip(1)?;
                    let name = self.string()?;
                    instance.attrs.insert("name".into(), name);
                    self.skip(4)?;
                }
                instance
            }
            "CPicBitmap" => {
                let schema = self.u8()?;
                ensure!(
                    (1..=2).contains(&schema),
                    "Unsupported binary FLA bitmap schema {schema}"
                );
                let m = self.matrix()?;
                let reference = self.u16()?;
                if schema >= 2 {
                    ensure!(self.u8()? == 0, "Binary FLA bitmap filters are unsupported");
                }
                let mut bitmap = Element::new("DOMBitmapInstance").with_attr("_media", reference);
                self.transform(&mut bitmap, m, point);
                bitmap
            }
            _ => bail!("Unsupported binary FLA timeline object {class}"),
        };
        if node.name == "DOMShape" && node.children.is_empty() {
            node.name = "DOMGroup".into();
        }
        Ok(Object { node, item: None })
    }
    fn actions(&mut self) -> Result<()> {
        let version = self.u32()?;
        let kind = self.u32()?;
        ensure!(
            version <= 5,
            "Unsupported binary FLA action version {version}"
        );
        if version != 0 {
            self.skip(4)?;
            let n = self.count()?;
            if version >= 5 {
                self.skip(n * 4 + 4)?;
                self.string()?;
                return Ok(());
            }
            ensure!(n == 0, "Unsupported binary FLA action table");
        }
        if version == 4 || kind == 1 {
            self.string()?;
        } else if kind == 0 {
            self.skip(4)?;
            ensure!(
                self.u32()? == 0,
                "Binary FLA normal-mode ActionScript is unsupported"
            );
        } else {
            ensure!(kind == 2, "Unsupported binary FLA action kind {kind}");
        }
        Ok(())
    }
    fn fill(&mut self, shape_schema: u8) -> Result<Element> {
        let color = self.color()?;
        let kind = self.u16()? & 0xff;
        match kind {
            0 => Ok(color),
            0x10 | 0x12 | 0x13 => {
                let m = self.matrix()?;
                let n = self.u8()?;
                if shape_schema >= 3 {
                    self.skip(8)?;
                }
                let mut gradient = Element::new(if kind == 0x10 {
                    "LinearGradient"
                } else {
                    "RadialGradient"
                });
                gradient.push(matrix_node(m));
                for _ in 0..n {
                    let ratio = self.u8()? as f64 / 255.;
                    let c = self.color()?;
                    gradient.push(
                        Element {
                            name: "GradientEntry".into(),
                            attrs: c.attrs,
                            children: Vec::new(),
                            text: String::new(),
                        }
                        .with_attr("ratio", ratio),
                    );
                }
                Ok(gradient)
            }
            0x40..=0x43 => {
                let m = self.matrix()?;
                let id = self.u16()?;
                let mut fill = Element::new("BitmapFill")
                    .with_attr("_bitmap", id)
                    .with_attr("bitmapIsClipped", kind & 1 != 0);
                fill.push(matrix_node(m));
                Ok(fill)
            }
            _ => bail!("Unsupported binary FLA fill type {kind:#x}"),
        }
    }
    fn delta(&mut self, kind: u8) -> Result<[i64; 2]> {
        Ok(match kind {
            0 => [0, 0],
            1 => [self.i16()? as i64, self.i16()? as i64],
            2 => [self.i32()? as i64, self.i32()? as i64],
            3 => [(self.i16()? as i64) << 7, (self.i16()? as i64) << 7],
            _ => unreachable!(),
        })
    }
    fn shape(&mut self) -> Result<Element> {
        let schema = self.u8()?;
        ensure!(
            (1..=6).contains(&schema),
            "Unsupported binary FLA shape schema {schema}"
        );
        let m = self.matrix()?;
        let data_schema = self.u8()?;
        ensure!(
            (2..=5).contains(&data_schema),
            "Unsupported binary FLA shape data schema {data_schema}"
        );
        self.skip(4)?;
        let n = self.u16()?;
        let mut fills = Vec::new();
        for i in 1..=n {
            fills.push(Element::wrap("FillStyle", vec![self.fill(schema)?]).with_attr("index", i));
        }
        let n = self.u16()?;
        let mut strokes = Vec::new();
        for i in 1..=n {
            let color = self.color()?;
            let weight = self.u16()? as f64 / 20.;
            self.skip(4)?;
            let fill = if schema >= 3 {
                self.skip(6)?;
                self.fill(schema)?
            } else {
                color
            };
            strokes.push(
                Element::wrap(
                    "StrokeStyle",
                    vec![
                        Element::wrap("SolidStroke", vec![Element::wrap("fill", vec![fill])])
                            .with_attr("weight", weight),
                    ],
                )
                .with_attr("index", i),
            );
        }
        let mut edges = Vec::new();
        let mut previous = [0i64; 2];
        let mut styles = [0u16; 3];
        loop {
            let f = self.u8()?;
            if f == 0 {
                break;
            }
            ensure!(edges.len() < 1_000_000, "Excessive binary FLA edges");
            if f & 0x40 != 0 {
                for style in &mut styles {
                    *style = if f & 0x80 != 0 {
                        self.u8()? as u16
                    } else {
                        self.u16()? & 0x7fff
                    };
                }
            }
            let d1 = self.delta(f & 3)?;
            let d2 = self.delta((f >> 2) & 3)?;
            let d3 = self.delta((f >> 4) & 3)?;
            let from = [previous[0] + d1[0], previous[1] + d1[1]];
            let ctrl = [from[0] + d2[0], from[1] + d2[1]];
            let to = [from[0] + d3[0], from[1] + d3[1]];
            let p = |p: [i64; 2]| format!("{} {}", p[0] as f64 / 256., p[1] as f64 / 256.);
            let path = if f & 0x0c == 0 {
                if schema >= 6 {
                    self.skip(1)?;
                }
                format!("!{}|{}", p(from), p(to))
            } else {
                format!("!{}[{} {}", p(from), p(ctrl), p(to))
            };
            edges.push(
                Element::new("Edge")
                    .with_attr("strokeStyle", styles[0])
                    .with_attr("fillStyle0", styles[1])
                    .with_attr("fillStyle1", styles[2])
                    .with_attr("edges", path),
            );
            previous = to;
        }
        if data_schema >= 5 {
            let n = self.count()?;
            for _ in 0..n {
                self.skip(32)?;
                if schema >= 6 {
                    let n = self.u8()? as usize;
                    self.skip(n * 10)?;
                    let more = self.u8()?;
                    self.skip(((more & 1 != 0) as usize + (more & 2 != 0) as usize) * 8)?;
                }
            }
        }
        let mut shape = Element::new("DOMShape");
        if !edges.is_empty() {
            shape.push(matrix_node(m));
            shape.push(Element::wrap("fills", fills));
            shape.push(Element::wrap("strokes", strokes));
            shape.push(Element::wrap("edges", edges));
        }
        Ok(shape)
    }
    fn linkage(&mut self) -> Result<()> {
        let ver = self.u32()? & 0xff;
        ensure!(
            (3..=7).contains(&ver),
            "Unsupported binary FLA linkage {ver}"
        );
        self.skip(1)?;
        self.string()?;
        self.string()?;
        if ver >= 5 {
            self.string()?;
        }
        self.skip(5)
    }
    fn source(&mut self) -> Result<()> {
        self.string()?;
        self.string()?;
        self.skip(20)
    }
    fn item_header(&mut self) -> Result<()> {
        let ver = self.u32()?;
        ensure!(
            (4..=6).contains(&ver),
            "Unsupported binary FLA library header {ver}"
        );
        self.skip(28)?;
        if ver >= 5 {
            if self.version & 0xff >= 0x29 {
                self.linkage()?;
                self.source()?;
            } else {
                self.skip(5)?;
                self.string()?;
                self.string()?;
            }
        }
        if ver >= 6 {
            self.skip(1)?;
            if self.version & 0xff >= 0x3f {
                self.string()?;
                self.skip(1)?;
            }
        }
        Ok(())
    }
    fn document_item(&mut self) -> Result<Object> {
        let schema = self.u8()?;
        ensure!(
            [17, 22, 23, 24, 25].contains(&schema),
            "Unsupported binary FLA library schema {schema}"
        );
        let stream = self.stream_name(schema >= 22)?;
        let name = self.string()?;
        let number = self.u32()?;
        let kind = self.u8()?;
        self.string()?;
        self.skip(4)?;
        self.item_header()?;
        self.skip(4)?;
        self.string()?;
        self.string()?;
        self.skip(13)?;
        self.linkage()?;
        self.source()?;
        if schema == 17 {
            self.skip(34)?;
        } else {
            if schema >= 23 {
                self.skip(1)?;
                self.string()?;
            }
            self.skip(5)?;
            self.string()?;
            self.skip(12)?;
            self.string()?;
            for _ in 0..3 {
                self.skip(1)?;
                self.string()?;
                self.skip(8)?;
            }
            self.string()?;
            self.skip(9)?;
            self.string()?;
            if schema >= 24 {
                self.skip(16)?;
                self.string()?;
            }
            self.skip(if schema == 23 { 12 } else { 8 })?;
            self.string()?;
            self.string()?;
            self.skip(4)?;
            self.string()?;
            if schema >= 23 {
                self.skip(20)?;
                if schema >= 24 {
                    self.skip(if schema == 25 { 11 } else { 9 })?;
                }
            }
        }
        Ok(Object {
            item: Some(Item {
                stream,
                name,
                number,
                kind,
            }),
            ..Object::default()
        })
    }
    fn media_item(&mut self, class: &str) -> Result<Object> {
        let schema = self.u8()?;
        ensure!(
            (5..=7).contains(&schema),
            "Unsupported binary FLA media schema {schema}"
        );
        let stream = self.stream_name(schema >= 6)?;
        let name = self.string()?;
        let number = self.u16()? as u32;
        self.string()?;
        self.skip(4)?;
        self.item_header()?;
        ensure!(
            class == "CMediaBits",
            "Binary FLA media {class} cannot be exported as reanim"
        );
        self.skip(6)?;
        let quality = self.u8()?;
        let smoothing = self.u8()? != 0;
        self.skip(4)?;
        if schema >= 7 {
            self.skip(1)?;
        }
        let node = Element::new("DOMBitmapItem")
            .with_attr("name", &name)
            .with_attr("bitmapDataHRef", format!("legacy-{number}.dat"))
            .with_attr("allowSmoothing", smoothing)
            .with_attr("compressionType", "lossless")
            .with_attr("quality", quality);
        Ok(Object {
            node,
            item: Some(Item {
                stream,
                name,
                number,
                kind: 0,
            }),
        })
    }
}

pub(crate) fn read_package(bytes: &[u8]) -> Result<Source> {
    let mut cfb =
        cfb::CompoundFile::open(Cursor::new(bytes)).context("Invalid FLA compound document")?;
    let entries = cfb
        .walk()
        .filter(|e| e.is_stream())
        .map(|e| (e.path().to_owned(), e.len()))
        .collect::<Vec<_>>();
    ensure!(entries.len() <= 100_000, "Too many binary FLA streams");
    let mut streams = BTreeMap::new();
    let mut total = 0u64;
    for (path, len) in entries {
        total = total.checked_add(len).context("Binary FLA size overflow")?;
        ensure!(
            len <= LIMIT as u64 && total <= 512 * 1024 * 1024,
            "Binary FLA exceeds stream size limit"
        );
        let mut data = Vec::with_capacity(len as usize);
        cfb.open_stream(&path)?
            .take(LIMIT as u64 + 1)
            .read_to_end(&mut data)?;
        ensure!(data.len() as u64 == len, "Binary FLA stream size mismatch");
        let name = path
            .to_string_lossy()
            .trim_start_matches(['/', '\\'])
            .to_string();
        ensure!(
            streams.insert(name, data).is_none(),
            "Duplicate binary FLA stream"
        );
    }
    let contents = streams
        .get("Contents")
        .context("Binary FLA has no Contents stream")?;
    let mut reader = Reader::new(contents, 0);
    let version = reader.u16()?;
    reader.version = version;
    reader.skip(match version & 0xff {
        0x29 => 13,
        0x38 => 17,
        0x3f => 21,
        0x43 => 25,
        0x47 => 29,
        _ => bail!("Unsupported binary FLA document version {version:#x}"),
    })?;
    let mut scenes = Vec::new();
    while let Some(item) = reader.object(0)? {
        scenes.push(item.item.clone().context("Invalid binary FLA scene")?);
    }
    reader.skip(6)?;
    let mut symbols = Vec::new();
    while let Some(item) = reader.object(0)? {
        symbols.push(item.item.clone().context("Invalid binary FLA symbol")?);
    }
    reader.skip(2)?;
    let mut media = Vec::new();
    while let Some(item) = reader.object(0)? {
        media.push(item);
    }
    reader.skip(5)?;
    let xmin = reader.i32()?;
    let xmax = reader.i32()?;
    let ymin = reader.i32()?;
    let ymax = reader.i32()?;
    let (width, height) = (
        (xmax as f64 - xmin as f64) / 20.,
        (ymax as f64 - ymin as f64) / 20.,
    );
    ensure!(width > 0. && height > 0., "Invalid binary FLA stage size");
    reader.skip(37)?;
    let background = reader.color()?;
    reader.skip(4)?;
    let fps = reader.u32()? as f64 / 65536.;
    ensure!(
        fps > 0. && fps <= 1000.,
        "Invalid binary FLA frame rate {fps}"
    );
    let mut symbol_names = HashMap::new();
    let mut media_names = HashMap::new();
    for item in &symbols {
        ensure!(
            symbol_names
                .insert(item.number, item.name.clone())
                .is_none(),
            "Duplicate binary FLA symbol number"
        );
    }
    for object in &media {
        let item = object.item.as_ref().unwrap();
        ensure!(
            media_names.insert(item.number, item.name.clone()).is_none(),
            "Duplicate binary FLA bitmap number"
        );
    }
    let mut members = BTreeMap::new();
    let mut libraries = BTreeMap::new();
    let mut includes = Vec::new();
    let timeline = |item: &Item| -> Result<Element> {
        let data = streams
            .get(&item.stream)
            .with_context(|| format!("Missing binary FLA stream {}", item.stream))?;
        let mut r = Reader::new(data, version);
        ensure!(r.u8()? == 1, "Invalid binary FLA timeline header");
        let root = r.object(0)?.context("Empty binary FLA timeline")?;
        ensure!(
            root.node.name == "DOMTimeline",
            "Invalid binary FLA timeline root"
        );
        ensure!(
            r.pos == data.len(),
            "Unrecognized binary FLA timeline tail at byte {} of {}",
            r.pos,
            data.len()
        );
        let mut node = root.node.clone();
        node.attrs.insert("name".into(), item.name.clone());
        Ok(node)
    };
    let mut used_names = std::collections::HashSet::new();
    for item in &symbols {
        ensure!(
            used_names.insert(item.name.clone()),
            "Duplicate binary FLA library name: {}",
            item.name
        );
        let href = format!("legacy-{}.node", item.number);
        let mut root = Element::new("DOMSymbolItem")
            .with_attr("xmlns", "http://ns.adobe.com/xfl/2008/")
            .with_attr("name", &item.name)
            .with_attr(
                "symbolType",
                match item.kind {
                    0 => "graphic",
                    1 => "button",
                    2 => "movie clip",
                    _ => bail!("Invalid binary FLA symbol type"),
                },
            );
        root.push(Element::wrap(
            "timeline",
            vec![timeline(item).with_context(|| format!("Library symbol {}", item.name))?],
        ));
        libraries.insert(format!("LIBRARY/{href}"), root);
        includes.push(Element::new("Include").with_attr("href", href));
    }
    let mut bitmaps = Vec::new();
    for object in &media {
        let item = object.item.as_ref().unwrap();
        ensure!(
            used_names.insert(item.name.clone()),
            "Duplicate binary FLA library name: {}",
            item.name
        );
        let data = streams
            .get(&item.stream)
            .with_context(|| format!("Missing binary FLA bitmap stream {}", item.stream))?;
        members.insert(format!("bin/legacy-{}.dat", item.number), data.clone());
        bitmaps.push(object.node.clone());
    }
    let mut timelines = Vec::new();
    for scene in scenes {
        timelines.push(timeline(&scene).with_context(|| format!("Scene {}", scene.name))?);
    }
    ensure!(!timelines.is_empty(), "Binary FLA has no document timeline");
    let mut document = Element::new("DOMDocument")
        .with_attr("xmlns", "http://ns.adobe.com/xfl/2008/")
        .with_attr("width", width)
        .with_attr("height", height)
        .with_attr("frameRate", fps)
        .with_attr("backgroundColor", &background.attrs["color"]);
    document.push(Element::wrap("symbols", includes));
    document.push(Element::wrap("media", bitmaps));
    document.push(Element::wrap("timelines", timelines));
    resolve(&mut document, &symbol_names, &media_names)?;
    for root in libraries.values_mut() {
        resolve(root, &symbol_names, &media_names)?;
    }
    Ok(Source {
        document,
        libraries,
        members,
    })
}

fn resolve(
    node: &mut Element,
    symbols: &HashMap<u32, String>,
    media: &HashMap<u32, String>,
) -> Result<()> {
    for (key, target, table) in [
        ("_symbol", "libraryItemName", symbols),
        ("_media", "libraryItemName", media),
        ("_bitmap", "bitmapPath", media),
    ] {
        if let Some(value) = node.attrs.remove(key) {
            let number = value.parse::<u32>()?;
            let name = table
                .get(&number)
                .with_context(|| format!("Binary FLA references missing {key} {number}"))?;
            node.attrs.insert(target.into(), name.clone());
        }
    }
    node.attrs.retain(|key, _| !key.starts_with('_'));
    for child in &mut node.children {
        resolve(child, symbols, media)?;
    }
    Ok(())
}
