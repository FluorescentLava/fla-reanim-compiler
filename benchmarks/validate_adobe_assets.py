"""Check original Adobe FLA bitmap exports against an independent decoder.

Run with a frozen Rust release; private source FLAs are identified by SHA256
and are never modified or redistributed. This is a correctness check, not a
performance benchmark. Python/Pillow are verification dependencies only.

Example from the converter directory:
  python benchmarks/validate_adobe_assets.py --rust .qa/release/fla-reanim-cli.exe \
    --sample-dir /path/to/private/FLAs --resource-root /path/to/game/Resources \
    --output benchmarks/results/adobe-bitmap-export.json
"""
from __future__ import annotations

import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path, PurePosixPath
import platform
import subprocess
import tempfile
import traceback
import xml.etree.ElementTree as ET

from PIL import Image, __version__ as pillow_version
from compare import (bitmap_dat, bitmap_resource, fla_archive, image_catalog,
                     manifest_images, sha256)
from validate_editable import alpha_mask_check, write_report


SAMPLES = ["Chomper", "Zombie_torch", "Zombie", "Zombie_polevaulter", "SelectorScreen"]


def local_name(node):
    return node.tag.rsplit("}", 1)[-1]


def straight_rgba(argb):
    """Independent integer conversion from Adobe's premultiplied pixels."""
    pixels = bytearray()
    for offset in range(0, len(argb), 4):
        alpha, red, green, blue = argb[offset:offset + 4]
        pixels.extend(min(255, (channel * 255 + alpha // 2) // alpha) if alpha else 0
                      for channel in (red, green, blue))
        pixels.append(alpha)
    return bytes(pixels)


def exported_paths(output, catalog, resource_root):
    manifest_path = output.with_suffix(".assets.json")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    paths, records = {}, []
    for identifier, relative in manifest.items():
        name = PurePosixPath(relative.replace("\\", "/"))
        path = (output.parent / relative).resolve()
        if name.is_absolute() or ".." in name.parts or not path.is_relative_to(output.parent):
            raise ValueError(f"Unsafe exported asset path: {relative}")
        source = catalog.get(identifier.upper())
        expected = source.relative_to(resource_root) if source else None
        path_matches = (name.with_suffix("").as_posix().casefold()
                        == expected.with_suffix("").as_posix().casefold()) if expected else None
        mask = alpha_mask_check(source, path, output.parent, resource_root) if source and path.is_file() else None
        record = {"id": identifier, "relative_path": relative, "exists": path.is_file(),
                  "source_resource_path": expected.as_posix() if expected else None,
                  "deployment_path_matches_known_game_resource": path_matches,
                  "external_alpha_mask": mask,
                  "passed": path.is_file() and path_matches is not False and (mask is None or mask["passed"])}
        if path.is_file():
            record["sha256"] = sha256(path)
        records.append(record)
        paths[identifier] = path
    return paths, records, sha256(manifest_path)


def validate_bitmap(item, archive, paths, catalog, cells, jpeg_tolerance):
    member = "bin/" + item.get("bitmapDataHRef")
    data = archive.read(member)
    encoding = ("direct_jpeg" if data.startswith(b"\xff\xd8") else
                "direct_png" if data.startswith(b"\x89PNG") else
                "raw_argb" if len(data) >= 26 and data[25] == 0 else "chunked_zlib_argb")
    width, height, argb = bitmap_dat(data)
    expected = straight_rgba(argb)
    identifier, cel = bitmap_resource(item)
    key = next((key for key in paths if key.upper() == identifier.upper()), None)
    source = catalog.get(identifier.upper())
    if key is None and source is not None:
        key = next((key for key in paths if catalog.get(key.upper()) == source), None)
    record = {"library_item": item.get("name"), "dat_member": member,
              "source_bitmap_sha256": hashlib.sha256(data).hexdigest(), "encoding": encoding,
              "resource_id": identifier, "manifest_id": key, "cel": cel,
              "width": width, "height": height,
              "expected_straight_rgba_sha256": hashlib.sha256(expected).hexdigest()}
    if key is None:
        raise ValueError(f"No exported image for library bitmap {identifier}")
    path = paths[key]
    with Image.open(path) as image:
        if image.format != "PNG":
            raise ValueError(f"Exported editable asset is not PNG: {path}")
        actual = image.convert("RGBA")
        if cel is not None:
            columns, rows = cells.get(identifier.upper(), (1, 1))
            cell_width, cell_height = actual.width // columns, actual.height // rows
            actual = actual.crop((cel * cell_width, 0, (cel + 1) * cell_width, cell_height))
        pixels = actual.tobytes()
        record["dimensions_equal"] = actual.size == (width, height)
    if len(pixels) != len(expected):
        record.update({"passed": False, "error": "Exported PNG/cel dimensions differ"})
        return record
    maximum = max((abs(a - b) for index, (a, b) in enumerate(zip(pixels, expected))
                   if index % 4 != 3), default=0)
    alpha_errors = sum(a != b for a, b in zip(pixels[3::4], expected[3::4]))
    tolerance = jpeg_tolerance if encoding == "direct_jpeg" else 0
    record.update({"actual_straight_rgba_sha256": hashlib.sha256(pixels).hexdigest(),
                   "rgba_bytes_equal": pixels == expected,
                   "maximum_rgb_channel_delta": maximum, "alpha_mismatches": alpha_errors,
                   "rgb_tolerance": tolerance,
                   "passed": record["dimensions_equal"] and maximum <= tolerance and alpha_errors == 0})
    return record


def validate_one(source, directory, args, catalog, cells):
    output = directory / (source.stem + ".reanim.compiled")
    command = [str(args.rust), "--resource-root", str(args.resource_root), "-o", str(output), str(source)]
    sample = {"input": str(source), "input_sha256": sha256(source),
              "input_bytes": source.stat().st_size, "command": command, "bitmaps": []}
    process = subprocess.run(command, capture_output=True, timeout=args.timeout,
                             creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    sample.update({"exit_code": process.returncode,
                   "stdout": process.stdout.decode("utf-8", "replace"),
                   "stderr": process.stderr.decode("utf-8", "replace")})
    if process.returncode:
        sample.update({"passed": False, "error": "Rust compilation failed"})
        return sample
    sample["conversion_report"] = json.loads(sample["stdout"].splitlines()[-1])
    sample["output_sha256"] = sha256(output)
    paths, sample["exports"], sample["manifest_sha256"] = exported_paths(output, catalog, args.resource_root)
    with fla_archive(source) as archive:
        document = ET.fromstring(archive.read("DOMDocument.xml"))
        for item in document.iter():
            if local_name(item) != "DOMBitmapItem" or not item.get("bitmapDataHRef"):
                continue
            try:
                sample["bitmaps"].append(validate_bitmap(item, archive, paths, catalog, cells, args.jpeg_tolerance))
            except Exception as error:
                sample["bitmaps"].append({"library_item": item.get("name"), "passed": False,
                                           "error": str(error), "traceback": traceback.format_exc()})
    sample["input_unchanged"] = sha256(source) == sample["input_sha256"]
    sample["passed"] = (bool(sample["bitmaps"]) and sample["input_unchanged"]
                        and all(item["passed"] for item in sample["bitmaps"])
                        and all(item["passed"] for item in sample["exports"]))
    return sample


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", required=True, type=Path)
    parser.add_argument("--sample-dir", required=True, type=Path)
    parser.add_argument("--resource-root", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--only", nargs="+", choices=SAMPLES)
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--jpeg-tolerance", type=int, default=4)
    args = parser.parse_args()
    for field in ("rust", "sample_dir", "resource_root"):
        setattr(args, field, getattr(args, field).resolve(strict=True))
    args.output = args.output.resolve()
    if args.work_dir:
        args.work_dir.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="adobe-bitmap-export-", dir=args.work_dir)).resolve()
    helpers = [Path(__file__), Path(__file__).with_name("compare.py"),
               Path(__file__).with_name("validate_editable.py")]
    helper_hashes = {path.name: sha256(path) for path in helpers}
    manifest = args.resource_root / "resources.xml"
    result = {"schema_version": 1, "created_utc": datetime.now(timezone.utc).isoformat(), "completed": False,
              "purpose": "original Adobe FLA bitmap export correctness, not performance timing",
              "platform": platform.platform(), "python": platform.python_version(), "Pillow": pillow_version,
              "binary": {"path": str(args.rust), "sha256": sha256(args.rust), "bytes": args.rust.stat().st_size},
              "helper_sha256": helper_hashes, "resource_manifest_sha256": sha256(manifest),
              "work_dir": str(work), "policies": {
                  "expected_pixels": "independent Python raw/chunked DAT and Pillow direct-image decode; premultiplied ARGB is independently unpremultiplied before comparing exported PNG",
                  "png_raw_and_zlib": "exact straight RGBA bytes and dimensions",
                  "direct_jpeg": f"Pillow/libjpeg versus native Rust JPEG decoder: at most {args.jpeg_tolerance} RGB channel levels, exact alpha",
                  "resource_ids": "PersistentData preferred, original short names accepted; catalog aliases only if the same game source path is independently proven",
                  "resource_paths": "match known game resource paths; unknown authoring-only assets must still have safe manifest paths and exact pixels",
                  "external_masks": "deployment overlay mask blue channel must equal exported PNG alpha",
                  "game_loader": "not executed here; compiled acceptance and original game semantics are verified separately"}, "samples": []}
    catalog = image_catalog(args.resource_root)
    declared, cells = manifest_images(args.resource_root)
    for identifier in list(cells):
        if identifier.startswith("IMAGE_") and not identifier.startswith("IMAGE_REANIM_"):
            alias = "IMAGE_REANIM_" + identifier[len("IMAGE_"):]
            if alias not in declared and catalog.get(alias) == declared.get(identifier):
                cells[alias] = cells[identifier]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for name in args.only or SAMPLES:
        source = args.sample_dir / (name + ".fla")
        try:
            sample = validate_one(source, work / name, args, catalog, cells)
        except Exception as error:
            sample = {"input": str(source), "passed": False, "error": str(error), "traceback": traceback.format_exc()}
        result["samples"].append(sample)
        write_report(args.output, result)
        print(f"{name}: {'PASS' if sample['passed'] else 'FAIL'} ({len(sample.get('bitmaps', []))} bitmaps)", flush=True)
    bitmaps = [item for sample in result["samples"] for item in sample.get("bitmaps", [])]
    exports = [item for sample in result["samples"] for item in sample.get("exports", [])]
    result["summary"] = {"input_files": len(result["samples"]),
                         "passed_files": sum(sample["passed"] for sample in result["samples"]),
                         "bitmap_count": len(bitmaps), "bitmap_encoding_counts": dict(Counter(item.get("encoding", "error") for item in bitmaps)),
                         "bitmap_failures": sum(not item["passed"] for item in bitmaps),
                         "raw_zlib_png_exact": sum(item.get("rgba_bytes_equal", False) for item in bitmaps if item.get("encoding") != "direct_jpeg"),
                         "maximum_jpeg_rgb_delta": max((item.get("maximum_rgb_channel_delta", 0) for item in bitmaps if item.get("encoding") == "direct_jpeg"), default=0),
                         "alpha_mismatches": sum(item.get("alpha_mismatches", 0) for item in bitmaps),
                         "external_alpha_mask_checks": sum(item.get("external_alpha_mask") is not None for item in exports),
                         "external_alpha_mask_failures": sum(item.get("external_alpha_mask") is not None and not item["external_alpha_mask"]["passed"] for item in exports),
                         "deployment_path_mismatches": sum(item.get("deployment_path_matches_known_game_resource") is False for item in exports)}
    result["binary_unchanged"] = sha256(args.rust) == result["binary"]["sha256"]
    result["helpers_unchanged"] = helper_hashes == {path.name: sha256(path) for path in helpers}
    result["resource_manifest_unchanged"] = sha256(manifest) == result["resource_manifest_sha256"]
    result["completed"] = True
    result["passed"] = (all(sample["passed"] for sample in result["samples"])
                        and result["binary_unchanged"] and result["helpers_unchanged"] and result["resource_manifest_unchanged"])
    write_report(args.output, result)
    print(json.dumps(result["summary"], ensure_ascii=False, indent=2), flush=True)
    raise SystemExit(0 if result["passed"] else 1)


if __name__ == "__main__":
    main()
