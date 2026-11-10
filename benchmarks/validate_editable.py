"""Independently validate normal compiled -> editable FLA -> compiled conversion.

The source fingerprint is removed and one visible DAT texel is changed before
normal compilation. This never uses unchanged-source restoration as evidence.
The Python DefMap reader and Pillow checks are independent of the Rust decoder;
they are not a claim that this script is the game's resource loader.

Example (converter directory; use a frozen copy of the release binary):
  python benchmarks/validate_editable.py --rust .qa/candidate/fla-reanim-cli.exe \
    --resource-root /path/to/game/Resources --output benchmarks/results/editable-validation.json

Python, Pillow and psutil are verification dependencies only, not shipped runtimes.
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict, deque
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path, PurePosixPath
import platform
import re
import struct
import subprocess
import tempfile
import time
import traceback
import xml.etree.ElementTree as ET
import zipfile
import zlib

from PIL import Image, __version__ as pillow_version
from compare import bitmap_dat, compiled, find_image, image_catalog, sha256
from validate_corpus import local_name


CHANNELS = ["x_px", "y_px", "kx_degrees", "ky_degrees", "scale_x", "scale_y", "frame", "alpha"]
SOURCE_HASH = "META-INF/reanim-source.sha256"


def write_report(path, result):
    """Publish complete JSON atomically, tolerating brief Windows reader locks."""
    data = json.dumps(result, ensure_ascii=False, indent=2)
    descriptor, name = tempfile.mkstemp(prefix=path.name + ".", suffix=".tmp", dir=path.parent)
    temporary = Path(name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            handle.write(data)
        for attempt in range(10):
            try:
                temporary.replace(path)
                break
            except OSError:
                if attempt == 9:
                    raise
                time.sleep(.1)
    finally:
        temporary.unlink(missing_ok=True)


def run(command, timeout):
    result = subprocess.run(command, capture_output=True, timeout=timeout,
                            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    stdout, stderr = (value.decode("utf-8", "replace") for value in (result.stdout, result.stderr))
    if result.returncode:
        raise RuntimeError(f"CLI exit {result.returncode}: {stderr or stdout}")
    lines = [line for line in stdout.splitlines() if line.strip()]
    return {"command": command, "report": json.loads(lines[-1]), "stderr": stderr}


def angle_error(a, b):
    return abs((a - b + 180.) % 360. - 180.)


def canonical(values):
    values = list(values)
    for scale, angle in [(4, 2), (5, 3)]:
        if values[scale] < 0:
            values[scale] = -values[scale]
            values[angle] += 180.
    return values


def affine(values):
    x, y, kx, ky, sx, sy, _, _ = values
    kx, ky = math.radians(kx), math.radians(ky)
    return [sx * math.cos(kx), sx * math.sin(kx), -sy * math.sin(ky), sy * math.cos(ky), x, y]


def bounds(track):
    active = [i for i, frame in enumerate(track["transforms"]) if frame["values"][6] >= 0]
    first, last = (active[0], active[-1]) if active else (0, 0)
    return {"first": first, "last": last, "count": last - first + 1,
            "active_frames": len(active), "empty_track_fallback": not active}


def empty_stats():
    return {"frames": 0, "maximum_by_channel": [0.] * 8,
            "sum_by_channel": [0.] * 8, "over_tolerance_by_channel": [0] * 8,
            "worst_by_channel": [None] * 8,
            "maximum_affine_coefficient_error": [0.] * 6}


def add_stats(stats, a, b, limits, use_canonical, track, frame):
    va, vb = (canonical(a), canonical(b)) if use_canonical else (a, b)
    errors = [angle_error(x, y) if i in (2, 3) else abs(x - y)
              for i, (x, y) in enumerate(zip(va, vb))]
    # Rotation of a zero-length axis has no visual or attachment-matrix effect.
    if use_canonical:
        for angle, scale in [(2, 4), (3, 5)]:
            if abs(va[scale]) < 1e-8 and abs(vb[scale]) < 1e-8:
                errors[angle] = 0.
    stats["frames"] += 1
    for i, error in enumerate(errors):
        if error > stats["maximum_by_channel"][i]:
            stats["worst_by_channel"][i] = {"track": track, "frame": frame,
                "source_raw": a[i], "output_raw": b[i], "error": error}
        stats["maximum_by_channel"][i] = max(stats["maximum_by_channel"][i], error)
        stats["sum_by_channel"][i] += error
        stats["over_tolerance_by_channel"][i] += error > limits[i]
    for i, (x, y) in enumerate(zip(affine(a), affine(b))):
        stats["maximum_affine_coefficient_error"][i] = max(
            stats["maximum_affine_coefficient_error"][i], abs(x - y))


def finish_stats(stats):
    stats["mean_by_channel"] = [value / stats["frames"] if stats["frames"] else 0.
                                for value in stats.pop("sum_by_channel")]
    return stats


def compare_definitions(source, output, limits, cells):
    source_names = [track["name"] for track in source["tracks"]]
    output_names = [track["name"] for track in output["tracks"]]
    result = {"fps_equal": source["fps"] == output["fps"], "source_fps": source["fps"],
              "output_fps": output["fps"], "track_names_in_order_equal": source_names == output_names,
              "source_track_names": source_names, "output_track_names": output_names,
              "missing_tracks": list((Counter(source_names) - Counter(output_names)).elements()),
              "extra_tracks": list((Counter(output_names) - Counter(source_names)).elements()),
              "track_frame_count_mismatches": [], "visibility_mask_mismatches": 0,
              "effective_image_cel_mismatches": 0, "field_differences": {},
              "anim_ranges": [], "examples": []}
    fields = {name: {"all_frames": 0, "both_active": 0, "both_hidden": 0}
              for name in ("image", "font", "text")}
    stats = {name: empty_stats() for name in ("all_raw", "active_raw", "active_canonical", "hidden_raw")}
    by_name = defaultdict(deque)
    for track in output["tracks"]:
        by_name[track["name"]].append(track)
    for ta in source["tracks"]:
        if not by_name[ta["name"]]:
            continue
        tb = by_name[ta["name"]].popleft()
        if len(ta["transforms"]) != len(tb["transforms"]):
            result["track_frame_count_mismatches"].append({"name": ta["name"],
                "source": len(ta["transforms"]), "output": len(tb["transforms"])})
        if ta["name"].lower().startswith("anim_"):
            a, b = bounds(ta), bounds(tb)
            result["anim_ranges"].append({"name": ta["name"], "source": a, "output": b,
                "first_last_count_equal": all(a[key] == b[key] for key in ("first", "last", "count"))})
        for frame, (fa, fb) in enumerate(zip(ta["transforms"], tb["transforms"])):
            va, vb = fa["values"], fb["values"]
            active_a, active_b = va[6] >= 0., vb[6] >= 0.
            active, hidden = active_a and active_b, not active_a and not active_b
            if active_a != active_b:
                result["visibility_mask_mismatches"] += 1
                if len(result["examples"]) < 20:
                    result["examples"].append({"track": ta["name"], "frame": frame, "field": "f>=0",
                        "source": va[6], "output": vb[6]})
            add_stats(stats["all_raw"], va, vb, limits, False, ta["name"], frame)
            if active:
                add_stats(stats["active_raw"], va, vb, limits, False, ta["name"], frame)
                add_stats(stats["active_canonical"], va, vb, limits, True, ta["name"], frame)
                if fa["image"] and fa["image"] == fb["image"]:
                    columns = cells.get(fa["image"].upper(), (1, 1))[0]
                    result["effective_image_cel_mismatches"] += (
                        math.floor(va[6] + .5) % columns != math.floor(vb[6] + .5) % columns)
            elif hidden:
                add_stats(stats["hidden_raw"], va, vb, limits, False, ta["name"], frame)
            for field in fields:
                if fa[field] != fb[field]:
                    fields[field]["all_frames"] += 1
                    fields[field]["both_active"] += active
                    fields[field]["both_hidden"] += hidden
                    if len(result["examples"]) < 20:
                        result["examples"].append({"track": ta["name"], "frame": frame, "field": field,
                            "source": fa[field], "output": fb[field], "both_active": active})
    result["field_differences"] = fields
    result["transform_errors"] = {name: finish_stats(value) for name, value in stats.items()}
    # Positive f values select rounded sprite cells; their raw numeric distance
    # is reported separately, but visibility and the selected cel are enforced.
    errors = stats["active_canonical"]["over_tolerance_by_channel"]
    result["active_parameter_limits_pass"] = not any(errors[i] for i in (0, 1, 2, 3, 4, 5, 7))
    result["passed"] = (result["fps_equal"] and result["track_names_in_order_equal"]
        and not result["track_frame_count_mismatches"] and not result["visibility_mask_mismatches"]
        and not result["effective_image_cel_mismatches"] and result["active_parameter_limits_pass"]
        and all(not field["both_active"] for field in fields.values())
        and all(item["first_last_count_equal"] for item in result["anim_ranges"]))
    return result


def edit_dat(data):
    width, height, pixels = bitmap_dat(data)
    pixels = bytearray(pixels)
    position = next((i for i in range(0, len(pixels), 4) if pixels[i]), None)
    if position is None:
        return None
    before = list(pixels[position:position + 4])
    old_alpha = pixels[position]
    new_alpha = old_alpha - 1 if old_alpha > 1 else 2
    for channel in range(1, 4):
        pixels[position + channel] = min(new_alpha,
            (pixels[position + channel] * new_alpha + old_alpha // 2) // old_alpha)
    pixels[position] = new_alpha
    pixels[position + 1] = (pixels[position + 1] + 1) % (new_alpha + 1)
    stride = struct.unpack_from("<H", data, 2)[0]
    raw = b"".join(pixels[y * width * 4:(y + 1) * width * 4] + bytes(stride - width * 4)
                   for y in range(height))
    compressed = zlib.compress(raw, 6)
    updated = bytearray(data[:26])
    for start in range(0, len(compressed), 60000):
        chunk = compressed[start:start + 60000]
        updated.extend(struct.pack("<H", len(chunk)))
        updated.extend(chunk)
    updated.extend(b"\0\0")
    return bytes(updated), {"pixel_xy": [(position // 4) % width, (position // 4) // width],
        "before_argb": before, "after_argb": list(pixels[position:position + 4])}


def prepare_editable(original, output):
    with zipfile.ZipFile(original) as archive:
        entries = {name: archive.read(name) for name in archive.namelist() if not name.endswith("/")}
    existed = entries.pop(SOURCE_HASH, None) is not None
    if not existed:
        raise ValueError("Reverse output lacks source.sha256; cannot demonstrate its removal")
    document = ET.fromstring(entries["DOMDocument.xml"])
    edit = None
    for item in document.iter():
        if local_name(item) != "DOMBitmapItem" or not item.get("bitmapDataHRef"):
            continue
        member = "bin/" + item.get("bitmapDataHRef")
        changed = edit_dat(entries[member])
        if changed is not None:
            import hashlib
            data, details = changed
            edit = {"member": member, "library_item": item.get("name"), **details,
                    "original_sha256": hashlib.sha256(entries[member]).hexdigest(),
                    "edited_sha256": hashlib.sha256(data).hexdigest()}
            entries[member] = data
            break
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in entries.items():
            archive.writestr(name, data)
    return {"source_sha256_removed": existed, "source_compiled_preserved_for_asset_metadata":
            "META-INF/reanim-source.compiled" in entries, "dat_pixel_edit": edit,
            "stage": [document.get("width"), document.get("height")]}


def resource_path(catalog, identifier):
    return catalog.get(identifier.upper()) or catalog.get(re.sub("[^A-Z0-9]+", "_", identifier.upper()).strip("_"))


def pixel_check(argb, rgba):
    differences = alpha_errors = maximum = 0
    for start in range(0, len(argb), 4):
        a, r, g, b = argb[start:start + 4]
        rr, gg, bb, aa = rgba[start:start + 4]
        expected = [aa, (rr * aa + 127) // 255, (gg * aa + 127) // 255, (bb * aa + 127) // 255]
        original = [a, r, g, b]
        differences += original != expected
        alpha_errors += a != aa
        maximum = max(maximum, *(abs(x - y) for x, y in zip(original, expected)))
    return {"exact_premultiplied_argb": differences == 0, "different_pixels": differences,
            "alpha_mismatches": alpha_errors, "maximum_channel_delta": maximum}


def game_resources(root):
    """Independent lookup order from DefinitionLoadImage, not Rust Catalog."""
    paths = image_catalog(root)
    # Preserve raw names, including spaces and repeated underscores, for the
    # game's direct filename fallbacks. The legacy helper also indexes aliases.
    for folder, prefix in [("reanim", "IMAGE_REANIM_"), ("images", "IMAGE_REANIM_"), ("particles", "IMAGE_")]:
        for path in sorted((root / folder).rglob("*")):
            if path.is_file() and path.suffix.lower() in (".png", ".jpg", ".jpeg"):
                paths.setdefault(prefix + path.stem.upper(), path)
    declared, cells = {}, {}
    folder = prefix = ""
    for item in ET.parse(root / "resources.xml").getroot().iter():
        if local_name(item) == "SetDefaults":
            folder, prefix = item.get("path", folder), item.get("idprefix", prefix)
        elif local_name(item) == "Image" and item.get("path"):
            relative = Path(folder.replace("\\", "/")) / item.get("path").replace("\\", "/")
            identifier = (prefix + item.get("id", relative.stem)).upper()
            cells.setdefault(identifier, (int(item.get("cols", "1")), int(item.get("rows", "1"))))
            path = find_image(root / relative)
            if path is not None:
                declared.setdefault(identifier, path)
    paths.update(declared)
    # The real C++ loader explicitly tries this manifest alias before scanning
    # reanim/images. TreeFood has different files in both folders, so order is
    # observable and cannot be replaced by a filename heuristic.
    for identifier, path in declared.items():
        if identifier.startswith("IMAGE_") and not identifier.startswith("IMAGE_REANIM_"):
            alias = "IMAGE_REANIM_" + identifier[len("IMAGE_"):]
            if alias not in declared:
                paths[alias] = path
                if alias not in cells and identifier in cells:
                    cells[alias] = cells[identifier]
    return paths, cells


def bitmap_resource(item):
    stem = PurePosixPath(item.get("name", "").replace("\\", "/")).stem
    cell = re.fullmatch(r"(.+)__REANIM_CEL_(\d+)", stem)
    cel = int(cell.group(2)) if cell else None
    bare = cell.group(1) if cell else stem
    identifier = bare if bare.startswith("IMAGE_") else "IMAGE_REANIM_" + bare.upper()
    for property in item.iter():
        if (local_name(property) == "PD" and property.get("n") == "fla-reanim.resource-id"
                and property.get("t") == "s" and property.get("v")):
            identifier = property.get("v")
            pd_cell = re.fullmatch(r"(.+)__REANIM_CEL_(\d+)", identifier)
            if pd_cell:
                identifier = pd_cell.group(1)
                if cel is None:
                    cel = int(pd_cell.group(2))
            break
    return identifier, cel


def alpha_mask_check(source, image_path, output_root, resource_root):
    """Model overlay deployment: an unexported original mask remains installed."""
    mask = (find_image(source.with_name("_" + source.stem))
            or find_image(source.with_name(source.stem + "_")))
    if mask is None:
        return None
    relative = mask.relative_to(resource_root)
    exported = find_image(output_root / relative.with_suffix(""))
    deployed = exported or mask
    with Image.open(image_path) as image, Image.open(deployed) as alpha:
        if image.size != alpha.size:
            return {"passed": False, "error": "External alpha mask dimensions differ",
                    "source_mask_relative_path": relative.as_posix(), "exported": exported is not None}
        expected, actual = image.convert("RGBA").getchannel("A").tobytes(), alpha.convert("RGB").getchannel("B").tobytes()
    errors = [abs(a - b) for a, b in zip(expected, actual)]
    return {"source_mask_relative_path": relative.as_posix(), "deployed_mask_path": str(deployed),
            "exported": exported is not None, "different_alpha_pixels": sum(error != 0 for error in errors),
            "maximum_blue_channel_error": max(errors, default=0), "passed": not any(errors)}


def validate_assets(fla, compiled_path, definition, resource_root, catalog, cells, edit):
    manifest_path = compiled_path.with_suffix(".assets.json")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8")) if manifest_path.exists() else {}
    records, errors = [], []
    identifiers = {frame["image"] for track in definition["tracks"] for frame in track["transforms"]
                   if frame["values"][6] >= 0 and frame["image"]}
    paths = {}
    for identifier, relative in manifest.items():
        posix = PurePosixPath(relative.replace("\\", "/"))
        path = (compiled_path.parent / relative).resolve()
        if posix.is_absolute() or ".." in posix.parts or not path.is_relative_to(compiled_path.parent):
            errors.append({"id": identifier, "error": "Unsafe exported asset path"})
            continue
        paths[identifier] = path
        source = resource_path(catalog, identifier)
        expected = source.relative_to(resource_root) if source else None
        record = {"id": identifier, "emitted_relative_path": relative, "file_exists": path.is_file(),
                  "source_resource_path": str(expected) if expected else None,
                  "source_resource_resolved": source is not None,
                  "deployment_stem_matches_resource_lookup": bool(expected and
                    posix.with_suffix("").as_posix().casefold() == expected.with_suffix("").as_posix().casefold())}
        if path.is_file():
            record["sha256"] = sha256(path)
            if source is not None:
                record["external_alpha_mask"] = alpha_mask_check(source, path, compiled_path.parent, resource_root)
        records.append(record)
    def manifest_id(identifier):
        if identifier in paths:
            return identifier
        source = resource_path(catalog, identifier)
        return next((candidate for candidate in paths if source is not None
                     and resource_path(catalog, candidate) == source), None)

    aliases = {identifier: manifest_id(identifier) for identifier in identifiers
               if identifier not in paths and manifest_id(identifier) is not None}
    missing = sorted(identifier for identifier in identifiers if manifest_id(identifier) is None)
    bitmaps, edited_verified = [], False
    image_cache = {}
    with zipfile.ZipFile(fla) as archive:
        document = ET.fromstring(archive.read("DOMDocument.xml"))
        for item in document.iter():
            if local_name(item) != "DOMBitmapItem" or not item.get("bitmapDataHRef"):
                continue
            member = "bin/" + item.get("bitmapDataHRef")
            width, height, argb = bitmap_dat(archive.read(member))
            identifier, cel = bitmap_resource(item)
            record = {"id": identifier, "library_item": item.get("name"), "dat_member": member,
                      "width": width, "height": height, "cel": cel, "is_edited_dat": bool(edit and edit["member"] == member)}
            key = manifest_id(identifier)
            record["export_manifest_id"] = key
            path = paths.get(key)
            if path is None or not path.is_file():
                record["error"] = "No exported image for editable DAT"
            else:
                if path not in image_cache:
                    with Image.open(path) as image:
                        image_cache[path] = image.convert("RGBA")
                image = image_cache[path]
                if cel is not None:
                    columns, rows = cells.get(identifier.upper(), (1, 1))
                    cw, ch = image.width // columns, image.height // rows
                    image = image.crop((cel * cw, 0, (cel + 1) * cw, ch))
                if image.size != (width, height):
                    record["error"] = f"Exported image/cel dimensions {image.size} differ from DAT {(width, height)}"
                else:
                    record["pixels"] = pixel_check(argb, image.tobytes())
                    if record["is_edited_dat"]:
                        edited_verified = record["pixels"]["exact_premultiplied_argb"]
            bitmaps.append(record)
    passed = (not errors and not missing and all(r["file_exists"] and r["deployment_stem_matches_resource_lookup"]
              and (r.get("external_alpha_mask") is None or r["external_alpha_mask"]["passed"]) for r in records)
              and all(not r.get("error") and r.get("pixels", {}).get("exact_premultiplied_argb") for r in bitmaps)
              and (edit is None or edited_verified))
    return {"manifest_exists": manifest_path.exists(), "manifest_sha256": sha256(manifest_path) if manifest_path.exists() else None,
            "missing_active_image_ids_in_manifest": missing, "path_records": records, "errors": errors,
            "resource_id_aliases_resolved_to_same_source_path": aliases,
            "dat_comparisons": bitmaps, "edited_dat_export_verified": edited_verified,
            "game_loader_executed": False, "passed": passed}


def validate_one(source, directory, args, catalog, cells, limits):
    directory.mkdir(parents=True)
    original = directory / "reverse.fla"
    editable = directory / "edited-no-fingerprint.fla"
    output = directory / "normal" / source.name
    command = [str(args.rust), "--resource-root", str(args.resource_root), "-o", str(original)]
    if args.strict_tweens:
        command.append("--strict-tweens")
    sample = {"input": str(source), "input_sha256": sha256(source), "input_bytes": source.stat().st_size}
    started = time.monotonic()
    sample["reverse"] = run(command + [str(source)], args.timeout)
    sample["editable_preparation"] = prepare_editable(original, editable)
    sample["editable_sha256"] = sha256(editable)
    sample["normal_compile"] = run([str(args.rust), "--resource-root", str(args.resource_root),
                                    "-o", str(output), str(editable)], args.timeout)
    sample["source_restore_branch_used"] = "文档未编辑" in sample["normal_compile"]["report"].get("detail", "")
    a, raw_a = compiled(source)
    b, raw_b = compiled(output)
    sample["output_sha256"] = sha256(output)
    sample["output_is_rpls"] = output.read_bytes().endswith(b"RPLS")
    sample["decompressed_bytes_equal"] = raw_a == raw_b
    sample["definition"] = compare_definitions(a, b, limits, cells)
    sample["assets"] = validate_assets(editable, output, b, args.resource_root, catalog, cells,
                                       sample["editable_preparation"]["dat_pixel_edit"])
    sample["seconds_for_correctness_checks"] = time.monotonic() - started
    sample["passed"] = (sample["definition"]["passed"] and sample["assets"]["passed"]
                        and not sample["source_restore_branch_used"] and not sample["output_is_rpls"])
    return sample


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--resource-root", type=Path, required=True)
    parser.add_argument("--game-source", type=Path,
                        help="Path to TodLib/Definition.cpp; defaults to resource-root.parent/TodLib/Definition.cpp")
    parser.add_argument("--compiled-dir", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--strict-tweens", action="store_true")
    parser.add_argument("--only", nargs="+", help="Optional source basenames without .reanim.compiled")
    parser.add_argument("--limit", type=int)
    args = parser.parse_args()
    args.rust = args.rust.resolve(strict=True)
    args.resource_root = args.resource_root.resolve(strict=True)
    game_source = (args.game_source or args.resource_root.parent / "TodLib/Definition.cpp").resolve(strict=True)
    args.output = args.output.resolve()
    inputs = sorted((args.compiled_dir or args.resource_root / "compiled/reanim").glob("*.compiled"))
    if args.only:
        wanted = {name.casefold() for name in args.only}
        inputs = [p for p in inputs if p.name.removesuffix(".reanim.compiled").casefold() in wanted]
    if args.limit is not None:
        inputs = inputs[:args.limit]
    if not inputs:
        parser.error("No compiled inputs matched")
    if args.work_dir:
        args.work_dir.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="fla-editable-validation-", dir=args.work_dir)).resolve()
    # Reverse parameter bounds plus half the normal compiler's .1/.001 grid.
    limits = ([.1001, .1001, .0701, .0701, .00101, .00101, 1e-6, .00151]
              if args.strict_tweens else [.3001, .3001, .1501, .1501, .00251, .00251, 1e-6, .00451])
    result = {"schema_version": 1, "created_utc": datetime.now(timezone.utc).isoformat(),
        "completed": False, "requested_files": len(inputs),
        "purpose": "normal editable conversion correctness, not snapshot restoration or a performance benchmark",
        "platform": platform.platform(), "python": platform.python_version(), "Pillow": pillow_version,
        "binary": {"path": str(args.rust), "sha256": sha256(args.rust), "bytes": args.rust.stat().st_size},
        "resource_root": str(args.resource_root), "resource_manifest_sha256": sha256(args.resource_root / "resources.xml"),
        "validator_sha256": sha256(Path(__file__)), "shared_reader_sha256": sha256(Path(__file__).with_name("compare.py")),
        "local_xml_helper_sha256": sha256(Path(__file__).with_name("validate_corpus.py")),
        "game_definition_source_path": str(game_source), "game_definition_source_sha256": sha256(game_source),
        "work_dir": str(work), "strict_tweens": args.strict_tweens, "channels": CHANNELS,
        "active_canonical_parameter_tolerances": limits,
        "policies": {"angle": "modulo 360; negative scale canonicalized by a 180-degree axis rotation",
            "hidden_frames": "report differences without enforcing unused f<0 pose, cached image/font/text; visibility masks and every subsequent active frame must match",
            "frame_number": "report raw differences; enforce f>=0 masks and rounded modulo-columns image cell",
            "assets": "one nontransparent DAT texel changes both alpha and red per FLA where possible; exported PNG must match edited DAT premultiplied ARGB exactly, and any deployed external mask blue channel must equal the new PNG alpha",
            "resource_paths": "emitted extension-free path must match independently resolved source resource path; case ignored for Windows",
            "resource_lookup_order": "explicit manifest ID, then IMAGE_REANIM_X -> explicit IMAGE_X manifest alias, then filename fallbacks; resource-id PersistentData preserves literal compiled IDs",
            "sampling": "compiled integer frames; canonical parameter equality alone does not certify every subframe render",
            "game_loader": "not executed by this script; independent DefMap parsing is not game-loader acceptance"},
        "samples": []}
    catalog, cells = game_resources(args.resource_root)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    print(f"Validating {len(inputs)} normal editable conversions in {work}", flush=True)
    for index, source in enumerate(inputs):
        try:
            sample = validate_one(source, work / f"{index:04}-{source.stem}", args, catalog, cells, limits)
        except Exception as error:
            sample = {"input": str(source), "input_sha256": sha256(source), "passed": False,
                      "error": str(error), "traceback": traceback.format_exc()}
        result["samples"].append(sample)
        if not sample["passed"]:
            print(f"FAIL {source.name}: {sample.get('error', {'definition': sample.get('definition', {}).get('passed'), 'assets': sample.get('assets', {}).get('passed')})}", flush=True)
        if (index + 1) % 10 == 0 or index + 1 == len(inputs):
            print(f"Checked {index + 1}/{len(inputs)}", flush=True)
        write_report(args.output, result)
    samples = result["samples"]
    comparisons = [sample["definition"] for sample in samples if "definition" in sample]
    assets = [sample["assets"] for sample in samples if "assets" in sample]
    result["summary"] = {"input_files": len(inputs), "passed_files": sum(s["passed"] for s in samples),
        "failed_files": sum(not s["passed"] for s in samples), "process_failures": sum("error" in s for s in samples),
        "definitions_compared": len(comparisons), "ordered_track_names_equal_files": sum(d["track_names_in_order_equal"] for d in comparisons),
        "frame_count_mismatch_tracks": sum(len(d["track_frame_count_mismatches"]) for d in comparisons),
        "visibility_mask_mismatches": sum(d["visibility_mask_mismatches"] for d in comparisons),
        "animation_range_mismatches": sum(not r["first_last_count_equal"] for d in comparisons for r in d["anim_ranges"]),
        "effective_image_cel_mismatches": sum(d["effective_image_cel_mismatches"] for d in comparisons),
        "active_field_differences": {key: sum(d["field_differences"][key]["both_active"] for d in comparisons) for key in ("image", "font", "text")},
        "active_canonical_maximum_by_channel": [max((d["transform_errors"]["active_canonical"]["maximum_by_channel"][i] for d in comparisons), default=0.) for i in range(8)],
        "active_parameter_limit_failures": sum(not d["active_parameter_limits_pass"] for d in comparisons),
        "edited_dat_files_verified": sum(a["edited_dat_export_verified"] for a in assets),
        "edited_dat_files_attempted": sum(s.get("editable_preparation", {}).get("dat_pixel_edit") is not None for s in samples),
        "dat_pixel_comparisons": sum(len(a["dat_comparisons"]) for a in assets),
        "dat_pixel_comparison_failures": sum(bool(r.get("error")) or not r.get("pixels", {}).get("exact_premultiplied_argb") for a in assets for r in a["dat_comparisons"]),
        "external_alpha_mask_checks": sum(r.get("external_alpha_mask") is not None for a in assets for r in a["path_records"]),
        "external_alpha_mask_failures": sum(r.get("external_alpha_mask") is not None and not r["external_alpha_mask"]["passed"] for a in assets for r in a["path_records"]),
        "missing_active_asset_manifest_ids": sum(len(a["missing_active_image_ids_in_manifest"]) for a in assets),
        "asset_deployment_path_mismatches": sum(not r["deployment_stem_matches_resource_lookup"] for a in assets for r in a["path_records"]),
        "asset_deployment_mismatched_ids": sorted({r["id"] for a in assets for r in a["path_records"] if not r["deployment_stem_matches_resource_lookup"]}),
        "restoration_branch_used": sum(s.get("source_restore_branch_used", False) for s in samples),
        "rpls_outputs": sum(s.get("output_is_rpls", False) for s in samples)}
    result["binary_unchanged_during_validation"] = sha256(args.rust) == result["binary"]["sha256"]
    result["resource_manifest_unchanged_during_validation"] = sha256(args.resource_root / "resources.xml") == result["resource_manifest_sha256"]
    result["completed"] = True
    result["passed"] = (not result["summary"]["failed_files"] and result["binary_unchanged_during_validation"]
                        and result["resource_manifest_unchanged_during_validation"])
    write_report(args.output, result)
    print(json.dumps(result["summary"], ensure_ascii=False, indent=2), flush=True)
    raise SystemExit(0 if result["passed"] else 1)


if __name__ == "__main__":
    main()
