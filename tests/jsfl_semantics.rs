//! Compare the normal importer directly with the original PopCap exporter.
//! These tests never call original_if_unchanged or recover compiled metadata.
use fla_reanim::forward;
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::Path};

#[derive(Deserialize)]
struct Reference {
    name: String,
    frames: usize,
    fps: f32,
    tracks: Vec<ReferenceTrack>,
    members: BTreeMap<String, String>,
}
#[derive(Deserialize)]
struct ReferenceTrack {
    name: String,
    active_ranges: Vec<[usize; 2]>,
}
fn package(path: &Path, members: impl IntoIterator<Item = (String, String)>) {
    for (name, text) in members {
        let target = path.join(name);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, text).unwrap();
    }
}
fn document(layers: &str) -> String {
    format!(
        "<DOMDocument frameRate=\"12\"><timelines><DOMTimeline><layers>{layers}</layers></DOMTimeline></timelines></DOMDocument>"
    )
}
fn symbol(name: &str, elements: &str) -> String {
    format!(
        "<DOMSymbolItem name=\"{name}\"><timeline><DOMTimeline><layers><DOMLayer><frames><DOMFrame><elements>{elements}</elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timeline></DOMSymbolItem>"
    )
}

#[test]
fn six_real_goty_anim_timelines_match_original_reanim_frame_states() {
    let references: Vec<Reference> =
        serde_json::from_str(include_str!("fixtures/goty-anim-reference.json")).unwrap();
    assert_eq!(references.len(), 6);
    let mut checked_tracks = 0;
    for reference in references {
        let dir = tempfile::tempdir().unwrap();
        package(dir.path(), reference.members);
        let compiled = forward::convert(dir.path()).unwrap().expanded();
        assert_eq!(
            compiled.frame_count(),
            reference.frames,
            "{}",
            reference.name
        );
        assert_eq!(compiled.fps, reference.fps, "{}", reference.name);
        assert_eq!(
            compiled
                .tracks
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            reference
                .tracks
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            "{} bottom layer must be the first exported track",
            reference.name
        );
        for (actual, expected) in compiled.tracks.iter().zip(&reference.tracks) {
            for (frame, transform) in actual.transforms.iter().enumerate() {
                let active = expected
                    .active_ranges
                    .iter()
                    .any(|[a, b]| *a <= frame && frame < *b);
                assert_eq!(
                    transform.values[6],
                    if active { 0. } else { -1. },
                    "{} / {} frame {frame}",
                    reference.name,
                    actual.name
                );
                assert!(
                    transform.image.is_empty(),
                    "{} / {} unexpectedly acquired artwork",
                    reference.name,
                    actual.name
                );
            }
        }
        checked_tracks += compiled.tracks.len();
    }
    assert_eq!(checked_tracks, 61);
}

#[test]
fn helper_names_visibility_labels_and_blank_anim_layers_follow_jsfl() {
    let dir = tempfile::tempdir().unwrap();
    let xml = document(
        r#"
<DOMLayer name="front" visible="false"><frames><DOMFrame duration="4"><elements><DOMBitmapInstance libraryItemName="IMAGE_REANIM_HEAD__A.png"/></elements></DOMFrame></frames></DOMLayer>
<DOMLayer name="_guide"><frames><DOMFrame duration="8"><elements><DOMShape/></elements></DOMFrame></frames></DOMLayer>
<DOMLayer name="anim_open"><frames><DOMFrame duration="2"/><DOMFrame index="2" duration="2"/></frames></DOMLayer>
<DOMLayer name="labels"><frames><DOMFrame duration="4" name="anim_from_label"/></frames></DOMLayer>
<DOMLayer name="_waterline"><frames><DOMFrame duration="4"/></frames></DOMLayer>
<DOMLayer name="_ground"><frames><DOMFrame duration="4"><elements><DOMShape><matrix><Matrix tx="7"/></matrix></DOMShape></elements></DOMFrame></frames></DOMLayer>
<DOMLayer name="bottom"><frames><DOMFrame duration="4"><elements><DOMBitmapInstance libraryItemName="BOTTOM.png"/></elements></DOMFrame></frames></DOMLayer>"#,
    );
    package(dir.path(), [("DOMDocument.xml".into(), xml)]);
    assert!(!forward::needs_plus(dir.path()).unwrap());
    let compiled = forward::convert(dir.path()).unwrap().expanded();
    assert_eq!(compiled.frame_count(), 8); // skipped _guide still sets document length
    assert_eq!(
        compiled
            .tracks
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["bottom", "_ground", "labels", "anim_open", "front"]
    );
    for name in ["labels", "anim_open"] {
        assert!(
            compiled
                .tracks
                .iter()
                .find(|t| t.name == name)
                .unwrap()
                .transforms
                .iter()
                .all(|t| t.values[6] == -1.)
        );
    }
    let ground = &compiled.tracks[1].transforms[0];
    assert_eq!(ground.values[0], 7.);
    assert_eq!(ground.values[6], 0.);
    assert!(ground.image.is_empty());
    assert_eq!(
        compiled.tracks[4].transforms[0].image,
        "IMAGE_REANIM_HEAD__A"
    );
}

#[test]
fn main_timeline_never_falls_back_to_a_library_animation() {
    let dir = tempfile::tempdir().unwrap();
    package(
        dir.path(),
        [
            (
                "DOMDocument.xml".into(),
                document(
                    r#"<DOMLayer name="blank_main"><frames><DOMFrame duration="3"/></frames></DOMLayer>"#,
                ),
            ),
            (
                "LIBRARY/long_animation.xml".into(),
                symbol(
                    "long_animation",
                    r#"<DOMBitmapInstance libraryItemName="UNRELATED.png"/>"#,
                ),
            ),
        ],
    );
    let compiled = forward::convert(dir.path()).unwrap();
    assert_eq!(compiled.frame_count(), 3);
    assert_eq!(compiled.tracks.len(), 1);
    assert_eq!(compiled.tracks[0].name, "blank_main");
    assert!(
        compiled.tracks[0]
            .transforms
            .iter()
            .all(|t| t.values[6] == -1.)
    );
}

#[test]
fn arbitrary_empty_symbol_is_active_but_multiple_root_elements_are_blank() {
    let dir = tempfile::tempdir().unwrap();
    package(dir.path(), [
        ("DOMDocument.xml".into(), document(r#"<DOMLayer name="anim_test"><frames>
<DOMFrame duration="2"><elements><DOMSymbolInstance libraryItemName="empty_command" firstFrame="2"><matrix><Matrix tx="42"/></matrix></DOMSymbolInstance></elements></DOMFrame>
<DOMFrame index="2"><elements><DOMBitmapInstance libraryItemName="A.png"/><DOMBitmapInstance libraryItemName="B.png"/></elements></DOMFrame>
</frames></DOMLayer>"#)),
        ("LIBRARY/empty_command.xml".into(), r#"<DOMSymbolItem name="empty_command"><timeline><DOMTimeline><layers><DOMLayer><frames><DOMFrame duration="2"><elements/></DOMFrame><DOMFrame index="2"><elements><DOMBitmapInstance libraryItemName="WRONG_LATER_FRAME.png"/></elements></DOMFrame></frames></DOMLayer><DOMLayer><frames><DOMFrame><elements><DOMBitmapInstance libraryItemName="WRONG_LOWER_LAYER.png"/></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timeline></DOMSymbolItem>"#.into()),
    ]);
    let compiled = forward::convert(dir.path()).unwrap();
    assert_eq!(compiled.tracks.len(), 1);
    let track = &compiled.tracks[0];
    assert_eq!(
        track
            .transforms
            .iter()
            .map(|t| t.values[6])
            .collect::<Vec<_>>(),
        [0., 0., -1.]
    );
    assert_eq!(track.transforms[0].values[0], 42.);
    assert!(track.transforms[0].image.is_empty());
}

#[test]
fn attacher_commands_use_jsfl_symbol_priority_and_clear_text_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    package(
        dir.path(),
        [
            (
                "DOMDocument.xml".into(),
                document(
                    r#"<DOMLayer name="attacher__effect"><frames>
<DOMFrame><elements><DOMSymbolInstance libraryItemName="root__fallback"/></elements></DOMFrame>
<DOMFrame index="1"><elements><DOMSymbolInstance libraryItemName="outer"/></elements></DOMFrame>
<DOMFrame index="2"><elements><DOMBitmapInstance libraryItemName="attacher__bitmap.png"/></elements></DOMFrame>
<DOMFrame index="3"><elements><DOMBitmapInstance libraryItemName="ORDINARY.png"/></elements></DOMFrame>
<DOMFrame index="4"><elements><DOMSymbolInstance libraryItemName="attacher__empty__anim_idle"/></elements></DOMFrame>
</frames></DOMLayer>"#,
                ),
            ),
            (
                "LIBRARY/root__fallback.xml".into(),
                symbol(
                    "root__fallback",
                    r#"<DOMSymbolInstance libraryItemName="inner"/>"#,
                ),
            ),
            (
                "LIBRARY/inner.xml".into(),
                symbol(
                    "inner",
                    r#"<DOMBitmapInstance libraryItemName="ORDINARY.png"/>"#,
                ),
            ),
            (
                "LIBRARY/outer.xml".into(),
                symbol(
                    "outer",
                    r#"<DOMSymbolInstance libraryItemName="inner__command"/>"#,
                ),
            ),
            (
                "LIBRARY/inner__command.xml".into(),
                symbol(
                    "inner__command",
                    r#"<DOMBitmapInstance libraryItemName="ORDINARY.png"/>"#,
                ),
            ),
            (
                "LIBRARY/attacher__empty__anim_idle.xml".into(),
                symbol("attacher__empty__anim_idle", ""),
            ),
        ],
    );
    let compiled = forward::convert(dir.path()).unwrap();
    let frames = &compiled.tracks[0].transforms;
    assert_eq!(
        frames.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
        [
            "root__fallback",
            "inner__command",
            "attacher__bitmap.png",
            "_",
            "attacher__empty__anim_idle"
        ]
    );
    assert_eq!(frames[3].image, "IMAGE_REANIM_ORDINARY");
    for i in [0, 1, 2, 4] {
        assert!(frames[i].image.is_empty());
        assert_eq!(frames[i].values[6], 0.);
    }
}

#[test]
fn single_run_static_text_keeps_font_override_characters_and_origin() {
    let dir = tempfile::tempdir().unwrap();
    package(
        dir.path(),
        [(
            "DOMDocument.xml".into(),
            document(
                r#"<DOMLayer name="caption"><frames>
<DOMFrame><elements><DOMStaticText x="0" y="0" width="0" autoExpand="true"><matrix><Matrix tx="80" ty="20"/></matrix><textRuns><DOMTextRun><characters> A&amp;B &lt;plant&gt; </characters><textAttrs><DOMTextAttrs face="Arial" size="40" url="DIALOGBROWN"/></textAttrs></DOMTextRun></textRuns></DOMStaticText></elements></DOMFrame>
<DOMFrame index="1"><elements><DOMStaticText x="3"><matrix><Matrix tx="80" ty="20"/></matrix><textRuns><DOMTextRun><characters>Default font</characters><textAttrs><DOMTextAttrs face="Arial" size="40"/></textAttrs></DOMTextRun></textRuns></DOMStaticText></elements></DOMFrame>
<DOMFrame index="2"><elements><DOMStaticText><textRuns><DOMTextRun><characters/><textAttrs><DOMTextAttrs face="Arial" size="40"/></textAttrs></DOMTextRun></textRuns></DOMStaticText></elements></DOMFrame>
</frames></DOMLayer>"#,
            ),
        )],
    );
    assert!(!forward::needs_plus(dir.path()).unwrap());
    let compiled = forward::convert(dir.path()).unwrap();
    let frames = &compiled.tracks[0].transforms;
    assert_eq!(frames[0].font, "FONT_DIALOGBROWN");
    assert_eq!(frames[0].text, " A&B <plant> ");
    assert_eq!(&frames[0].values[..2], &[80., 20.]);
    assert_eq!(frames[1].font, "FONT_ARIAL32");
    assert_eq!(&frames[1].values[..2], &[77., 20.]);
    assert_eq!(frames[2].text, "_");
    assert!(
        frames
            .iter()
            .all(|t| t.image.is_empty() && t.values[6] == 0.)
    );
}

#[test]
fn mixed_text_runs_fail_instead_of_losing_font_or_characters() {
    let dir = tempfile::tempdir().unwrap();
    package(
        dir.path(),
        [(
            "DOMDocument.xml".into(),
            document(
                r#"<DOMLayer name="caption"><frames><DOMFrame><elements><DOMStaticText><textRuns><DOMTextRun><characters>A</characters></DOMTextRun><DOMTextRun><characters>B</characters></DOMTextRun></textRuns></DOMStaticText></elements></DOMFrame></frames></DOMLayer>"#,
            ),
        )],
    );
    assert!(
        forward::convert(dir.path())
            .unwrap_err()
            .to_string()
            .contains("exactly one DOMTextRun")
    );
}

#[test]
fn rotating_symbol_samples_outer_tween_before_applying_child_offset() {
    let dir = tempfile::tempdir().unwrap();
    package(
        dir.path(),
        [
            (
                "DOMDocument.xml".into(),
                document(
                    r#"<DOMLayer name="art"><frames><DOMFrame duration="2" tweenType="motion"><elements><DOMSymbolInstance libraryItemName="offset"/></elements></DOMFrame><DOMFrame index="2"><elements><DOMSymbolInstance libraryItemName="offset"><matrix><Matrix a="0" b="1" c="-1" d="0"/></matrix></DOMSymbolInstance></elements></DOMFrame></frames></DOMLayer>"#,
                ),
            ),
            (
                "LIBRARY/offset.xml".into(),
                symbol(
                    "offset",
                    r#"<DOMBitmapInstance libraryItemName="TILE.png"><matrix><Matrix tx="100"/></matrix></DOMBitmapInstance>"#,
                ),
            ),
        ],
    );
    let compiled = forward::convert(dir.path()).unwrap();
    let pose = &compiled.tracks[0].transforms[1].values;
    assert_eq!(&pose[..4], &[70.7, 70.7, 45., 45.]);
    // Interpolating flattened bitmap endpoints instead produces (50, 50).
}

#[test]
fn real_blover_offset_petals_follow_original_jsfl_poses() {
    #[derive(Deserialize)]
    struct Fixture {
        members: BTreeMap<String, String>,
        tracks: Vec<Poses>,
    }
    #[derive(Deserialize)]
    struct Poses {
        name: String,
        poses: Vec<[f32; 3]>,
    }
    let reference: Fixture =
        serde_json::from_str(include_str!("fixtures/goty-blover-symbol-tween.json")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    package(dir.path(), reference.members);
    let compiled = forward::convert(dir.path()).unwrap().expanded();
    assert_eq!(compiled.tracks.len(), reference.tracks.len());
    for (track, expected) in compiled.tracks.iter().zip(reference.tracks) {
        assert_eq!(track.name, expected.name);
        for (frame, (pose, original)) in track.transforms.iter().zip(expected.poses).enumerate() {
            assert_eq!(pose.values[6], original[2]);
            if original[2] >= 0. {
                for (axis, expected) in original.iter().take(2).enumerate() {
                    assert!(
                        (pose.values[axis] - expected).abs() <= 0.65,
                        "{} frame {frame} axis {axis}: {} vs stock {}",
                        track.name,
                        pose.values[axis],
                        expected
                    );
                }
            }
        }
    }
}

#[test]
fn bitmap_resource_ids_preserve_spaces_hyphens_and_repeated_underscores() {
    let dir = tempfile::tempdir().unwrap();
    package(
        dir.path(),
        [(
            "DOMDocument.xml".into(),
            document(
                r#"<DOMLayer name="art"><frames>
<DOMFrame><elements><DOMBitmapInstance libraryItemName="Tanglekelp_arm 8.png"/></elements></DOMFrame>
<DOMFrame index="1"><elements><DOMBitmapInstance libraryItemName="Zengarden-wateringcan4.png"/></elements></DOMFrame>
<DOMFrame index="2"><elements><DOMBitmapInstance libraryItemName="Zombie_dancer__head.png"/></elements></DOMFrame>
</frames></DOMLayer>"#,
            ),
        )],
    );
    let compiled = forward::convert(dir.path()).unwrap();
    assert_eq!(
        compiled.tracks[0]
            .transforms
            .iter()
            .map(|t| t.image.as_str())
            .collect::<Vec<_>>(),
        [
            "IMAGE_REANIM_TANGLEKELP_ARM 8",
            "IMAGE_REANIM_ZENGARDEN-WATERINGCAN4",
            "IMAGE_REANIM_ZOMBIE_DANCER__HEAD"
        ]
    );
}

#[test]
fn catalog_only_maps_explicit_full_paths_when_raw_id_has_no_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let resources = dir.path().join("Resources");
    fs::create_dir(&resources).unwrap();
    fs::write(resources.join("resources.xml"), r#"<ResourceManifest><Resources><SetDefaults path="reanim" idprefix="IMAGE_REANIM_"/><Image id="CANONICAL__ARM8" path="Tanglekelp_arm 8"/><Image id="ZOMBIE_DANCER__HEAD" path="Zombie_dancer__head"/></Resources></ResourceManifest>"#).unwrap();
    package(
        dir.path(),
        [(
            "DOMDocument.xml".into(),
            document(
                r#"<DOMLayer name="art"><frames>
<DOMFrame><elements><DOMBitmapInstance libraryItemName="Tanglekelp_arm 8.png"/></elements></DOMFrame>
<DOMFrame index="1"><elements><DOMBitmapInstance libraryItemName="Zombie dancer head.png"/></elements></DOMFrame>
<DOMFrame index="2"><elements><DOMBitmapInstance libraryItemName="New__Custom-Art.png"/></elements></DOMFrame>
<DOMFrame index="3"><elements><DOMBitmapInstance libraryItemName="reanim/Tanglekelp_arm 8.png"/></elements></DOMFrame>
</frames></DOMLayer>"#,
            ),
        )],
    );
    let compiled = forward::convert_with_resources(dir.path(), Some(&resources)).unwrap();
    assert_eq!(
        compiled.tracks[0]
            .transforms
            .iter()
            .map(|t| t.image.as_str())
            .collect::<Vec<_>>(),
        [
            "IMAGE_REANIM_TANGLEKELP_ARM 8",
            "IMAGE_REANIM_ZOMBIE DANCER HEAD",
            "IMAGE_REANIM_NEW__CUSTOM-ART",
            "IMAGE_REANIM_CANONICAL__ARM8"
        ]
    );
}

#[test]
fn registration_change_of_the_same_bitmap_does_not_create_motion() {
    let dir = tempfile::tempdir().unwrap();
    package(
        dir.path(),
        [
            (
                "DOMDocument.xml".into(),
                document(
                    r#"<DOMLayer name="art"><frames><DOMFrame duration="2" tweenType="motion"><elements><DOMSymbolInstance libraryItemName="offset"/></elements></DOMFrame><DOMFrame index="2"><elements><DOMSymbolInstance libraryItemName="unshifted"><matrix><Matrix tx="100"/></matrix></DOMSymbolInstance></elements></DOMFrame></frames></DOMLayer>"#,
                ),
            ),
            (
                "LIBRARY/offset.xml".into(),
                symbol(
                    "offset",
                    r#"<DOMBitmapInstance libraryItemName="TILE.png"><matrix><Matrix tx="100"/></matrix></DOMBitmapInstance>"#,
                ),
            ),
            (
                "LIBRARY/unshifted.xml".into(),
                symbol(
                    "unshifted",
                    r#"<DOMBitmapInstance libraryItemName="TILE.png"/>"#,
                ),
            ),
        ],
    );
    let compiled = forward::convert(dir.path()).unwrap();
    assert!(
        compiled.tracks[0]
            .transforms
            .iter()
            .all(|t| t.values[0] == 100.)
    );
}

#[test]
fn image_swap_does_not_blend_in_the_next_bitmaps_internal_scale() {
    let dir = tempfile::tempdir().unwrap();
    package(
        dir.path(),
        [
            (
                "DOMDocument.xml".into(),
                document(
                    r#"<DOMLayer name="art"><frames><DOMFrame duration="2" tweenType="motion"><elements><DOMSymbolInstance libraryItemName="first"/></elements></DOMFrame><DOMFrame index="2"><elements><DOMSymbolInstance libraryItemName="second"><matrix><Matrix tx="10"/></matrix></DOMSymbolInstance></elements></DOMFrame></frames></DOMLayer>"#,
                ),
            ),
            (
                "LIBRARY/first.xml".into(),
                symbol(
                    "first",
                    r#"<DOMBitmapInstance libraryItemName="FIRST.png"/>"#,
                ),
            ),
            (
                "LIBRARY/second.xml".into(),
                symbol(
                    "second",
                    r#"<DOMBitmapInstance libraryItemName="SECOND.png"><matrix><Matrix a="0.8" d="0.8"/></matrix></DOMBitmapInstance>"#,
                ),
            ),
        ],
    );
    let compiled = forward::convert(dir.path()).unwrap();
    let frames = &compiled.tracks[0].transforms;
    assert_eq!(frames[1].values[0], 5.);
    assert_eq!(frames[1].values[4], 1.);
    assert_eq!(frames[1].image, "IMAGE_REANIM_FIRST");
    assert_eq!(frames[2].values[4], 0.8);
    assert_eq!(frames[2].image, "IMAGE_REANIM_SECOND");
}

fn pixel_png(path: &Path, pixel: [u8; 4]) {
    let file = fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(file, 1, 1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&pixel)
        .unwrap();
}

#[test]
fn treefood_reanim_fallback_is_not_replaced_by_different_manifest_art() {
    let dir = tempfile::tempdir().unwrap();
    let resources = dir.path().join("Resources");
    fs::create_dir_all(resources.join("images")).unwrap();
    fs::create_dir_all(resources.join("reanim")).unwrap();
    pixel_png(&resources.join("images/TreeFood.png"), [0, 0, 255, 255]);
    pixel_png(&resources.join("reanim/TreeFood.png"), [255, 0, 0, 255]);
    assert_ne!(
        fla_reanim::image_io::decode(&fs::read(resources.join("images/TreeFood.png")).unwrap())
            .unwrap()
            .as_raw(),
        fla_reanim::image_io::decode(&fs::read(resources.join("reanim/TreeFood.png")).unwrap())
            .unwrap()
            .as_raw(),
    );
    fs::write(resources.join("resources.xml"), r#"<ResourceManifest><Resources><SetDefaults path="images" idprefix="IMAGE_"/><Image id="TREEFOOD" path="TreeFood"/></Resources></ResourceManifest>"#).unwrap();
    package(
        dir.path(),
        [(
            "DOMDocument.xml".into(),
            document(
                r#"<DOMLayer name="art"><frames>
<DOMFrame><elements><DOMBitmapInstance libraryItemName="TreeFood.png"/></elements></DOMFrame>
<DOMFrame index="1"><elements><DOMBitmapInstance libraryItemName="images/TreeFood.png"/></elements></DOMFrame>
</frames></DOMLayer>"#,
            ),
        )],
    );
    let compiled = forward::convert_with_resources(dir.path(), Some(&resources)).unwrap();
    assert!(
        compiled.tracks[0]
            .transforms
            .iter()
            .all(|t| t.image == "IMAGE_REANIM_TREEFOOD")
    );

    // Without the genuine reanim fallback, only the explicitly complete path
    // supplies proof that the manifest's different resource was requested.
    fs::remove_file(resources.join("reanim/TreeFood.png")).unwrap();
    let compiled = forward::convert_with_resources(dir.path(), Some(&resources)).unwrap();
    assert_eq!(
        compiled.tracks[0].transforms[0].image,
        "IMAGE_REANIM_TREEFOOD"
    );
    assert_eq!(compiled.tracks[0].transforms[1].image, "IMAGE_TREEFOOD");
}

#[test]
fn bitmap_persistent_resource_id_overrides_display_name_and_manifest_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let resources = dir.path().join("Resources");
    fs::create_dir(&resources).unwrap();
    fs::write(resources.join("resources.xml"), r#"<ResourceManifest><Resources><SetDefaults path="images" idprefix="IMAGE_"/><Image id="WRONG_ALIAS" path="display-alias"/></Resources></ResourceManifest>"#).unwrap();
    let xml = document(r#"<DOMLayer name="art"><frames><DOMFrame><elements><DOMBitmapInstance libraryItemName="images/display-alias.png"/></elements></DOMFrame></frames></DOMLayer>"#)
        .replace("<timelines>", r#"<media><DOMBitmapItem name="images/display-alias.png"><persistentData><PD n="fla-reanim.resource-id" t="s" v="IMAGE_REANIM_ORIGINAL__TREEFOOD"/></persistentData></DOMBitmapItem></media><timelines>"#);
    package(dir.path(), [("DOMDocument.xml".into(), xml)]);
    let compiled = forward::convert_with_resources(dir.path(), Some(&resources)).unwrap();
    assert_eq!(
        compiled.tracks[0].transforms[0].image,
        "IMAGE_REANIM_ORIGINAL__TREEFOOD"
    );
}

#[test]
fn native_helper_whitelist_keeps_duplicate_original_tracks_only() {
    let dir = tempfile::tempdir().unwrap();
    let xml = document(r#"
<DOMLayer name="_guide"><frames><DOMFrame duration="3"/></frames></DOMLayer>
<DOMLayer name="_guide"><frames><DOMFrame duration="3"/></frames></DOMLayer>
<DOMLayer name="_guide"><frames><DOMFrame duration="3"/></frames></DOMLayer>
<DOMLayer name="_other_authoring_helper"><frames><DOMFrame duration="3"/></frames></DOMLayer>
<DOMLayer name="art"><frames><DOMFrame duration="3"><elements><DOMBitmapInstance libraryItemName="TILE.png"/></elements></DOMFrame></frames></DOMLayer>"#)
        .replace("<timelines>", r#"<persistentData><PD n="fla-reanim.compiled-helper-tracks" t="s" v='["_guide"]'/></persistentData><timelines>"#);
    package(dir.path(), [("DOMDocument.xml".into(), xml)]);
    assert!(!forward::needs_plus(dir.path()).unwrap());
    let compiled = forward::convert(dir.path()).unwrap();
    assert_eq!(
        compiled
            .tracks
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["art", "_guide", "_guide", "_guide"]
    );
    assert!(
        compiled.tracks[1..]
            .iter()
            .flat_map(|t| &t.transforms)
            .all(|t| t.values[6] == -1.)
    );
}

#[test]
fn both_native_bitmap_symbol_names_keep_explicit_sprite_cel() {
    for name in ["ReanimBitmap_0", "__reanim_bitmap_0"] {
        let dir = tempfile::tempdir().unwrap();
        package(
            dir.path(),
            [
                (
                    "DOMDocument.xml".into(),
                    document(&format!(
                        r#"<DOMLayer name="art"><frames><DOMFrame><elements><DOMSymbolInstance libraryItemName="{name}" firstFrame="2"/></elements></DOMFrame></frames></DOMLayer>"#
                    )),
                ),
                (
                    format!("LIBRARY/{name}.xml"),
                    format!(
                        r#"<DOMSymbolItem name="{name}"><timeline><DOMTimeline><layers><DOMLayer><frames><DOMFrame duration="2"><elements><DOMShape/></elements></DOMFrame><DOMFrame index="2"><elements><DOMBitmapInstance libraryItemName="IMAGE_REANIM_ORIGINAL__REANIM_CEL_2.png"/></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timeline></DOMSymbolItem>"#
                    ),
                ),
            ],
        );
        assert!(!forward::needs_plus(dir.path()).unwrap());
        let compiled = forward::convert(dir.path()).unwrap();
        assert_eq!(compiled.tracks[0].transforms[0].values[6], 2.);
        assert_eq!(
            compiled.tracks[0].transforms[0].image,
            "IMAGE_REANIM_ORIGINAL"
        );
    }
}

#[test]
fn new_native_wrapper_keeps_ordinary_attacher_art_as_an_image() {
    let dir = tempfile::tempdir().unwrap();
    package(
        dir.path(),
        [
            (
                "DOMDocument.xml".into(),
                document(
                    r#"<DOMLayer name="attacher__Plants"><frames><DOMFrame><elements><DOMSymbolInstance libraryItemName="ReanimBitmap_0"/></elements></DOMFrame></frames></DOMLayer>"#,
                ),
            ),
            (
                "LIBRARY/ReanimBitmap_0.xml".into(),
                symbol(
                    "ReanimBitmap_0",
                    r#"<DOMBitmapInstance libraryItemName="TreeFood.png"/>"#,
                ),
            ),
        ],
    );
    let compiled = forward::convert(dir.path()).unwrap();
    assert_eq!(
        compiled.tracks[0].transforms[0].image,
        "IMAGE_REANIM_TREEFOOD"
    );
    assert!(compiled.tracks[0].transforms[0].text.is_empty());
}
