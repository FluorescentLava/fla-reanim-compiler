use fla_reanim::{Definition, Track, Transform, codec, forward, reverse, tween, workflow};
use std::{fs, path::Path};

#[test]
fn markers_use_real_locator_instances_and_bitmap_names_match_stock_exporter() {
    let dir = tempfile::tempdir().unwrap();
    let images = dir.path().join("reanim");
    fs::create_dir(&images).unwrap();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([12, 34, 56, 255]))
        .save(images.join("HEAD.png"))
        .unwrap();
    let inactive = || Transform {
        values: [0., 0., 0., 0., 1., 1., -1., 1.],
        ..Transform::default()
    };
    let active = || Transform {
        values: [-3., 7., 0., 0., 1., 1., 0., 1.],
        ..Transform::default()
    };
    let definition = Definition {
        fps: 12.,
        tracks: vec![
            Track {
                name: "anim_idle".into(),
                transforms: vec![
                    inactive(),
                    active(),
                    active(),
                    Transform {
                        values: [-3., 7., 0., 0., 1., 1., -1., 1.],
                        ..Transform::default()
                    },
                ],
            },
            Track {
                name: "anim_hair".into(),
                transforms: (0..4)
                    .map(|_| Transform {
                        image: "IMAGE_REANIM_HEAD".into(),
                        ..active()
                    })
                    .collect(),
            },
        ],
    };
    let compiled = dir.path().join("Chomper.reanim.compiled");
    fs::write(&compiled, codec::encode(&definition, false).unwrap()).unwrap();
    let output = dir.path().join("Chomper.fla");
    let report = reverse::convert(&compiled, &output, Some(dir.path()), false).unwrap();
    assert_eq!(report.stage, [80, 80]);
    let package = forward::read_package(&output).unwrap();
    let xml = std::str::from_utf8(&package["DOMDocument.xml"]).unwrap();
    let doc = roxmltree::Document::parse(xml).unwrap();
    let marker = doc
        .descendants()
        .find(|n| n.has_tag_name("DOMLayer") && n.attribute("name") == Some("anim_idle"))
        .unwrap();
    for frame in marker.descendants().filter(|n| n.has_tag_name("DOMFrame")) {
        assert!(frame.attribute("name").is_none());
        let start: usize = frame.attribute("index").unwrap().parse().unwrap();
        assert_eq!(
            frame
                .descendants()
                .filter(|n| n.has_tag_name("DOMSymbolInstance"))
                .count(),
            usize::from((1..3).contains(&start))
        );
    }
    assert!(xml.contains("name=\"HEAD.png\""));
    assert!(!xml.contains("name=\"IMAGE_REANIM_HEAD.png\""));
    // Bypass the embedded original: call the ordinary importer directly.
    let rebuilt = forward::convert(&output).unwrap().expanded();
    assert_eq!(rebuilt.tracks, definition.tracks);
}

#[test]
fn custom_stage_changes_canvas_only_and_commands_survive_normal_compile() {
    let dir = tempfile::tempdir().unwrap();
    let definition = Definition {
        fps: 12.,
        tracks: vec![Track {
            name: "attacher__plant".into(),
            transforms: vec![Transform {
                values: [-50., 25., 0., 0., 1., 1., 0., 1.],
                text: "attacher__Wallnut__anim_idle".into(),
                ..Transform::default()
            }],
        }],
    };
    let compiled = dir.path().join("custom.compiled");
    fs::write(&compiled, codec::encode(&definition, false).unwrap()).unwrap();
    let output = dir.path().join("custom.fla");
    let report = reverse::convert_with_stage(
        &compiled,
        &output,
        None,
        false,
        tween::Tolerance::editable(),
        Some([320, 180]),
    )
    .unwrap();
    assert_eq!(report.stage, [320, 180]);
    assert_eq!(
        forward::convert(Path::new(&output)).unwrap().expanded(),
        definition
    );
}

#[test]
fn game_font_and_text_are_editable_instead_of_metadata_only() {
    let dir = tempfile::tempdir().unwrap();
    let definition = Definition {
        fps: 12.,
        tracks: vec![Track {
            name: "words".into(),
            transforms: vec![Transform {
                values: [14., -3., 0., 0., 1., 1., 0., 0.75],
                font: "FONT_BRIANNETOD32".into(),
                text: "A & B".into(),
                ..Transform::default()
            }],
        }],
    };
    let compiled = dir.path().join("text.compiled");
    fs::write(&compiled, codec::encode(&definition, false).unwrap()).unwrap();
    let output = dir.path().join("text.fla");
    reverse::convert(&compiled, &output, None, false).unwrap();
    assert_eq!(forward::convert(&output).unwrap().expanded(), definition);
}

#[test]
fn custom_authoring_stage_survives_compiled_sidecar_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("DOMDocument.xml"), r#"<DOMDocument width="321" height="179" frameRate="12"><timelines><DOMTimeline><layers><DOMLayer name="locator"><frames><DOMFrame><elements><DOMSymbolInstance libraryItemName="locator"><matrix><Matrix tx="-11" ty="9"/></matrix></DOMSymbolInstance></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#).unwrap();
    let compiled = dir.path().join("custom.reanim.compiled");
    workflow::convert(
        &source,
        &workflow::Options {
            output: Some(compiled.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let output = dir.path().join("custom.fla");
    let report = reverse::convert(&compiled, &output, None, false).unwrap();
    assert_eq!(report.stage, [321, 179]);
    let rebuilt = forward::convert(&output).unwrap().expanded();
    assert_eq!(&rebuilt.tracks[0].transforms[0].values[..2], &[-11., 9.]);
}

#[test]
fn compiled_helper_tracks_and_attacher_bitmap_cuts_remain_explicit() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("reanim")).unwrap();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
        .save(dir.path().join("reanim/HEAD.png"))
        .unwrap();
    let definition = Definition {
        fps: 12.,
        tracks: vec![
            Track {
                name: "_guide".into(),
                transforms: vec![
                    Transform {
                        values: [0., 0., 0., 0., 1., 1., -1., 1.],
                        ..Transform::default()
                    };
                    2
                ],
            },
            Track {
                name: "attacher__part".into(),
                transforms: vec![
                    Transform {
                        text: "attacher__Wallnut__anim_idle".into(),
                        ..Transform::default()
                    },
                    Transform {
                        image: "IMAGE_REANIM_HEAD".into(),
                        text: "_".into(),
                        ..Transform::default()
                    },
                ],
            },
        ],
    };
    let compiled = dir.path().join("cuts.compiled");
    fs::write(&compiled, codec::encode(&definition, false).unwrap()).unwrap();
    let output = dir.path().join("cuts.fla");
    reverse::convert(&compiled, &output, Some(dir.path()), false).unwrap();
    assert_eq!(forward::convert(&output).unwrap().expanded(), definition);
}
