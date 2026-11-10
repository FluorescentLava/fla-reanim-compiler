use crate::{codec, forward, plus, reverse, tv};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Operation {
    #[default]
    Auto,
    Pack,
    Unpack,
    TvToPc,
}

#[derive(Clone, Default)]
pub struct Options {
    pub operation: Operation,
    pub output: Option<PathBuf>,
    pub resource_root: Option<PathBuf>,
    pub plus: bool,
    pub link: bool,
    pub force: bool,
    pub strict_tweens: bool,
    pub stage: Option<[u32; 2]>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub input: PathBuf,
    pub output: PathBuf,
    pub tracks: usize,
    pub frames: usize,
    pub fps: f32,
    pub seconds: f64,
    pub bytes: u64,
    pub detail: String,
}
pub fn default_output(input: &Path) -> PathBuf {
    let text = input.file_name().unwrap_or_default().to_string_lossy();
    if text.to_ascii_lowercase().ends_with(".compiled") {
        let stem = &text[..text.len() - 9];
        let stem = stem.strip_suffix(".reanim").unwrap_or(stem);
        input.with_file_name(format!("{stem}.fla"))
    } else {
        input.with_extension("reanim.compiled")
    }
}

pub fn tv_output(input: &Path) -> PathBuf {
    let name = input.file_name().unwrap_or_default().to_string_lossy();
    let lower = name.to_ascii_lowercase();
    let length = if lower.ends_with(".reanim.compiled") {
        name.len() - ".reanim.compiled".len()
    } else if lower.ends_with(".compiled") {
        name.len() - ".compiled".len()
    } else {
        name.len()
    };
    input.with_file_name(format!("{}_pc.reanim.compiled", &name[..length]))
}

/// TV folders use the same nonrecursive batch selection as the old script.
/// A selected output directory preserves input filenames; otherwise each file
/// receives the safe `_pc` suffix next to its input.
pub fn expand_inputs(input: &Path, options: &Options) -> Result<Vec<(PathBuf, Options)>> {
    if options.operation != Operation::TvToPc || !input.is_dir() {
        return Ok(vec![(input.to_owned(), options.clone())]);
    }
    if let Some(output) = &options.output {
        ensure!(!output.is_file(), "批量转换的输出路径必须是文件夹");
    }
    let mut inputs = Vec::new();
    for entry in fs::read_dir(input)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file()
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("compiled"))
        {
            inputs.push(path);
        }
    }
    inputs.sort();
    ensure!(!inputs.is_empty(), "文件夹中没有 .compiled 动画文件");
    Ok(inputs
        .into_iter()
        .map(|input| {
            let mut settings = options.clone();
            settings.output = options
                .output
                .as_ref()
                .map(|root| root.join(input.file_name().expect("directory entry has a filename")));
            (input, settings)
        })
        .collect())
}

pub fn convert(input: &Path, options: &Options) -> Result<Report> {
    let start = Instant::now();
    let input =
        fs::canonicalize(input).with_context(|| format!("找不到输入 {}", input.display()))?;
    let output = options.output.clone().unwrap_or_else(|| {
        if options.operation == Operation::TvToPc {
            tv_output(&input)
        } else {
            default_output(&input)
        }
    });
    let output = std::path::absolute(output)?;
    ensure!(output != input, "输出路径不能与输入相同");
    if output.exists() {
        ensure!(
            fs::canonicalize(&output)? != input,
            "输出路径不能与输入相同"
        );
    }
    ensure!(
        options.force || !output.exists(),
        "输出已存在：{}；请换一个输出路径，或使用 --force",
        output.display()
    );
    let parent = output.parent().context("invalid output directory")?;
    fs::create_dir_all(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".fla-reanim-")
        .tempdir_in(parent)?;
    let staged = staging
        .path()
        .join(output.file_name().context("invalid output filename")?);
    let compiled = input
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("compiled"));
    match options.operation {
        Operation::Pack => ensure!(!compiled, "请在“解包（逆向）”页面选择 compiled 文件"),
        Operation::Unpack => ensure!(compiled, "解包需要 .reanim.compiled 文件"),
        Operation::Auto | Operation::TvToPc => {}
    }
    if options.operation == Operation::TvToPc {
        let (bytes, summary) = tv::convert_with_summary(&fs::read(&input)?)?;
        fs::write(&staged, bytes)?;
        fs::rename(staged, &output)?;
        return Ok(Report {
            input,
            bytes: fs::metadata(&output)?.len(),
            output,
            tracks: summary.tracks,
            frames: summary.frames,
            fps: summary.fps,
            seconds: start.elapsed().as_secs_f64(),
            detail: "TV → PC 动画转换完成".into(),
        });
    }
    let (definition, detail) = if compiled {
        let bytes = fs::read(&input)?;
        let definition = codec::decode(&bytes)?;
        let report = reverse::convert_with_stage(
            &input,
            &staged,
            options.resource_root.as_deref(),
            options.link,
            if options.strict_tweens {
                crate::tween::Tolerance::default()
            } else {
                crate::tween::Tolerance::editable()
            },
            options.stage,
        )?;
        (definition, format!("{report:?}"))
    } else if let Some(bytes) = reverse::original_if_unchanged(&input)? {
        let definition = codec::decode(&bytes)?;
        fs::write(&staged, bytes)?;
        reverse::restore_assets(&input, &staged)?;
        (
            definition,
            "文档未编辑，保留原始 compiled 的全部字段".into(),
        )
    } else {
        let members = forward::read_package(&input)?;
        let enhanced_source = members
            .get("META-INF/reanim-source.compiled")
            .is_some_and(|bytes| bytes.ends_with(b"RPLS"));
        if options.plus || enhanced_source || forward::needs_plus(&input)? {
            let graph = plus::import(&input, &staged, options.resource_root.as_deref())?;
            let bytes = plus::encode(&graph)?;
            let definition = codec::decode(&bytes)?;
            fs::write(&staged, bytes)?;
            (definition, "增强编译：分组、骨骼和部件图片".into())
        } else {
            let definition =
                forward::convert_with_resources(&input, options.resource_root.as_deref())?;
            fs::write(&staged, codec::encode(&definition, true)?)?;
            let assets = reverse::export_classic_assets_with_resources(
                &input,
                &staged,
                options.resource_root.as_deref(),
            )?;
            (
                definition,
                if assets > 0 {
                    format!(
                        "经典编译；导出 {assets} 张可编辑贴图，请按 .assets.json 中的路径更新到游戏 Resources"
                    )
                } else {
                    "经典编译".into()
                },
            )
        }
    };
    if !compiled {
        let members = forward::read_package(&input)?;
        if let Some(bytes) = members.get("DOMDocument.xml") {
            let source = std::str::from_utf8(bytes)?.trim_start_matches('\u{feff}');
            let document = roxmltree::Document::parse(source)?;
            let root = document.root_element();
            let width: u32 = root.attribute("width").unwrap_or("550").parse()?;
            let height: u32 = root.attribute("height").unwrap_or("400").parse()?;
            fs::write(
                staged.with_extension("authoring.json"),
                serde_json::to_vec_pretty(
                    &serde_json::json!({"stage":[width,height],"coordinate_origin":[0,0]}),
                )?,
            )?;
        }
    }
    let files = walkdir::WalkDir::new(staging.path())
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for entry in &files {
        if entry.file_type().is_file() {
            let target = parent.join(entry.path().strip_prefix(staging.path())?);
            ensure!(
                options.force || !target.exists(),
                "输出已存在：{}",
                target.display()
            );
        }
    }
    for entry in files {
        if !entry.file_type().is_file() {
            continue;
        }
        let target = parent.join(entry.path().strip_prefix(staging.path())?);
        if let Some(directory) = target.parent() {
            fs::create_dir_all(directory)?;
        }
        fs::rename(entry.path(), target)?;
    }
    Ok(Report {
        input,
        bytes: fs::metadata(&output)?.len(),
        output,
        tracks: definition.tracks.len(),
        frames: definition.frame_count(),
        fps: definition.fps,
        seconds: start.elapsed().as_secs_f64(),
        detail,
    })
}

pub fn cli(args: impl IntoIterator<Item = String>) -> Result<()> {
    let mut options = Options::default();
    let mut paths = Vec::new();
    let (mut inspect, mut dump, mut verify) = (false, false, false);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tv-to-pc" => options.operation = Operation::TvToPc,
            "--plus" => options.plus = true,
            "--link" => options.link = true,
            "--force" => options.force = true,
            "--strict-tweens" => options.strict_tweens = true,
            "--stage" => {
                let size = args.next().context("--stage 缺少 WIDTHxHEIGHT")?;
                let (w, h) = size
                    .split_once(['x', 'X', '×'])
                    .context("--stage 格式应为 WIDTHxHEIGHT")?;
                options.stage = Some([w.parse()?, h.parse()?]);
            }
            "--inspect" => inspect = true,
            "--dump" => dump = true,
            "--verify" => verify = true,
            "--output" | "-o" => {
                options.output = Some(args.next().context("--output 缺少路径")?.into())
            }
            "--resource-root" => {
                options.resource_root =
                    Some(args.next().context("--resource-root 缺少路径")?.into())
            }
            "--help" | "-h" => {
                println!(
                    "FlaReanimCompiler [--plus] [--strict-tweens] [--stage WIDTHxHEIGHT] [--force] [--resource-root DIR] [-o FILE] FILE...\nFlaReanimCompiler --tv-to-pc [-o FILE_OR_DIRECTORY] TV_FILE_OR_DIRECTORY...\n--stage: override reverse authoring stage; stock sizes come from GOTY FLA measurements\n--strict-tweens: tighter reverse curve tolerance, with more keyframes\n--tv-to-pc: convert TV animation layout to PC; folders are processed as a batch\n--inspect: summary; --dump: raw definition JSON; --verify: check unchanged-source exact recovery\nNo .NET/Python required. GUI uses installed Windows App Runtime."
                );
                return Ok(());
            }
            _ if arg.starts_with('-') => bail!("未知选项：{arg}"),
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    ensure!(
        !paths.is_empty(),
        "请选择 .fla、XFL 目录或 .reanim.compiled 文件"
    );
    ensure!(
        options.output.is_none() || paths.len() == 1,
        "--output 只能用于一个输入文件"
    );
    if options.operation == Operation::TvToPc {
        ensure!(
            !options.plus
                && !options.strict_tweens
                && options.stage.is_none()
                && !options.link
                && !inspect
                && !dump
                && !verify,
            "--tv-to-pc 不与打包、解包或检查选项组合使用"
        );
    }
    let mut jobs = Vec::new();
    for path in paths {
        jobs.extend(expand_inputs(&path, &options)?);
    }
    let mut failed = 0;
    for (input, options) in jobs {
        let result = (|| -> Result<()> {
            if inspect || dump {
                let d = codec::decode(&fs::read(&input)?)?;
                if dump {
                    println!("{}", serde_json::to_string(&d)?);
                } else {
                    println!(
                        "{}: {} tracks × {} frames, {} fps",
                        input.display(),
                        d.tracks.len(),
                        d.frame_count(),
                        d.fps
                    );
                }
            } else if verify {
                let temp = tempfile::tempdir()?;
                let mut settings = options.clone();
                settings.output = Some(temp.path().join("verify.fla"));
                let report = convert(&input, &settings)?;
                let restored = reverse::original_if_unchanged(&report.output)?
                    .context("missing round-trip metadata")?;
                ensure!(restored == fs::read(&input)?, "round trip differs");
                println!(
                    "{}: exact compiled round trip; {} tracks, {} frames",
                    input.display(),
                    report.tracks,
                    report.frames
                );
            } else {
                println!("{}", serde_json::to_string(&convert(&input, &options)?)?);
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("{}: {error:#}", input.display());
            failed += 1;
        }
    }
    ensure!(failed == 0, "{failed} 个文件转换失败");
    Ok(())
}
