//! Shared authoring data. Binary FLA builds these nodes directly; ZIP/XFL
//! populates the same structure by parsing the XML already in the archive.
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub(crate) struct Element {
    pub name: String,
    pub attrs: BTreeMap<String, String>,
    pub children: Vec<Element>,
    pub text: String,
}
impl Element {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }
    pub fn with_attr(mut self, name: &str, value: impl ToString) -> Self {
        self.attrs.insert(name.into(), value.to_string());
        self
    }
    pub fn push(&mut self, child: Self) {
        self.children.push(child);
    }
    pub fn wrap(name: &str, children: Vec<Self>) -> Self {
        Self {
            name: name.into(),
            children,
            ..Default::default()
        }
    }
    pub fn parse(bytes: &[u8]) -> Result<Self> {
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
                text: n
                    .children()
                    .filter(|n| n.is_text())
                    .filter_map(|n| n.text())
                    .collect(),
            }
        }
        Ok(own(doc.root_element()))
    }
    pub fn attr(&self, name: &str) -> &str {
        self.attrs.get(name).map(String::as_str).unwrap_or("")
    }
    pub fn default<'a>(&'a self, name: &str, default: &'a str) -> &'a str {
        self.attrs.get(name).map(String::as_str).unwrap_or(default)
    }
    pub fn child(&self, name: &str) -> Option<&Self> {
        self.children.iter().find(|c| c.name == name)
    }
    pub fn path(&self, path: &[&str]) -> Option<&Self> {
        path.iter().try_fold(self, |n, k| n.child(k))
    }
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Self> {
        self.children.iter().filter(move |c| c.name == name)
    }
    pub fn list(&self, a: &str, b: &str) -> Vec<&Self> {
        self.child(a)
            .map(|n| n.children.iter().filter(|c| c.name == b).collect())
            .unwrap_or_default()
    }
    pub fn descendants<'a>(&'a self, name: &str, out: &mut Vec<&'a Self>) {
        for c in &self.children {
            if c.name == name {
                out.push(c);
            }
            c.descendants(name, out);
        }
    }
    pub fn number(&self, name: &str, default: f64) -> f64 {
        self.attr(name)
            .parse()
            .ok()
            .filter(|v: &f64| v.is_finite())
            .unwrap_or(default)
    }
    pub fn index(&self, name: &str, default: usize) -> usize {
        self.attr(name).parse().unwrap_or(default)
    }
    pub fn num(&self, name: &str, default: f64) -> Result<f64> {
        let value = self
            .attrs
            .get(name)
            .map(|v| v.parse::<f64>())
            .transpose()?
            .unwrap_or(default);
        ensure!(value.is_finite(), "Non-finite authoring value {name}");
        Ok(value)
    }
    pub fn integer(&self, name: &str, default: usize) -> Result<usize> {
        Ok(self
            .attrs
            .get(name)
            .map(|v| v.parse::<usize>())
            .transpose()?
            .unwrap_or(default))
    }
}

pub(crate) struct Source {
    pub document: Element,
    pub libraries: BTreeMap<String, Element>,
    pub members: BTreeMap<String, Vec<u8>>,
    pub prefix: String,
}
impl Source {
    pub fn from_members(members: BTreeMap<String, Vec<u8>>) -> Result<Self> {
        let (name, bytes) = members
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("DOMDocument.xml"))
            .or_else(|| {
                members
                    .iter()
                    .find(|(n, _)| n.to_ascii_lowercase().ends_with("/domdocument.xml"))
            })
            .context("FLA has no DOMDocument.xml")?;
        let prefix = name[..name.len() - "DOMDocument.xml".len()].to_string();
        let document = Element::parse(bytes)?;
        let mut libraries = BTreeMap::new();
        for (name, bytes) in &members {
            let relative = name.strip_prefix(&prefix).unwrap_or(name);
            if relative.to_ascii_lowercase().starts_with("library/")
                && relative.to_ascii_lowercase().ends_with(".xml")
            {
                libraries.insert(
                    relative.into(),
                    Element::parse(bytes).with_context(|| format!("Invalid library XML {name}"))?,
                );
            }
        }
        Ok(Self {
            document,
            libraries,
            members,
            prefix,
        })
    }
}
