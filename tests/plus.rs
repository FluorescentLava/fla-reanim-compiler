use fla_reanim::{codec, plus};
use flate2::{Compression, FlushCompress};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

fn png() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[255, 80, 40, 255])
            .unwrap();
    }
    bytes
}
fn input(dir: &Path, document: &str) {
    fs::write(dir.join("DOMDocument.xml"), document).unwrap();
    fs::write(dir.join("tile.png"), png()).unwrap();
}
fn node<'a>(graph: &'a Value, suffix: &str) -> &'a Value {
    graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"].as_str().unwrap().ends_with(suffix))
        .unwrap()
}
fn matrix(key: &Value) -> Vec<f64> {
    key["matrix"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect()
}
fn fixture(dir: &Path) -> Value {
    let matrices = (0..8)
        .map(|x| format!("<Matrix a=\"1\" d=\"1\" tx=\"{}\" ty=\"20\"/>", x * 10))
        .collect::<String>();
    input(
        dir,
        &format!(
            r#"<DOMDocument frameRate="10"><media><DOMBitmapItem name="tile.png" href="tile.png"/></media><timelines><DOMTimeline><layers>
<DOMLayer name="IK armature" animationType="IK pose" parentLayerIndex="2"><frames><DOMFrame index="0" duration="4" tweenType="IK pose"><elements><DOMBitmapInstance name="upper" libraryItemName="tile.png"/><DOMBitmapInstance name="hand" libraryItemName="tile.png"/></elements><betweenFrameMatrixList>{matrices}</betweenFrameMatrixList></DOMFrame></frames></DOMLayer>
<DOMLayer name="grouped" parentLayerIndex="2"><frames><DOMFrame index="0" duration="4"><elements><DOMGroup name="translated-group"><matrix><Matrix tx="100" ty="60"/></matrix><members><DOMBitmapInstance name="child" libraryItemName="tile.png"><matrix><Matrix tx="120" ty="70"/></matrix></DOMBitmapInstance></members></DOMGroup></elements></DOMFrame></frames></DOMLayer>
<DOMLayer name="folder" layerType="folder"/></layers></DOMTimeline></timelines></DOMDocument>"#
        ),
    );
    plus::import(dir, &dir.join("rig.reanim.compiled"), None).unwrap()
}
#[test]
fn baked_ik_and_local_groups_keep_all_poses() {
    let dir = tempfile::tempdir().unwrap();
    let g = fixture(dir.path());
    assert_eq!(g["source"]["bakedIKSamples"], 8);
    assert_eq!(
        node(&g, "/upper")["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| matrix(k)[4])
            .collect::<Vec<_>>(),
        vec![0., 10., 20., 30.]
    );
    assert_eq!(
        node(&g, "/hand")["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| matrix(k)[4])
            .collect::<Vec<_>>(),
        vec![40., 50., 60., 70.]
    );
    assert_eq!(&matrix(&node(&g, "/child")["keys"][0])[4..], &[20., 10.]);
    assert_eq!(g["images"].as_array().unwrap().len(), 1);
    assert!(dir.path().join(g["images"][0].as_str().unwrap()).is_file());
    let bytes = plus::encode(&g).unwrap();
    assert_eq!(plus::read_graph(&bytes).unwrap().unwrap(), g);
    let definition = codec::decode(&bytes).unwrap();
    assert_eq!(definition.frame_count(), 4);
    assert_eq!(definition.fps, 10.);
}
#[test]
fn ik_deltas_become_parent_local_bone_transforms() {
    let dir = tempfile::tempdir().unwrap();
    let matrices = [
        "1,0,0,1,0,0",
        "0,1,-1,0,200,0",
        "1,0,0,1,0,0",
        "1,0,0,1,0,0",
        "0,1,-1,0,200,0",
        "1,0,0,1,0,0",
    ]
    .iter()
    .map(|m| {
        let v = m.split(',').collect::<Vec<_>>();
        format!(
            r#"<Matrix a="{}" b="{}" c="{}" d="{}" tx="{}" ty="{}"/>"#,
            v[0], v[1], v[2], v[3], v[4], v[5]
        )
    })
    .collect::<String>();
    input(
        dir.path(),
        &format!(
            r#"<DOMDocument frameRate="10"><media><DOMBitmapItem name="tile.png" href="tile.png"/></media><timelines><DOMTimeline><layers><DOMLayer name="rig" animationType="IK pose"><frames><DOMFrame duration="3" tweenType="IK pose"><elements>
<DOMBitmapInstance name="upper" referenceID="upper" libraryItemName="tile.png"><matrix><Matrix tx="100" ty="100"/></matrix></DOMBitmapInstance><DOMBitmapInstance name="hand" referenceID="hand" libraryItemName="tile.png"><matrix><Matrix tx="140" ty="100"/></matrix></DOMBitmapInstance></elements><betweenFrameMatrixList>{matrices}</betweenFrameMatrixList><IKTree><IKNode referenceID="upper" location="0,0"><childNodes><ChildNode referenceID="hand" location="0,0"/></childNodes></IKNode></IKTree></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#
        ),
    );
    let g = plus::import(dir.path(), &dir.path().join("rig.compiled"), None).unwrap();
    let upper = node(&g, "/upper");
    let hand = node(&g, "/hand");
    assert_eq!(hand["parent"], upper["name"]);
    assert_eq!(hand["bone"], true);
    assert_eq!(matrix(&upper["keys"][1]), vec![0., 1., -1., 0., 100., 100.]);
    for key in hand["keys"].as_array().unwrap() {
        assert_eq!(matrix(key), vec![1., 0., 0., 1., 40., 0.]);
    }
}
#[test]
fn root_editor_visibility_and_guides_do_not_filter_exported_art() {
    let dir = tempfile::tempdir().unwrap();
    input(
        dir.path(),
        r#"<DOMDocument><media><DOMBitmapItem name="tile.png" href="tile.png"/></media><timelines><DOMTimeline><layers><DOMLayer name="body" visible="false"><frames><DOMFrame><elements><DOMBitmapInstance name="body" libraryItemName="tile.png"/></elements></DOMFrame></frames></DOMLayer><DOMLayer name="guide" layerType="guide" visible="false"><frames><DOMFrame><elements><DOMBitmapInstance name="guide" libraryItemName="tile.png"/></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#,
    );
    let g = plus::import(dir.path(), &dir.path().join("hidden.compiled"), None).unwrap();
    assert_eq!(node(&g, "/body")["keys"][0]["visible"], true);
    assert_eq!(node(&g, "/guide")["keys"][0]["visible"], true);
    assert_eq!(g["images"].as_array().unwrap().len(), 1);
}
#[test]
fn late_lower_layer_gets_stable_draw_order() {
    let dir = tempfile::tempdir().unwrap();
    input(
        dir.path(),
        r#"<DOMDocument frameRate="10"><media><DOMBitmapItem name="tile.png" href="tile.png"/></media><timelines><DOMTimeline><layers>
<DOMLayer name="front"><frames><DOMFrame duration="2"><elements><DOMBitmapInstance name="front" libraryItemName="tile.png"/></elements></DOMFrame></frames></DOMLayer><DOMLayer name="back"><frames><DOMFrame index="1"><elements><DOMBitmapInstance name="back" libraryItemName="tile.png"/></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#,
    );
    let g = plus::import(dir.path(), &dir.path().join("order.compiled"), None).unwrap();
    let back = node(&g, "/back");
    let front = node(&g, "/front");
    assert_eq!(back["keys"][0]["visible"], false);
    assert!(
        back["keys"].as_array().unwrap().last().unwrap()["order"]
            .as_u64()
            .unwrap()
            < front["keys"].as_array().unwrap().last().unwrap()["order"]
                .as_u64()
                .unwrap()
    );
}
#[test]
fn missing_baked_ik_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    input(
        dir.path(),
        r#"<DOMDocument><timelines><DOMTimeline><layers><DOMLayer animationType="IK pose"><frames><DOMFrame duration="2"><elements/></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#,
    );
    assert!(
        plus::import(dir.path(), &dir.path().join("bad.compiled"), None)
            .unwrap_err()
            .to_string()
            .contains("baked")
    );
}
#[test]
fn linked_bitmap_cannot_escape_source() {
    let dir = tempfile::tempdir().unwrap();
    input(
        dir.path(),
        r#"<DOMDocument><media><DOMBitmapItem name="tile.png" href="../outside.png"/></media><timelines><DOMTimeline><layers><DOMLayer><frames><DOMFrame><elements><DOMBitmapInstance libraryItemName="tile.png"/></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#,
    );
    assert!(
        plus::import(dir.path(), &dir.path().join("bad.compiled"), None)
            .unwrap_err()
            .to_string()
            .contains("Invalid FLA member path")
    );
}
#[test]
fn chunked_sync_flush_bitmap_restores_rgba() {
    // Known Animate on-disk bytes: alpha first, premultiplied color channels.
    let raw = [255, 240, 20, 50, 128, 0, 50, 40];
    let expected_rgba = [240, 20, 50, 255, 0, 100, 80, 128];
    let mut compressor = flate2::Compress::new(Compression::new(1), true);
    let mut compressed = vec![0; 1024];
    compressor
        .compress(&raw, &mut compressed, FlushCompress::Sync)
        .unwrap();
    compressed.truncate(compressor.total_out() as usize);
    let mut dat = vec![3, 5, 8, 0, 2, 0, 1, 0];
    dat.resize(26, 0);
    dat[24..26].copy_from_slice(&[1, 1]);
    // Split the same zlib stream, including a split inside its header.
    dat.extend((1u16).to_le_bytes());
    dat.extend(&compressed[..1]);
    dat.extend(((compressed.len() - 1) as u16).to_le_bytes());
    dat.extend(&compressed[1..]);
    dat.extend([0, 0]);
    let restored = plus::bitmap_dat(&dat).unwrap();
    assert_eq!(
        image::load_from_memory(&restored)
            .unwrap()
            .to_rgba8()
            .as_raw(),
        &expected_rgba
    );
    dat.truncate(dat.len() - 4);
    assert!(plus::bitmap_dat(&dat).is_err());
}
#[test]
fn original_adobe_raw_bitmap_restores_independently_verified_pixels() {
    let data = include_bytes!("fixtures/adobe-raw-bitmap.dat");
    let metadata: Value =
        serde_json::from_str(include_str!("fixtures/adobe-raw-bitmap.json")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(data)),
        metadata["fixture_sha256"].as_str().unwrap()
    );
    let png = plus::bitmap_dat(data).unwrap();
    let pixels = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!((pixels.width(), pixels.height()), (10, 8));
    assert_eq!(
        format!("{:x}", Sha256::digest(pixels.as_raw())),
        metadata["decoded_rgba_sha256"].as_str().unwrap()
    );
}
#[test]
fn raw_bitmap_uses_header_compression_flag_and_padded_row_stride() {
    let mut data = vec![3, 5, 12, 0, 2, 0, 2, 0];
    data.resize(26, 0);
    data[24] = 1;
    data.extend([
        128, 64, 0, 0, 255, 0, 80, 40, 0xef, 0xbe, 0xad, 0xde, 0, 0, 0, 0, 64, 0, 0, 32, 0xef,
        0xbe, 0xad, 0xde,
    ]);
    let png = plus::bitmap_dat(&data).unwrap();
    let pixels = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!((pixels.width(), pixels.height()), (2, 2));
    assert_eq!(
        pixels.as_raw(),
        &[128, 0, 0, 128, 0, 80, 40, 255, 0, 0, 0, 0, 0, 0, 128, 64]
    );
    let mut invalid = data.clone();
    invalid[25] = 2;
    assert!(plus::bitmap_dat(&invalid).is_err());
    invalid = data.clone();
    invalid.push(0);
    assert!(plus::bitmap_dat(&invalid).is_err());
    data.pop();
    assert!(plus::bitmap_dat(&data).is_err());
}
#[test]
fn compressed_bitmap_requires_exact_chunk_framing() {
    let valid = fla_reanim::reverse::bitmap_dat(&[10, 20, 30, 255], 1, 1).unwrap();
    let mut invalid = valid.clone();
    invalid.truncate(valid.len() - 2);
    assert!(plus::bitmap_dat(&invalid).is_err());
    invalid = valid;
    invalid.extend([0, 0]);
    assert!(plus::bitmap_dat(&invalid).is_err());
}
#[test]
fn solid_vectors_rasterize_without_python() {
    let dir = tempfile::tempdir().unwrap();
    input(
        dir.path(),
        r##"<DOMDocument><timelines><DOMTimeline><layers><DOMLayer><frames><DOMFrame><elements><DOMShape><fills><FillStyle index="1"><SolidColor color="#F05028"/></FillStyle></fills><edges><Edge fillStyle1="1" edges="! 0 0| 400 0|400 400|0 400|0 0"/></edges></DOMShape></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"##,
    );
    let g = plus::import(dir.path(), &dir.path().join("vector.compiled"), None).unwrap();
    assert_eq!(g["images"].as_array().unwrap().len(), 1);
    let bytes = fs::read(dir.path().join(g["images"][0].as_str().unwrap())).unwrap();
    let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
    assert_eq!((image.width(), image.height()), (48, 48));
    assert_eq!(image.get_pixel(24, 24).0, [240, 80, 40, 255]);
    assert_eq!(
        matrix(&node(&g, "/DOMShape")["keys"][0]),
        vec![0.5, 0., 0., 0.5, -2., -2.]
    );
}
#[test]
fn extension_rejects_bad_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let g = fixture(dir.path());
    let mut bytes = plus::encode(&g).unwrap();
    let n = bytes.len();
    bytes[n - 8..n - 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(plus::read_graph(&bytes).is_err());
}
#[test]
fn graph_core_keeps_raw_pose_and_visibility() {
    let graph = json!({"format":"pvz-reanim-plus","version":1,"fps":10,"images":[],"clips":{"default":{"start":0,"end":0.2}},"nodes":[{"name":"bone","parent":"","type":"group","keys":[{"time":0,"matrix":[0,1,-1,0,100,30],"visible":true,"alpha":0.3},{"time":0.1,"visible":false}]}]});
    let bytes = plus::encode(&graph).unwrap();
    let d = codec::decode(&bytes).unwrap();
    assert_eq!(
        d.tracks[0].transforms[0].values,
        [100., 30., 90., 90., 1., 1., 0., 0.3]
    );
    assert_eq!(d.tracks[0].transforms[1].values[6], -1.);
}

#[test]
fn flash_animation_ranges_use_layer_elements_and_keep_art() {
    let dir = tempfile::tempdir().unwrap();
    input(
        dir.path(),
        r#"<DOMDocument frameRate="10"><media><DOMBitmapItem name="tile.png" href="tile.png"/></media><timelines><DOMTimeline><layers>
<DOMLayer name="anim_open"><frames><DOMFrame duration="3"><elements/></DOMFrame><DOMFrame index="3" duration="2" name="anim_wrong_label"><elements><DOMSymbolInstance libraryItemName="locator"/></elements></DOMFrame><DOMFrame index="5" duration="4"><elements/></DOMFrame></frames></DOMLayer>
<DOMLayer name="anim_face"><frames><DOMFrame index="1" duration="2"><elements><DOMBitmapInstance libraryItemName="tile.png"/></elements></DOMFrame><DOMFrame index="3" duration="2"><elements/></DOMFrame><DOMFrame index="5"><elements><DOMBitmapInstance libraryItemName="tile.png"/></elements></DOMFrame><DOMFrame index="6" duration="3"><elements><DOMSymbolInstance libraryItemName="locator"/><DOMSymbolInstance libraryItemName="locator"/></elements></DOMFrame></frames></DOMLayer>
<DOMLayer name="anim_start"><frames><DOMFrame duration="9"><elements/></DOMFrame></frames></DOMLayer>
<DOMLayer name="notes"><frames><DOMFrame duration="9" name="anim_label_only"><elements/></DOMFrame></frames></DOMLayer>
</layers></DOMTimeline></timelines></DOMDocument>"#,
    );
    let graph = plus::import(dir.path(), &dir.path().join("clips.compiled"), None).unwrap();
    // The original JSFL has no anim_open exception and never reads frame.name.
    // Reanimation::GetFramesForLayer takes first..last f>=0, including holes.
    assert_eq!(graph["clips"]["anim_open"], json!({"start":0.3,"end":0.5}));
    assert_eq!(graph["clips"]["anim_face"], json!({"start":0.1,"end":0.6}));
    assert_eq!(graph["clips"]["anim_start"], json!({"start":0.,"end":0.1}));
    assert_eq!(graph["clips"]["default"], json!({"start":0.,"end":0.9}));
    assert!(graph["clips"].get("anim_wrong_label").is_none());
    assert!(graph["clips"].get("anim_label_only").is_none());
    assert!(graph["clips"].get("anim_full").is_none());
    // anim_face is a real drawing track in the original Cabbagepult source;
    // anim_ alone must never suppress its artwork.
    assert_eq!(graph["images"].as_array().unwrap().len(), 1);
    assert_eq!(node(&graph, "/tile.png")["type"], "sprite");
}

#[test]
fn flash_locator_layers_keep_raw_transforms_without_loading_art() {
    let dir = tempfile::tempdir().unwrap();
    input(
        dir.path(),
        r#"<DOMDocument width="1200" height="900"><timelines><DOMTimeline><layers>
<DOMLayer name="_reference" layerType="folder"/>
<DOMLayer name="_ground" parentLayerIndex="0"><frames><DOMFrame><elements><DOMBitmapInstance libraryItemName="absent.png"><matrix><Matrix tx="-20" ty="35"/></matrix></DOMBitmapInstance></elements></DOMFrame></frames></DOMLayer>
<DOMLayer name="fullscreen"><frames><DOMFrame><elements><DOMBitmapInstance libraryItemName="absent.png"/></elements></DOMFrame></frames></DOMLayer>
<DOMLayer name="locator_hand"><frames><DOMFrame><elements><DOMBitmapInstance libraryItemName="absent.png"/></elements></DOMFrame></frames></DOMLayer>
<DOMLayer name="_do_not_export"><frames><DOMFrame><elements><DOMBitmapInstance libraryItemName="absent.png"/></elements></DOMFrame></frames></DOMLayer>
</layers></DOMTimeline></timelines></DOMDocument>"#,
    );
    let graph = plus::import(dir.path(), &dir.path().join("locators.compiled"), None).unwrap();
    assert_eq!(graph["images"], json!([]));
    let ground = node(&graph, "_ground/element0/absent.png");
    assert_eq!(&matrix(&ground["keys"][0])[4..], &[-20., 35.]);
    assert_eq!(ground["keys"][0]["image"], -1);
    let nodes = graph["nodes"].as_array().unwrap();
    assert!(
        nodes
            .iter()
            .all(|n| !n["name"].as_str().unwrap().contains("_reference"))
    );
    assert!(
        nodes
            .iter()
            .all(|n| !n["name"].as_str().unwrap().contains("_do_not_export"))
    );
    // A normal layer nested in an omitted authoring folder must not acquire
    // a dangling RPLS parent, which the native game loader would reject.
    assert_eq!(node(&graph, "layer1:_ground")["parent"], "");
}

#[test]
fn flash_stage_size_does_not_recenter_art_and_layers_are_bottom_first() {
    let dir = tempfile::tempdir().unwrap();
    input(
        dir.path(),
        r#"<DOMDocument width="1200" height="900"><media><DOMBitmapItem name="tile.png" href="tile.png"/></media><timelines><DOMTimeline><layers>
<DOMLayer name="front"><frames><DOMFrame><elements><DOMBitmapInstance name="front" libraryItemName="tile.png"><matrix><Matrix tx="-40" ty="70"/></matrix></DOMBitmapInstance></elements></DOMFrame></frames></DOMLayer>
<DOMLayer name="back"><frames><DOMFrame><elements><DOMBitmapInstance name="back" libraryItemName="tile.png"><matrix><Matrix tx="25" ty="-15"/></matrix></DOMBitmapInstance></elements></DOMFrame></frames></DOMLayer>
</layers></DOMTimeline></timelines></DOMDocument>"#,
    );
    let graph = plus::import(dir.path(), &dir.path().join("stage.compiled"), None).unwrap();
    let back = node(&graph, "/back");
    let front = node(&graph, "/front");
    assert_eq!(&matrix(&back["keys"][0])[4..], &[25., -15.]);
    assert_eq!(&matrix(&front["keys"][0])[4..], &[-40., 70.]);
    assert!(
        back["keys"][0]["order"].as_u64().unwrap() < front["keys"][0]["order"].as_u64().unwrap()
    );
    let names = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        names.iter().position(|n| n.ends_with("/back")).unwrap()
            < names.iter().position(|n| n.ends_with("/front")).unwrap()
    );
}

#[test]
fn longer_scene_does_not_replace_the_first_main_timeline() {
    let dir = tempfile::tempdir().unwrap();
    input(
        dir.path(),
        r#"<DOMDocument frameRate="10"><timelines><DOMTimeline name="Scene 1"><layers><DOMLayer name="anim_first"><frames><DOMFrame duration="2"><elements><DOMSymbolInstance libraryItemName="locator"/></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline><DOMTimeline name="Scene 2"><layers><DOMLayer name="anim_second"><frames><DOMFrame duration="20"><elements><DOMSymbolInstance libraryItemName="locator"/></elements></DOMFrame></frames></DOMLayer></layers></DOMTimeline></timelines></DOMDocument>"#,
    );
    let graph = plus::import(dir.path(), &dir.path().join("scene.compiled"), None).unwrap();
    assert_eq!(graph["clips"]["anim_first"], json!({"start":0.,"end":0.2}));
    assert!(graph["clips"].get("anim_second").is_none());
    assert_eq!(graph["clips"]["default"], json!({"start":0.,"end":0.2}));
}
