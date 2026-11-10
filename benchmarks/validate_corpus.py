"""Validate a real compiled corpus through the published Rust CLI.

This is a correctness check, not a performance benchmark. Pillow independently
decodes source PNG/JPEG files and the engine's blue-channel alpha masks. DAT
validation uses Adobe's premultiplied ARGB representation, verified against an
Adobe-authored document and actual Animate display. PNG samples require exact
bytes; JPEG decoder rounding is reported separately and never called exact.

Example:
  python benchmarks/validate_corpus.py --rust path/to/fla-reanim-cli.exe \
    --resource-root path/to/Resources --output benchmarks/corpus-results.json

Python, Pillow and psutil (imported by compare.py) are verification dependencies
only; the released application does not load them.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import re
import subprocess
import tempfile
import time
import traceback
import xml.etree.ElementTree as ET
import zipfile

from compare import bitmap_dat, compiled, find_image, graph, image_catalog, sha256, source_pixels


def local_name(node):
    return node.tag.rsplit("}", 1)[-1]


def run(command, timeout):
    result = subprocess.run(command, capture_output=True, timeout=timeout,
                            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    stdout = result.stdout.decode("utf-8", "replace")
    stderr = result.stderr.decode("utf-8", "replace")
    if result.returncode:
        raise RuntimeError(f"CLI exit {result.returncode}: {stderr or stdout}")
    lines = [line for line in stdout.splitlines() if line.strip()]
    if not lines:
        raise ValueError("CLI returned no conversion report")
    return json.loads(lines[-1])


def resource_cells(root):
    result = {}
    prefix = ""
    for element in ET.parse(root / "resources.xml").getroot().iter():
        if local_name(element) == "SetDefaults":
            prefix = element.get("idprefix", prefix)
        elif local_name(element) == "Image" and element.get("path"):
            identifier = prefix + element.get("id", Path(element.get("path")).stem)
            result.setdefault(identifier.upper(), (int(element.get("cols", "1")),
                                                    int(element.get("rows", "1"))))
    return result


def has_jpeg_dependency(path):
    mask = find_image(path.with_name("_" + path.stem)) or find_image(path.with_name(path.stem + "_"))
    return path.suffix.lower() in (".jpg", ".jpeg") or bool(mask and mask.suffix.lower() in (".jpg", ".jpeg"))


class PixelSources:
    """Cache independent source decoding, never read pixels back from Rust output."""
    def __init__(self, resource_root):
        self.catalog = image_catalog(resource_root)
        self.cells = resource_cells(resource_root)
        self.cache = {}
        self.inputs = {}

    def load(self, identifier, source, enhanced_images):
        match = re.fullmatch(r"(.+)__REANIM_CEL_(\d+)", identifier)
        cel = int(match.group(2)) if match else None
        identifier = match.group(1) if match else identifier
        enhanced = re.fullmatch(r"RPLS_IMAGE_(\d+)", identifier)
        if enhanced and int(enhanced.group(1)) < len(enhanced_images):
            path = (source.parent / enhanced_images[int(enhanced.group(1))]).resolve()
            columns, rows = 1, 1
        else:
            path = self.catalog.get(identifier.upper()) or self.catalog.get(
                re.sub("[^A-Z0-9]+", "_", identifier.upper()).strip("_"))
            columns, rows = self.cells.get(identifier.upper(), (1, 1))
        if path is None:
            return None
        if path not in self.cache:
            size, rgba = source_pixels(path)
            self.cache[path] = (size, rgba, has_jpeg_dependency(path))
            self.inputs[str(path)] = {"sha256": sha256(path), "bytes": path.stat().st_size}
            mask = find_image(path.with_name("_" + path.stem)) or find_image(path.with_name(path.stem + "_"))
            if mask:
                self.inputs[str(mask)] = {"sha256": sha256(mask), "bytes": mask.stat().st_size}
        (width, height), rgba, jpeg = self.cache[path]
        if cel is not None:
            if not (columns > 0 and rows > 0 and 0 <= cel < columns):
                raise ValueError(f"Invalid sprite layout/cel for {identifier}")
            cell_width, cell_height = width // columns, height // rows
            rgba = b"".join(rgba[(y * width + cel * cell_width) * 4:
                                  (y * width + (cel + 1) * cell_width) * 4]
                            for y in range(cell_height))
            width, height = cell_width, cell_height
        return {"path": str(path), "sha256": self.inputs[str(path)]["sha256"],
                "width": width, "height": height, "rgba": rgba,
                "jpeg_dependency": jpeg, "columns": columns, "rows": rows, "cel": cel}


def pixels_match(actual, expected_rgba, jpeg_tolerance, jpeg):
    mismatches = alpha_mismatches = 0
    max_delta = 0
    first = None
    for position in range(0, len(expected_rgba), 4):
        r, g, b, a = expected_rgba[position:position + 4]
        expected = (a, (r * a + 127) // 255, (g * a + 127) // 255, (b * a + 127) // 255)
        pixel = actual[position:position + 4]
        alpha_mismatches += pixel[0] != a
        delta = max(abs(pixel[i] - expected[i]) for i in range(1, 4))
        max_delta = max(max_delta, delta)
        if tuple(pixel) != expected:
            mismatches += 1
            if first is None:
                first = {"pixel": position // 4, "argb": list(pixel), "expected_argb": list(expected)}
    valid = alpha_mismatches == 0 and (max_delta <= jpeg_tolerance if jpeg else mismatches == 0)
    return {"passed": valid, "premultiplied_argb_exact": mismatches == 0,
            "different_pixels": mismatches, "alpha_mismatches": alpha_mismatches,
            "maximum_rgb_channel_delta": max_delta, "first_difference": first,
            "jpeg_decoder_tolerance": jpeg_tolerance if jpeg else 0}


def validate_one(source, directory, args, sources):
    directory.mkdir(parents=True)
    output = directory / "editable.fla"
    command = [str(args.rust), "--resource-root", str(args.resource_root),
               "--output", str(output), str(source)]
    begin = time.monotonic()
    report = run(command, args.timeout)
    definition, _ = compiled(source)
    image_paths = graph(source)["images"] if source.read_bytes().endswith(b"RPLS") else []
    sample = {"input": str(source), "input_sha256": sha256(source),
              "input_bytes": source.stat().st_size, "command": command,
              "cli_report": report, "output_sha256": sha256(output),
              "output_bytes": output.stat().st_size, "xml_valid": True,
              "bitmaps": [], "errors": [], "unresolved_images": []}
    detail = report.get("detail", "")
    sample["warnings"] = {"missing_image_messages": detail.count("找不到贴图"),
                           "decode_failure_messages": detail.count("无法读取"),
                           "detail": detail}
    with zipfile.ZipFile(output) as archive:
        for name in archive.namelist():
            if name.endswith(".xml"):
                ET.fromstring(archive.read(name))
        document = ET.fromstring(archive.read("DOMDocument.xml"))
        frames = [node for node in document.iter() if local_name(node) == "DOMFrame"]
        tweens = [node for node in frames if node.get("tweenType") == "motion"
                  and int(node.get("duration", "1")) > 1]
        original_samples = sum(len(track["transforms"]) for track in definition["tracks"])
        sample["timeline"] = {
            "source_tracks": len(definition["tracks"]),
            "source_animation_frames": max((len(track["transforms"]) for track in definition["tracks"]), default=0),
            "source_track_frame_samples": original_samples,
            "dom_frame_elements": len(frames),
            "actual_multi_frame_motion_tweens": len(tweens),
            "motion_tween_frames_covered": sum(int(node.get("duration", "1")) for node in tweens),
            "dom_frame_reduction_fraction": 1 - len(frames) / original_samples if original_samples else 0,
        }
        for bitmap in (n for n in document.iter() if local_name(n) == "DOMBitmapItem"):
            identifier = Path(bitmap.get("name", "")).stem
            item = {"identifier": identifier, "dat_member": "bin/" + bitmap.get("bitmapDataHRef", "")}
            if not bitmap.get("bitmapDataHRef"):
                item["error"] = "Missing DAT reference"
                sample["errors"].append(item)
                continue
            width, height, argb = bitmap_dat(archive.read(item["dat_member"]))
            item.update(width=width, height=height, decoded_pixel_bytes=len(argb),
                        large_bitmap=len(argb) > 32768)
            expected = sources.load(identifier, source, image_paths)
            if expected is None:
                item["error"] = "Source image cannot be resolved independently"
                sample["unresolved_images"].append(identifier)
            else:
                item.update({key: value for key, value in expected.items() if key != "rgba"})
                if (width, height) != (expected["width"], expected["height"]):
                    item["error"] = "Source and DAT dimensions differ"
                    sample["errors"].append(item)
                else:
                    item["pixels"] = pixels_match(argb, expected["rgba"], args.jpeg_tolerance,
                                                   expected["jpeg_dependency"])
                    if not item["pixels"]["passed"]:
                        sample["errors"].append(item)
            sample["bitmaps"].append(item)
        sample["source_snapshot_exact"] = archive.read("META-INF/reanim-source.compiled") == source.read_bytes()
        digest = hashlib.sha256()
        for name in sorted(archive.namelist()):
            if name.endswith("/") or name.startswith("META-INF/reanim-source."):
                continue
            digest.update(name.encode("utf-8"))
            digest.update(b"\0")
            digest.update(archive.read(name))
        sample["editable_members_fingerprint_matches"] = digest.hexdigest() == archive.read("META-INF/reanim-source.sha256").decode().strip()
    restored = directory / "restored.reanim.compiled"
    sample["restore_cli_report"] = run([str(args.rust), "--output", str(restored), str(output)], args.timeout)
    sample["restored_compiled_bytes_exact"] = restored.read_bytes() == source.read_bytes()
    sample["restored_compiled_sha256"] = sha256(restored)
    sample["restored_external_assets_exact"] = all(
        (restored.parent / image).read_bytes() == (source.parent / image).read_bytes() for image in image_paths)
    sample["seconds_for_correctness_checks"] = time.monotonic() - begin
    sample["passed"] = (not sample["errors"] and not sample["unresolved_images"]
                        and not sample["warnings"]["missing_image_messages"]
                        and not sample["warnings"]["decode_failure_messages"]
                        and sample["source_snapshot_exact"]
                        and sample["editable_members_fingerprint_matches"]
                        and sample["restored_compiled_bytes_exact"]
                        and sample["restored_external_assets_exact"])
    return sample


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--resource-root", type=Path, required=True)
    parser.add_argument("--compiled-dir", type=Path, help="Default: RESOURCE_ROOT/compiled/reanim")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, help="Create a fresh isolated directory here; default: system temp")
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--jpeg-tolerance", type=int, default=4,
                        help="RGB channel tolerance between independent JPEG decoders; alpha is always exact")
    parser.add_argument("--limit", type=int, help="Optional smoke-test sample limit; omitted for the full corpus")
    args = parser.parse_args()
    args.rust = args.rust.resolve(strict=True)
    args.resource_root = args.resource_root.resolve(strict=True)
    inputs = sorted((args.compiled_dir or args.resource_root / "compiled/reanim").glob("*.compiled"))
    if args.limit is not None:
        inputs = inputs[:args.limit]
    if not inputs:
        parser.error("No .compiled resources found")
    if args.work_dir:
        args.work_dir.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="fla-native-corpus-", dir=args.work_dir))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    result = {"schema_version": 1, "created_utc": datetime.now(timezone.utc).isoformat(),
              "purpose": "correctness validation, not performance measurement",
              "platform": platform.platform(), "python": platform.python_version(),
              "binary": {"path": str(args.rust), "sha256": sha256(args.rust),
                         "bytes": args.rust.stat().st_size},
              "resource_root": str(args.resource_root), "work_dir": str(work),
              "resource_manifest": {"path": str(args.resource_root / "resources.xml"),
                                    "sha256": sha256(args.resource_root / "resources.xml")},
              "validator_sha256": sha256(Path(__file__)),
              "shared_validator_sha256": sha256(Path(__file__).with_name("compare.py")),
              "png_policy": "exact premultiplied ARGB and alpha against independently decoded source",
              "jpeg_policy": {"maximum_rgb_channel_delta": args.jpeg_tolerance,
                              "alpha": "exact", "exactness_claimed": False},
              "sample_limit": args.limit, "samples": []}
    sources = PixelSources(args.resource_root)
    started = time.monotonic()
    print(f"Validating {len(inputs)} resources in {work}", flush=True)
    for index, source in enumerate(inputs):
        try:
            sample = validate_one(source, work / f"{index:04}-{source.stem}", args, sources)
        except Exception as error:
            sample = {"input": str(source), "input_sha256": sha256(source),
                      "passed": False, "error": str(error), "traceback": traceback.format_exc()}
        result["samples"].append(sample)
        if not sample["passed"]:
            print(f"FAIL {source.name}: {sample.get('error', sample.get('errors', []))}", flush=True)
        if (index + 1) % 10 == 0 or index + 1 == len(inputs):
            print(f"Checked {index + 1}/{len(inputs)}", flush=True)
        args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
    samples = result["samples"]
    bitmaps = [bitmap for sample in samples for bitmap in sample.get("bitmaps", [])]
    result["source_image_inputs"] = sources.inputs
    result["summary"] = {
        "input_files": len(inputs), "passed_files": sum(sample["passed"] for sample in samples),
        "failed_files": sum(not sample["passed"] for sample in samples),
        "dat_bitmaps": len(bitmaps), "large_dat_bitmaps": sum(b.get("large_bitmap", False) for b in bitmaps),
        "source_pixels_compared": sum(b.get("width", 0) * b.get("height", 0) for b in bitmaps if "pixels" in b),
        "png_bitmaps_exact": sum(not b.get("jpeg_dependency", False) and b.get("pixels", {}).get("premultiplied_argb_exact", False) for b in bitmaps),
        "jpeg_bitmaps_compared": sum(b.get("jpeg_dependency", False) for b in bitmaps),
        "jpeg_maximum_rgb_delta": max((b.get("pixels", {}).get("maximum_rgb_channel_delta", 0) for b in bitmaps if b.get("jpeg_dependency", False)), default=0),
        "missing_image_messages": sum(s.get("warnings", {}).get("missing_image_messages", 0) for s in samples),
        "decode_failure_messages": sum(s.get("warnings", {}).get("decode_failure_messages", 0) for s in samples),
        "unresolved_images": sum(len(s.get("unresolved_images", [])) for s in samples),
        "source_track_frame_samples": sum(s.get("timeline", {}).get("source_track_frame_samples", 0) for s in samples),
        "source_animation_frames": sum(s.get("timeline", {}).get("source_animation_frames", 0) for s in samples),
        "dom_frame_elements": sum(s.get("timeline", {}).get("dom_frame_elements", 0) for s in samples),
        "actual_multi_frame_motion_tweens": sum(s.get("timeline", {}).get("actual_multi_frame_motion_tweens", 0) for s in samples),
        "motion_tween_frames_covered": sum(s.get("timeline", {}).get("motion_tween_frames_covered", 0) for s in samples),
        "exact_compiled_restores": sum(s.get("restored_compiled_bytes_exact", False) for s in samples),
        "seconds_for_correctness_checks": time.monotonic() - started,
    }
    result["binary_unchanged_during_validation"] = sha256(args.rust) == result["binary"]["sha256"]
    result["resource_manifest_unchanged_during_validation"] = sha256(args.resource_root / "resources.xml") == result["resource_manifest"]["sha256"]
    result["passed"] = (result["summary"]["failed_files"] == 0
                        and result["summary"]["actual_multi_frame_motion_tweens"] > 0
                        and result["binary_unchanged_during_validation"]
                        and result["resource_manifest_unchanged_during_validation"])
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(result["summary"], ensure_ascii=False, indent=2), flush=True)
    raise SystemExit(0 if result["passed"] else 1)


if __name__ == "__main__":
    main()
