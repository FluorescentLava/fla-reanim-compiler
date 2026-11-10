use anyhow::Result;
use fla_reanim::reverse::{
    bitmap_dat, export_classic_assets, export_classic_assets_with_resources,
};
use image::{ImageBuffer, Rgba, RgbaImage};
use std::{fs, io::Write, path::Path};
use zip::{ZipWriter, write::SimpleFileOptions};

fn library(path: &Path, images: &[(&str, RgbaImage)]) -> Result<()> {
    let mut zip = ZipWriter::new(fs::File::create(path)?);
    let mut xml = String::from("<DOMDocument><media>");
    for (index, (name, image)) in images.iter().enumerate() {
        xml.push_str(&format!(
            "<DOMBitmapItem name=\"{name}\" bitmapDataHRef=\"{index}.dat\"/>"
        ));
        zip.start_file(format!("bin/{index}.dat"), SimpleFileOptions::default())?;
        zip.write_all(&bitmap_dat(image.as_raw(), image.width(), image.height())?)?;
    }
    xml.push_str("</media></DOMDocument>");
    zip.start_file("DOMDocument.xml", SimpleFileOptions::default())?;
    zip.write_all(xml.as_bytes())?;
    zip.finish()?;
    Ok(())
}

#[test]
fn manifest_paths_and_separate_alpha_survive_without_recovery_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("Resources");
    let folder = root.join("images/background");
    fs::create_dir_all(&folder)?;
    fs::write(
        folder.join("background1.jpg"),
        include_bytes!("fixtures/rgb-red.jpg"),
    )?;
    ImageBuffer::from_pixel(8, 8, Rgba([9u8, 12, 40, 255]))
        .save(folder.join("_background1.png"))?;
    fs::write(
        root.join("resources.xml"),
        "<ResourceManifest><Resources id=\"test\"><SetDefaults path=\"images/background\" idprefix=\"IMAGE_\"/><Image id=\"BACKGROUND1\" path=\"background1\"/></Resources></ResourceManifest>",
    )?;
    let fla = temp.path().join("saved-by-adobe.fla");
    library(
        &fla,
        &[(
            "BACKGROUND1.png",
            ImageBuffer::from_pixel(8, 8, Rgba([255, 0, 0, 73])),
        )],
    )?;
    let output = temp.path().join("result/edited.reanim.compiled");
    assert_eq!(
        export_classic_assets_with_resources(&fla, &output, Some(&root))?,
        1
    );
    let image =
        image::open(temp.path().join("result/images/background/background1.png"))?.to_rgba8();
    let mask = image::open(
        temp.path()
            .join("result/images/background/_background1.png"),
    )?
    .to_rgba8();
    assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 73]);
    assert_eq!(mask.get_pixel(0, 0).0, [73, 73, 73, 255]);
    assert!(!temp.path().join("result/reanim/BACKGROUND1.png").exists());
    let mapping: serde_json::Value =
        serde_json::from_slice(&fs::read(output.with_extension("assets.json"))?)?;
    assert_eq!(
        mapping["IMAGE_REANIM_BACKGROUND1"],
        "images/background/background1.png"
    );
    Ok(())
}

#[test]
fn sprite_sheet_keeps_unedited_rows_after_adobe_discards_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("Resources");
    fs::create_dir_all(root.join("images/sheets"))?;
    ImageBuffer::from_pixel(8, 6, Rgba([10u8, 20, 30, 255]))
        .save(root.join("images/sheets/sprite.png"))?;
    fs::write(
        root.join("resources.xml"),
        "<ResourceManifest><Resources id=\"test\"><SetDefaults path=\"images/sheets\" idprefix=\"IMAGE_REANIM_\"/><Image id=\"SPRITE\" path=\"sprite\" cols=\"2\" rows=\"2\"/></Resources></ResourceManifest>",
    )?;
    let fla = temp.path().join("saved.fla");
    library(
        &fla,
        &[(
            "SPRITE__REANIM_CEL_1.png",
            ImageBuffer::from_pixel(4, 3, Rgba([0, 255, 0, 91])),
        )],
    )?;
    let output = temp.path().join("result/edited.reanim.compiled");
    assert_eq!(
        export_classic_assets_with_resources(&fla, &output, Some(&root))?,
        1
    );
    let sheet = image::open(temp.path().join("result/images/sheets/sprite.png"))?.to_rgba8();
    assert_eq!(sheet.dimensions(), (8, 6));
    assert_eq!(sheet.get_pixel(0, 0).0, [10, 20, 30, 255]);
    assert_eq!(sheet.get_pixel(5, 1).0, [0, 255, 0, 91]);
    assert_eq!(sheet.get_pixel(5, 5).0, [10, 20, 30, 255]);
    Ok(())
}

#[test]
fn no_resource_root_retains_reanim_fallback_and_suffix_mask_updates() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("Resources");
    fs::create_dir_all(root.join("reanim"))?;
    ImageBuffer::from_pixel(2, 2, Rgba([255u8, 0, 0, 255])).save(root.join("reanim/Part.png"))?;
    ImageBuffer::from_pixel(2, 2, Rgba([10u8, 20, 30, 255])).save(root.join("reanim/Part_.png"))?;
    let fla = temp.path().join("library.fla");
    library(
        &fla,
        &[(
            "PART.png",
            ImageBuffer::from_pixel(2, 2, Rgba([255, 0, 0, 99])),
        )],
    )?;
    export_classic_assets_with_resources(
        &fla,
        &temp.path().join("explicit/edited.compiled"),
        Some(&root),
    )?;
    let mask = image::open(temp.path().join("explicit/reanim/Part_.png"))?.to_rgba8();
    assert_eq!(mask.get_pixel(0, 0).0[2], 99);
    // An adjacent Resources directory is also discovered without --resource-root.
    export_classic_assets(&fla, &temp.path().join("auto/edited.compiled"))?;
    assert!(temp.path().join("auto/reanim/Part.png").is_file());
    let fallback = tempfile::tempdir()?;
    let fallback_fla = fallback.path().join("library.fla");
    fs::copy(&fla, &fallback_fla)?;
    export_classic_assets(
        &fallback_fla,
        &fallback.path().join("result/edited.compiled"),
    )?;
    assert!(fallback.path().join("result/reanim/PART.png").is_file());
    Ok(())
}

#[test]
fn adobe_jpeg_backed_dat_is_decoded_by_the_shared_native_decoder() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let fla = temp.path().join("saved.fla");
    let mut zip = ZipWriter::new(fs::File::create(&fla)?);
    zip.start_file("bin/image.dat", SimpleFileOptions::default())?;
    zip.write_all(include_bytes!("fixtures/rgb-red.jpg"))?;
    zip.start_file("DOMDocument.xml", SimpleFileOptions::default())?;
    zip.write_all(b"<DOMDocument><media><DOMBitmapItem name=\"RED.jpg\" bitmapDataHRef=\"image.dat\"/></media></DOMDocument>")?;
    zip.finish()?;
    let output = temp.path().join("result/edited.compiled");
    assert_eq!(export_classic_assets(&fla, &output)?, 1);
    let image = image::open(temp.path().join("result/reanim/RED.png"))?.to_rgba8();
    assert_eq!(image.dimensions(), (8, 8));
    assert_eq!(image.get_pixel(2, 2).0[3], 255);
    Ok(())
}

#[test]
fn manifest_image_alias_wins_over_same_named_reanim_file_like_the_game_loader() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("Resources");
    fs::create_dir_all(root.join("images"))?;
    fs::create_dir_all(root.join("reanim"))?;
    ImageBuffer::from_pixel(2, 2, Rgba([255u8, 0, 0, 255]))
        .save(root.join("images/TreeFood.png"))?;
    ImageBuffer::from_pixel(7, 5, Rgba([0u8, 255, 0, 255]))
        .save(root.join("reanim/TreeFood.png"))?;
    fs::write(
        root.join("resources.xml"),
        "<ResourceManifest><Resources id=\"test\"><SetDefaults path=\"images\" idprefix=\"IMAGE_\"/><Image id=\"TREEFOOD\" path=\"TreeFood\"/></Resources></ResourceManifest>",
    )?;
    let definition = fla_reanim::Definition {
        fps: 12.,
        tracks: vec![fla_reanim::Track {
            name: "food".into(),
            transforms: vec![fla_reanim::Transform {
                image: "IMAGE_REANIM_TREEFOOD".into(),
                ..Default::default()
            }],
        }],
    };
    let compiled = temp.path().join("input.compiled");
    fs::write(&compiled, fla_reanim::codec::encode(&definition, false)?)?;
    let fla = temp.path().join("reverse.fla");
    fla_reanim::reverse::convert(&compiled, &fla, Some(&root), false)?;
    let members = fla_reanim::forward::read_package(&fla)?;
    let xml = roxmltree::Document::parse(std::str::from_utf8(&members["DOMDocument.xml"])?)?;
    let item = xml
        .descendants()
        .find(|n| n.has_tag_name("DOMBitmapItem"))
        .unwrap();
    assert_eq!(item.attribute("frameRight"), Some("40"));
    assert_eq!(item.attribute("frameBottom"), Some("40"));
    let output = temp.path().join("result/edited.compiled");
    export_classic_assets_with_resources(&fla, &output, Some(&root))?;
    assert!(temp.path().join("result/images/TreeFood.png").is_file());
    assert!(!temp.path().join("result/reanim/TreeFood.png").is_file());
    let mapping: serde_json::Value =
        serde_json::from_slice(&fs::read(output.with_extension("assets.json"))?)?;
    assert_eq!(mapping["IMAGE_REANIM_TREEFOOD"], "images/TreeFood.png");
    Ok(())
}

#[test]
fn adobe_persistent_resource_id_survives_library_renaming_and_asset_mapping() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("Resources");
    fs::create_dir_all(root.join("images"))?;
    ImageBuffer::from_pixel(2, 2, Rgba([10u8, 20, 30, 255]))
        .save(root.join("images/TreeFood.png"))?;
    fs::write(
        root.join("resources.xml"),
        "<ResourceManifest><Resources id=\"test\"><SetDefaults path=\"images\" idprefix=\"IMAGE_\"/><Image id=\"TREEFOOD\" path=\"TreeFood\"/></Resources></ResourceManifest>",
    )?;
    let fla = temp.path().join("saved.fla");
    let mut zip = ZipWriter::new(fs::File::create(&fla)?);
    zip.start_file("bin/edited.dat", SimpleFileOptions::default())?;
    zip.write_all(&bitmap_dat(&[255, 0, 0, 255].repeat(4), 2, 2)?)?;
    zip.start_file("DOMDocument.xml", SimpleFileOptions::default())?;
    zip.write_all(b"<DOMDocument><media><DOMBitmapItem name=\"Renamed pixel.png\" bitmapDataHRef=\"edited.dat\"><persistentData><PD n=\"fla-reanim.resource-id\" t=\"s\" v=\"IMAGE_REANIM_TREEFOOD\"/></persistentData></DOMBitmapItem></media></DOMDocument>")?;
    zip.finish()?;
    let output = temp.path().join("result/edited.compiled");
    assert_eq!(
        export_classic_assets_with_resources(&fla, &output, Some(&root))?,
        1
    );
    let mapping: serde_json::Value =
        serde_json::from_slice(&fs::read(output.with_extension("assets.json"))?)?;
    assert_eq!(mapping["IMAGE_REANIM_TREEFOOD"], "images/TreeFood.png");
    assert!(mapping.get("IMAGE_TREEFOOD").is_none());
    assert!(mapping.get("IMAGE_REANIM_RENAMED PIXEL").is_none());
    Ok(())
}

#[test]
fn legal_original_names_keep_spaces_hyphens_and_repeated_underscores() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("Resources");
    fs::create_dir_all(root.join("reanim"))?;
    let names = [
        "TANGLEKELP_ARM 8",
        "ZENGARDEN-WATERINGCAN4",
        "ZOMBIE_DANCER__HEAD",
    ];
    for name in names {
        ImageBuffer::from_pixel(2, 2, Rgba([10u8, 20, 30, 255]))
            .save(root.join(format!("reanim/{name}.png")))?;
    }
    let fla = temp.path().join("saved.fla");
    let library_images = names.map(|name| {
        (
            format!("{name}.png"),
            ImageBuffer::from_pixel(2, 2, Rgba([255, 0, 0, 73])),
        )
    });
    let images: Vec<_> = library_images
        .iter()
        .map(|(name, image)| (name.as_str(), image.clone()))
        .collect();
    library(&fla, &images)?;
    let output = temp.path().join("result/edited.compiled");
    assert_eq!(
        export_classic_assets_with_resources(&fla, &output, Some(&root))?,
        3
    );
    let mapping: serde_json::Value =
        serde_json::from_slice(&fs::read(output.with_extension("assets.json"))?)?;
    for name in names {
        assert!(
            temp.path()
                .join(format!("result/reanim/{name}.png"))
                .is_file()
        );
        assert_eq!(
            mapping[format!("IMAGE_REANIM_{name}")],
            format!("reanim/{name}.png")
        );
    }
    Ok(())
}

#[test]
fn literal_zamboni_stems_with_and_without_trailing_space_never_collide() -> Result<()> {
    // Reverse creation order too: filesystem enumeration must not choose the
    // source image or deployment destination for a different literal ID.
    for reverse_order in [false, true] {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("Resources");
        fs::create_dir_all(root.join("reanim"))?;
        fs::create_dir_all(root.join("images"))?;
        let mut stems = ["Zombie_charred_zamboni6", "Zombie_charred_zamboni6 "];
        if reverse_order {
            stems.reverse();
        }
        for stem in stems {
            let (width, height, red) = if stem.ends_with(' ') {
                (2, 3, 50u8)
            } else {
                (4, 5, 200u8)
            };
            ImageBuffer::from_pixel(width, height, Rgba([red, 20, 30, 255]))
                .save(root.join(format!("reanim/{stem}.png")))?;
            // The game tries reanim before images for IMAGE_REANIM_ IDs.
            ImageBuffer::from_pixel(7, 8, Rgba([0u8, 255, 0, 255]))
                .save(root.join(format!("images/{stem}.png")))?;
        }
        let definition = fla_reanim::Definition {
            fps: 12.,
            tracks: ["ZOMBIE_CHARRED_ZAMBONI6", "ZOMBIE_CHARRED_ZAMBONI6 "]
                .into_iter()
                .map(|stem| fla_reanim::Track {
                    name: stem.to_owned(),
                    transforms: vec![fla_reanim::Transform {
                        image: format!("IMAGE_REANIM_{stem}"),
                        ..Default::default()
                    }],
                })
                .collect(),
        };
        let compiled = temp.path().join("source.compiled");
        fs::write(&compiled, fla_reanim::codec::encode(&definition, false)?)?;
        let fla = temp.path().join("reverse.fla");
        fla_reanim::reverse::convert(&compiled, &fla, Some(&root), false)?;
        let members = fla_reanim::forward::read_package(&fla)?;
        let doc = roxmltree::Document::parse(std::str::from_utf8(&members["DOMDocument.xml"])?)?;
        for item in doc
            .descendants()
            .filter(|n| n.has_tag_name("DOMBitmapItem"))
        {
            let id = item
                .descendants()
                .find(|n| n.attribute("n") == Some("fla-reanim.resource-id"))
                .and_then(|n| n.attribute("v"))
                .unwrap();
            assert_eq!(
                item.attribute("frameRight"),
                Some(if id.ends_with(' ') { "40" } else { "80" })
            );
            assert_eq!(
                item.attribute("frameBottom"),
                Some(if id.ends_with(' ') { "60" } else { "100" })
            );
        }
        let output = temp.path().join("result/edited.compiled");
        assert_eq!(
            export_classic_assets_with_resources(&fla, &output, Some(&root))?,
            2
        );
        let mapping: serde_json::Value =
            serde_json::from_slice(&fs::read(output.with_extension("assets.json"))?)?;
        for (stem, width, height, red) in [
            ("Zombie_charred_zamboni6", 4, 5, 200u8),
            ("Zombie_charred_zamboni6 ", 2, 3, 50u8),
        ] {
            let relative = format!("reanim/{stem}.png");
            assert!(
                mapping[format!("IMAGE_REANIM_{}", stem.to_ascii_uppercase())]
                    .as_str()
                    .unwrap()
                    .eq_ignore_ascii_case(&relative)
            );
            let image = image::open(temp.path().join("result").join(relative))?.to_rgba8();
            assert_eq!(image.dimensions(), (width, height));
            assert_eq!(image.get_pixel(0, 0).0, [red, 20, 30, 255]);
        }
    }
    Ok(())
}
