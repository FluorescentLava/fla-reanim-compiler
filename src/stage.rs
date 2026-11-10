//! Authoring stage dimensions, which the game's compiled format does not store.
use std::{collections::BTreeMap, path::Path, sync::LazyLock};

static STOCK: LazyLock<BTreeMap<String, [u32; 2]>> = LazyLock::new(|| {
    serde_json::from_str::<BTreeMap<String, [u32; 2]>>(
        include_str!("../data/goty-stages.json").trim_start_matches('\u{feff}'),
    )
    .expect("valid checked-in GOTY stage dimensions")
    .into_iter()
    .map(|(name, size)| (name.to_ascii_lowercase(), size))
    .collect()
});

pub fn stock(input: &Path) -> Option<[u32; 2]> {
    let name = input.file_name()?.to_str()?.to_ascii_lowercase();
    let name = name.strip_suffix(".compiled").unwrap_or(&name);
    let name = name.strip_suffix(".reanim").unwrap_or(name);
    STOCK.get(name).copied()
}

pub fn recorded(input: &Path) -> Option<[u32; 2]> {
    let bytes = std::fs::read(input.with_extension("authoring.json")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let size = value.get("stage")?.as_array()?;
    if size.len() != 2 {
        return None;
    }
    let size = [
        u32::try_from(size[0].as_u64()?).ok()?,
        u32::try_from(size[1].as_u64()?).ok()?,
    ];
    size.iter().all(|v| (1..=16384).contains(v)).then_some(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_stages_are_not_inferred_from_content_bounds() {
        assert_eq!(stock(Path::new("Chomper.reanim.compiled")), Some([80, 80]));
        assert_eq!(stock(Path::new("ZOMBIE.reanim.compiled")), Some([80, 139]));
        assert_eq!(
            stock(Path::new("Zombie_boss.reanim.compiled")),
            Some([800, 600])
        );
        assert_eq!(
            stock(Path::new("SelectorScreen.reanim.compiled")),
            Some([800, 600])
        );
        assert_eq!(stock(Path::new("custom.reanim.compiled")), None);
    }
}
