use fla_reanim::{Definition, Track, Transform, codec, forward, plus, reverse};
use serde_json::Value;
use std::{fs, path::Path};

fn graph_pose(graph: &Value, suffix: &str, time: f64) -> [f64; 2] {
    let node = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"].as_str().unwrap().ends_with(suffix))
        .unwrap();
    let key = node["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| (key["time"].as_f64().unwrap() - time).abs() < 1e-9)
        .unwrap();
    [
        key["matrix"][4].as_f64().unwrap(),
        key["matrix"][5].as_f64().unwrap(),
    ]
}

#[test]
fn adobe_saved_classic_ease_matches_its_observed_midpoint() {
    // Genuine Animate 24 save after JSFL frame.tweenEasing = 50. Adobe emitted
    // acceleration="-50" and exported frame 20 at (350, 193.75), rather than
    // the linear (300, 175). Import directly, bypassing original-source metadata.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("adobe.fla");
    fs::write(&input, include_bytes!("fixtures/adobe-classic-ease.fla")).unwrap();
    let package = forward::read_package(&input).unwrap();
    let xml = std::str::from_utf8(&package["DOMDocument.xml"]).unwrap();
    assert!(xml.contains("creatorInfo=\"Adobe Animate\""));
    assert!(xml.contains("acceleration=\"-50\""));
    let definition = forward::convert(&input).unwrap();
    let track = definition.tracks.iter().find(|t| t.name == "tile").unwrap();
    assert_eq!(track.transforms.len(), 41);
    assert_eq!(&track.transforms[20].values[..2], &[350., 193.8]);
    assert_eq!(&track.transforms[40].values[..2], &[500., 250.]);

    let graph = plus::import(&input, &dir.path().join("plus.compiled"), None).unwrap();
    assert_eq!(graph_pose(&graph, "/__reanim_bitmap_0", 1.), [350., 193.75]);
}

#[test]
fn plus_acceleration_takes_precedence_over_legacy_tween_easing() {
    for (attributes, expected_x) in [
        (r#"acceleration="-100""#, 7.5),
        (r#"acceleration="100""#, 2.5),
        (r#"tweenEasing="100""#, 7.5),
        (r#"tweenEasing="-100""#, 2.5),
        (r#"acceleration="-100" tweenEasing="-100""#, 7.5),
        (r#"acceleration="0" tweenEasing="100""#, 5.),
    ] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("DOMDocument.xml"), format!(
            r#"<DOMDocument frameRate="10"><timelines><DOMTimeline><layers><DOMLayer name="art"><frames><DOMFrame duration="4" tweenType="motion" {attributes}><elements><DOMSymbolInstance libraryItemName="locator"/></elements></DOMFrame><DOMFrame index="4"><elements><DOMSymbolInstance libraryItemName="locator"><matrix><Matrix tx="10"/></matrix></DOMSymbolInstance></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#
        )).unwrap();
        let graph = plus::import(dir.path(), &dir.path().join("plus.compiled"), None).unwrap();
        assert_eq!(graph_pose(&graph, "/locator", 0.2), [expected_x, 0.]);
    }
}

#[test]
fn reverse_writes_adobe_acceleration_with_opposite_sign() {
    for ease in [-50, 50] {
        let dir = tempfile::tempdir().unwrap();
        let definition = Definition {
            fps: 20.,
            tracks: vec![Track {
                name: "locator_track".into(),
                transforms: (0..41)
                    .map(|frame| {
                        let time = frame as f32 / 40.;
                        let q = time + ease as f32 / 100. * time * (1. - time);
                        Transform {
                            values: [100. + 400. * q, 100. + 150. * q, 0., 0., 1., 1., 0., 1.],
                            ..Transform::default()
                        }
                    })
                    .collect(),
            }],
        };
        let compiled = dir.path().join("input.compiled");
        fs::write(&compiled, codec::encode(&definition, false).unwrap()).unwrap();
        let output = dir.path().join("output.fla");
        reverse::convert(&compiled, &output, None, false).unwrap();
        let package = forward::read_package(&output).unwrap();
        let xml = std::str::from_utf8(&package["DOMDocument.xml"]).unwrap();
        let doc = roxmltree::Document::parse(xml).unwrap();
        let tween = doc
            .descendants()
            .find(|node| node.attribute("tweenType") == Some("motion"))
            .unwrap();
        assert_eq!(
            tween.attribute("acceleration"),
            Some((-ease).to_string().as_str())
        );
        assert_eq!(tween.attribute("duration"), Some("40"));
        assert!(!xml.contains("tweenEasing="));
        let recompiled = forward::convert(Path::new(&output)).unwrap();
        let actual = &recompiled
            .tracks
            .iter()
            .find(|t| t.name == "locator_track")
            .unwrap()
            .transforms[20];
        assert!((actual.values[0] - definition.tracks[0].transforms[20].values[0]).abs() < 0.051);
        assert!((actual.values[1] - definition.tracks[0].transforms[20].values[1]).abs() < 0.051);
    }
}
