use fla_reanim::{
    Definition, Track, Transform, codec, forward, plus,
    workflow::{self, Operation, Options},
};
use std::{fs, path::Path};

fn classic() -> Vec<u8> {
    codec::encode(
        &Definition {
            fps: 12.,
            tracks: vec![Track {
                name: "anim_idle".into(),
                transforms: vec![Transform::default()],
            }],
        },
        false,
    )
    .unwrap()
}

#[test]
fn selected_page_rejects_the_opposite_direction() {
    let dir = tempfile::tempdir().unwrap();
    let compiled = dir.path().join("input.reanim.compiled");
    let fla = dir.path().join("input.fla");
    fs::write(&compiled, classic()).unwrap();
    fs::write(&fla, b"not parsed on unpack page").unwrap();
    for (input, operation) in [(&compiled, Operation::Pack), (&fla, Operation::Unpack)] {
        let output = dir.path().join("output");
        assert!(
            workflow::convert(
                input,
                &Options {
                    operation,
                    output: Some(output.clone()),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(!output.exists());
    }
}

#[test]
fn tv_defaults_and_directory_batches_preserve_inputs() {
    assert_eq!(
        workflow::tv_output(Path::new("植物.REANIM.COMPILED")),
        Path::new("植物_pc.reanim.compiled")
    );
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("TV");
    let output = dir.path().join("PC");
    fs::create_dir(&input).unwrap();
    fs::create_dir(input.join("nested")).unwrap();
    fs::write(input.join("a.compiled"), classic()).unwrap();
    fs::write(input.join("b.COMPILED"), classic()).unwrap();
    fs::write(input.join("ignored.txt"), b"keep").unwrap();
    fs::write(input.join("nested/ignored.compiled"), classic()).unwrap();
    let options = Options {
        operation: Operation::TvToPc,
        output: Some(output.clone()),
        ..Default::default()
    };
    let jobs = workflow::expand_inputs(&input, &options).unwrap();
    assert_eq!(jobs.len(), 2);
    for (source, settings) in &jobs {
        assert_eq!(
            settings.output.as_ref().unwrap(),
            &output.join(source.file_name().unwrap())
        );
        // These are PC files deliberately: TV mode must reject them rather
        // than accidentally reversing them into a FLA or modifying the input.
        let before = fs::read(source).unwrap();
        assert!(workflow::convert(source, settings).is_err());
        assert_eq!(fs::read(source).unwrap(), before);
        assert!(!settings.output.as_ref().unwrap().exists());
    }
}

#[test]
fn reverse_autodetects_classic_and_enhanced_without_plus_option() {
    let dir = tempfile::tempdir().unwrap();
    let graph = serde_json::json!({
        "format":"pvz-reanim-plus", "version":1, "fps":12,
        "images":[], "clips":{"anim_idle":{"start":0.,"end":1. / 12.}},
        "nodes":[{"name":"locator", "parent":-1, "keys":[{"time":0.,"matrix":[1.,0.,0.,1.,2.,3.]}]}]
    });
    for (name, bytes, enhanced) in [
        ("classic", classic(), false),
        ("enhanced", plus::encode(&graph).unwrap(), true),
    ] {
        let input = dir.path().join(format!("{name}.reanim.compiled"));
        let output = dir.path().join(format!("{name}.fla"));
        fs::write(&input, bytes).unwrap();
        workflow::convert(
            &input,
            &Options {
                operation: Operation::Unpack,
                output: Some(output.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let package = forward::read_package(&output).unwrap();
        assert!(package.contains_key("DOMDocument.xml"));
        assert_eq!(
            output.with_extension("reanim-plus.json").is_file(),
            enhanced
        );
    }
}
