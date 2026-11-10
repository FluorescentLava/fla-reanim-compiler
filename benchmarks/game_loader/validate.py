"""Execute original game C++ loader functions with filesystem resource callbacks.

This checks the loader, resource IDs and image file presence; separate image
validation and actual game rendering remain necessary.
"""
from pathlib import Path
import argparse
import hashlib
import json
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def catalog(root):
    rows = []
    folder = prefix = ""
    for node in ET.parse(root / "resources.xml").getroot().iter():
        tag = node.tag.rsplit("}", 1)[-1]
        if tag == "SetDefaults":
            folder = node.get("path", folder)
            prefix = node.get("idprefix", prefix)
        elif tag in ("Image", "Font") and node.get("path"):
            path = root / folder.replace("\\", "/") / node.get("path").replace("\\", "/")
            resolved = path if path.is_file() else next(
                (Path(str(path) + extension) for extension in
                 (".png", ".jpg", ".jpeg", ".bmp", ".gif", ".txt", ".ttf")
                 if Path(str(path) + extension).is_file()), None)
            if resolved:
                relative = resolved.relative_to(root).as_posix()
                rows.append(("I" if tag == "Image" else "F") + "\t" + prefix + node.get("id", path.stem) + "\t" + relative)
    return rows


def trace(stdout):
    """Parse observations made after actual game missing-value expansion."""
    tracks = []
    fps = None
    for line in stdout.splitlines():
        parts = line.split("\t")
        if parts[0] == "FILE":
            values = dict(part.split("=", 1) for part in parts[2:])
            fps = float(values["fps"])
        elif parts[0] == "TRACK":
            values = dict(part.split("=", 1) for part in parts[2:])
            tracks.append({"name": parts[1], "engine_clip": values["engine_clip"],
                           "frames": int(values["frames"]), "active": values["active"],
                           "active_image_spans": []})
        elif parts[0] == "IMAGE_SPAN":
            tracks[int(parts[1])]["active_image_spans"].append(
                {"frames": parts[2], "path": parts[3]})
    return {"fps": fps, "tracks": tracks}


def normalized_track(track):
    return {**track, "active_image_spans": [
        {**span, "path": span["path"].replace("\\", "/").casefold()}
        for span in track["active_image_spans"]]}


def compare(source, sample):
    expected = source["engine_observations"]
    observed = sample["engine_observations"]
    expected_tracks = expected["tracks"]
    observed_tracks = observed["tracks"]
    expected_names = [track["name"] for track in expected_tracks]
    observed_names = [track["name"] for track in observed_tracks]
    names_equal = expected_names == observed_names
    mismatches = []
    for index in range(max(len(expected_tracks), len(observed_tracks))):
        before = normalized_track(expected_tracks[index]) if index < len(expected_tracks) else None
        after = normalized_track(observed_tracks[index]) if index < len(observed_tracks) else None
        if before != after:
            mismatches.append({"track_index": index, "source": before, "normal": after})
    # Raw image loads may include hidden cached images; compare them as a
    # diagnostic, while enforcing active resolved paths after game expansion.
    image_set = lambda value: sorted(set(tuple(part.replace("\\", "/").casefold()
                                             for part in row)
                                        for row in value["resolved_images"]))
    return {"source_input": source["input"], "source_sha256": source["sha256"],
            "source_loaded": source["loaded"], "fps_equal": expected["fps"] == observed["fps"],
            "ordered_track_names_equal": names_equal,
            "track_observations_equal": not mismatches,
            "track_mismatches": mismatches,
            "raw_image_id_path_sets_equal": image_set(source) == image_set(sample),
            "passed": source["loaded"] and sample["loaded"] and names_equal
                      and not mismatches and expected["fps"] == observed["fps"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--loader", type=Path, required=True)
    parser.add_argument("--resources", type=Path, required=True)
    parser.add_argument("--inputs", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--recursive", action="store_true")
    parser.add_argument("--controls-sample", type=Path,
                        help="Also require the valid sample to load and three corrupted/missing-resource controls to fail")
    parser.add_argument("--source-report", type=Path,
                        help="Compare expanded tracks, engine clips and active resolved image spans against this source loader report")
    parser.add_argument("--conversion-report", type=Path,
                        help="Bind output SHA256s and the converter binary to a separate editable conversion report")
    args = parser.parse_args()
    if not args.loader.is_file():
        parser.error("Build the C++ harness first and pass --loader PATH")
    args.loader = args.loader.resolve()
    args.resources = args.resources.resolve(strict=True)
    args.inputs = args.inputs.resolve(strict=True)
    files = sorted(args.inputs.rglob("*.compiled") if args.recursive else args.inputs.glob("*.compiled"))
    if not files:
        parser.error("No compiled files found")
    rows = catalog(args.resources)
    samples = []
    started = time.monotonic()
    initial = digest(args.loader)
    source_report = None
    source_by_name = {}
    if args.source_report:
        source_report = json.loads(args.source_report.read_text(encoding="utf-8"))
        if source_report["loader"]["sha256"] != initial:
            parser.error("Source report must use the same C++ loader binary")
        if source_report["manifest_sha256"] != digest(args.resources / "resources.xml"):
            parser.error("Source and normal reports must use the same resource manifest")
        for sample in source_report["samples"]:
            name = Path(sample["input"]).name.casefold()
            if name in source_by_name:
                parser.error("Source report has duplicate compiled filenames")
            if "engine_observations" not in sample:
                parser.error("Regenerate source report with this validator and image-span test host")
            source_by_name[name] = sample
        if {path.name.casefold() for path in files} != set(source_by_name):
            parser.error("Source and normal compiled file sets differ")
    conversion_report = None
    conversion_by_name = {}
    if args.conversion_report:
        conversion_report = json.loads(args.conversion_report.read_text(encoding="utf-8"))
        conversion_by_name = {Path(sample["input"]).name.casefold(): sample
                              for sample in conversion_report["samples"]}
        if {path.name.casefold() for path in files} != set(conversion_by_name):
            parser.error("Conversion report and loader file sets differ")
    controls = None
    with tempfile.TemporaryDirectory(prefix="cpp-reanim-loader-") as temporary:
        manifest = Path(temporary) / "catalog.tsv"
        manifest.write_text("\n".join(rows), encoding="utf-8")
        for index, path in enumerate(files):
            process = subprocess.run([str(args.loader), str(manifest), str(args.resources), str(path)],
                                     capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=30)
            sample = {"input": str(path), "sha256": digest(path), "bytes": path.stat().st_size,
                      "loaded": process.returncode == 0, "returncode": process.returncode,
                      "resolved_images": [line.split("\t")[1:] for line in process.stdout.splitlines()
                                          if line.startswith("IMAGE\t")],
                      "engine_observations": trace(process.stdout),
                      "stdout": process.stdout, "stderr": process.stderr}
            if source_report:
                sample["source_comparison"] = compare(source_by_name[path.name.casefold()], sample)
            if conversion_report:
                conversion_sample = conversion_by_name[path.name.casefold()]
                sample["conversion_report_output_sha256_matches"] = sample["sha256"] == conversion_sample["output_sha256"]
                if source_report:
                    sample["conversion_report_source_sha256_matches"] = source_by_name[path.name.casefold()]["sha256"] == conversion_sample["input_sha256"]
            samples.append(sample)
            if (index + 1) % 25 == 0 or index + 1 == len(files):
                print(f"Loaded {index + 1}/{len(files)}", flush=True)
        if args.controls_sample:
            control_report = Path(temporary) / "controls.json"
            process = subprocess.run([sys.executable, str(Path(__file__).with_name("check_controls.py")),
                                      "--loader", str(args.loader), "--resources", str(args.resources),
                                      "--sample", str(args.controls_sample.resolve(strict=True)),
                                      "--output", str(control_report)],
                                     capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=60)
            if process.returncode != 0:
                raise RuntimeError("Game loader negative controls failed: " + process.stderr + process.stdout)
            controls = json.loads(control_report.read_text(encoding="utf-8"))
    report = {
        "method": "Actual game C++ loader core and field tables with filesystem resource callbacks; excludes full SDL/image rendering",
        "loader": {"path": str(args.loader), "sha256": initial},
        "validator_sha256": digest(Path(__file__)),
        "provenance": json.loads(args.loader.with_name("provenance.json").read_text(encoding="utf-8")),
        "resources": str(args.resources), "manifest_sha256": digest(args.resources / "resources.xml"),
        "manifest_entries": len(rows), "samples": samples,
        "summary": {"files": len(files), "loaded": sum(sample["loaded"] for sample in samples),
                    "rejected": sum(not sample["loaded"] for sample in samples),
                    "seconds": time.monotonic() - started},
        "loader_unchanged": initial == digest(args.loader),
    }
    if controls:
        report["negative_controls"] = controls
    elif source_report and source_report.get("negative_controls"):
        report["negative_controls"] = source_report["negative_controls"]
    report["passed"] = report["summary"]["rejected"] == 0 and report["loader_unchanged"]
    if source_report:
        report["source_report"] = {"path": str(args.source_report), "sha256": digest(args.source_report),
                                   "passed": source_report["passed"]}
        report["summary"]["source_comparisons_passed"] = sum(
            sample["source_comparison"]["passed"] for sample in samples)
        report["summary"]["source_comparisons_failed"] = sum(
            not sample["source_comparison"]["passed"] for sample in samples)
        report["summary"]["raw_image_id_path_set_mismatch_files"] = sum(
            not sample["source_comparison"]["raw_image_id_path_sets_equal"] for sample in samples)
        report["passed"] &= source_report["passed"] and report["summary"]["source_comparisons_failed"] == 0
    if conversion_report:
        report["conversion_report"] = {"path": str(args.conversion_report), "sha256": digest(args.conversion_report),
                                       "binary": conversion_report["binary"],
                                       "scope": conversion_report["purpose"],
                                       "summary": conversion_report["summary"], "passed": conversion_report["passed"]}
        report["summary"]["conversion_output_hash_matches"] = sum(
            sample["conversion_report_output_sha256_matches"] for sample in samples)
        report["passed"] &= all(sample["conversion_report_output_sha256_matches"] for sample in samples)
        if source_report:
            report["summary"]["conversion_source_hash_matches"] = sum(
                sample["conversion_report_source_sha256_matches"] for sample in samples)
            report["passed"] &= all(sample["conversion_report_source_sha256_matches"] for sample in samples)
    if report.get("negative_controls"):
        report["passed"] &= report["negative_controls"]["passed"]
    if conversion_report:
        report["combined_validation_passed"] = report["passed"] and conversion_report["passed"]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report["summary"]), flush=True)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
