"""Reproducible process-level comparison with the historical converter.

Python and psutil are test dependencies only. The shipped Rust application does
not load or launch Python. Run --help for baseline restoration and input options.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import math
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import statistics
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import xml.etree.ElementTree as ET
import zipfile
import zlib

import psutil
from PIL import __version__ as pillow_version


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True, encoding="utf-8").strip()


def restore_baseline(project, revision, destination):
    """Extract only benchmark dependencies from committed history, never checkout/reset."""
    destination.mkdir(parents=True, exist_ok=True)
    command = ["git", "-C", str(project), "archive", "--format=tar", revision,
               "tools/fla_reanim_compiler", "scripts/animations"]
    process = subprocess.Popen(command, stdout=subprocess.PIPE)
    try:
        with tarfile.open(fileobj=process.stdout, mode="r|") as archive:
            for member in archive:
                relative = Path(member.name)
                if relative.is_absolute() or ".." in relative.parts or member.issym() or member.islnk():
                    raise ValueError("Unsafe git archive member")
                if member.isdir():
                    (destination / relative).mkdir(parents=True, exist_ok=True)
                elif member.isfile():
                    target = destination / relative
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with archive.extractfile(member) as source, target.open("wb") as output:
                        shutil.copyfileobj(source, output)
    finally:
        process.stdout.close()
    if process.wait() != 0:
        raise RuntimeError("Cannot restore the historical benchmark dependencies")
    return destination


def windows_process_peak(process):
    """Retained Popen handle permits reading root peak even after process exit."""
    if os.name != "nt":
        return None
    import ctypes
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD),
                    ("PeakWorkingSetSize", ctypes.c_size_t), ("WorkingSetSize", ctypes.c_size_t),
                    ("QuotaPeakPagedPoolUsage", ctypes.c_size_t), ("QuotaPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t), ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                    ("PagefileUsage", ctypes.c_size_t), ("PeakPagefileUsage", ctypes.c_size_t)]
    try:
        counters = Counters()
        counters.cb = ctypes.sizeof(counters)
        query = ctypes.WinDLL("psapi").GetProcessMemoryInfo
        query.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
        query.restype = wintypes.BOOL
        if query(wintypes.HANDLE(int(process._handle)), ctypes.byref(counters), counters.cb):
            return counters.PeakWorkingSetSize
    except (AttributeError, OSError, TypeError):
        pass
    return None


def measure(command, cwd, timeout=180):
    """Sample live process-tree working set; also retain Windows per-process peaks.

    Sum of recorded process peaks is not simultaneous memory. Short-lived child
    peaks can be missed; the root lifetime peak is queried through its OS handle.
    The sampled tree number is simultaneous but can miss very short peaks.
    Neither includes the shared system WinUI runtime used by another process.
    """
    creation_flags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        start = time.perf_counter()
        process = psutil.Popen(command, cwd=cwd, stdout=stdout, stderr=stderr,
                               creationflags=creation_flags)
        known = {process.pid: process}
        peaks = {}
        sampled_peak = 0
        timed_out = False
        while True:
            try:
                for child in process.children(recursive=True):
                    known[child.pid] = child
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                pass
            current = 0
            for pid, child in list(known.items()):
                try:
                    info = child.memory_info()
                    current += info.rss
                    peaks[pid] = max(peaks.get(pid, 0), getattr(info, "peak_wset", info.rss))
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    pass
            sampled_peak = max(sampled_peak, current)
            code = process.poll()
            if code is not None:
                break
            if time.perf_counter() - start > timeout:
                timed_out = True
                for child in reversed(list(known.values())):
                    try:
                        child.kill()
                    except psutil.NoSuchProcess:
                        pass
                code = process.wait()
                break
            time.sleep(0.002)
        elapsed = time.perf_counter() - start
        root_peak = windows_process_peak(process)
        if root_peak is not None:
            peaks[process.pid] = max(peaks.get(process.pid, 0), root_peak)
        stdout.seek(0)
        stderr.seek(0)
        output = stdout.read().decode("utf-8", "replace")
        errors = stderr.read().decode("utf-8", "replace")
    return {
        "wall_seconds": elapsed,
        "sampled_tree_peak_rss_bytes": sampled_peak,
        "windows_root_peak_working_set_bytes": root_peak,
        "sum_process_peak_wset_bytes": sum(peaks.values()),
        "processes_observed": len(known),
        "exit_code": code,
        "timed_out": timed_out,
        "stdout": output[-10000:],
        "stderr": errors[-10000:],
    }


def median_runs(runs):
    good = [r for r in runs if r["exit_code"] == 0 and not r["timed_out"] and r.get("output_exists", True)]
    return {"successful_runs": len(good), "attempted_runs": len(runs), **{
        key: statistics.median(r[key] for r in good) if good else None
        for key in ("wall_seconds", "sampled_tree_peak_rss_bytes", "sum_process_peak_wset_bytes")},
        "windows_root_peak_working_set_bytes": statistics.median(r["windows_root_peak_working_set_bytes"] for r in good)
            if good and all(r["windows_root_peak_working_set_bytes"] is not None for r in good) else None}


def compiled(path):
    """Independent reader of the engine's canonical 32-bit compiled layout."""
    data = Path(path).read_bytes()
    if len(data) < 8 or struct.unpack_from("<I", data)[0] != 0xDEADFED4:
        raise ValueError("Invalid DefMap cookie")
    declared = struct.unpack_from("<I", data, 4)[0]
    if declared > 512 * 1024 * 1024:
        raise ValueError("Unreasonably large DefMap")
    decoder = zlib.decompressobj()
    raw = decoder.decompress(data[8:], declared + 1)
    if len(raw) != declared or not decoder.eof:
        raise ValueError("Invalid DefMap compressed size/stream")
    cursor = 0

    def u32():
        nonlocal cursor
        value = struct.unpack_from("<I", raw, cursor)[0]
        cursor += 4
        return value

    def text():
        nonlocal cursor
        length = u32()
        if length > 100000 or cursor + length > len(raw):
            raise ValueError("Invalid compiled string")
        value = raw[cursor:cursor + length].decode("utf-8")
        cursor += length
        return value

    if u32() != 0xB393B4C0:
        raise ValueError("Unexpected reanim schema")
    u32()
    count = u32()
    fps = struct.unpack("<f", struct.pack("<I", u32()))[0]
    if not math.isfinite(fps) or fps <= 0:
        raise ValueError("Invalid compiled FPS")
    u32()
    if u32() != 12 or count > 100000:
        raise ValueError("Invalid track layout/count")
    counts = []
    for _ in range(count):
        u32(); u32(); counts.append(u32())
    tracks = []
    for frames in counts:
        name = text()
        if u32() != 44 or frames > 1000000:
            raise ValueError("Invalid transform layout/count")
        transforms = []
        for _ in range(frames):
            values = list(struct.unpack_from("<8f", raw, cursor))
            if not all(math.isfinite(v) for v in values):
                raise ValueError("Non-finite compiled transform")
            cursor += 44
            transforms.append({"values": values})
        for transform in transforms:
            transform.update(zip(("image", "font", "text"), (text(), text(), text())))
        previous = dict(values=[0., 0., 0., 0., 1., 1., 0., 1.], image="", font="", text="")
        for transform in transforms:
            for i, value in enumerate(transform["values"]):
                if value == -10000:
                    transform["values"][i] = previous["values"][i]
                else:
                    previous["values"][i] = value
            for key in ("image", "font", "text"):
                if not transform[key]:
                    transform[key] = previous[key]
                else:
                    previous[key] = transform[key]
        tracks.append({"name": name, "transforms": transforms})
    if cursor != len(raw):
        raise ValueError(f"Unconsumed DefMap bytes: {len(raw) - cursor}")
    return {"fps": fps, "tracks": tracks}, raw


def compare_compiled(old, new):
    a, raw_a = compiled(old)
    b, raw_b = compiled(new)
    differences = []
    max_error = 0.
    if a["fps"] != b["fps"] or len(a["tracks"]) != len(b["tracks"]):
        differences.append("FPS/track count differs")
    for ta, tb in zip(a["tracks"], b["tracks"]):
        if ta["name"] != tb["name"] or len(ta["transforms"]) != len(tb["transforms"]):
            differences.append(f"Track structure: {ta['name']} / {tb['name']}")
            continue
        for frame, (fa, fb) in enumerate(zip(ta["transforms"], tb["transforms"])):
            for key in ("image", "font", "text"):
                if fa[key] != fb[key]:
                    differences.append(f"{ta['name']} frame {frame} {key}")
            for index, (va, vb) in enumerate(zip(fa["values"], fb["values"])):
                error = abs(va - vb)
                if index in (2, 3):
                    error = abs((va - vb + 180) % 360 - 180)
                max_error = max(max_error, error)
                if error > 1e-3:
                    differences.append(f"{ta['name']} frame {frame} value[{index}]: {va} / {vb}")
    return {"equivalent_within_1e_3": not differences, "decompressed_bytes_identical": raw_a == raw_b,
            "maximum_numeric_error": max_error, "difference_count": len(differences), "examples": differences[:12]}


def graph(path):
    data = Path(path).read_bytes()
    if not data.endswith(b"RPLS"):
        raise ValueError("Missing RPLS extension")
    length = struct.unpack_from("<I", data, len(data) - 8)[0]
    if length < 4 or length > len(data) - 16:
        raise ValueError("Invalid RPLS extension length")
    block = data[-8 - length:-8]
    expected = struct.unpack_from("<I", block)[0]
    raw = zlib.decompress(block[4:])
    if len(raw) != expected:
        raise ValueError("RPLS length mismatch")
    return json.loads(raw)


def bitmap_dat(data):
    # Adobe also puts ordinary JPEG/PNG payloads behind bitmapDataHRef.
    if data.startswith((b"\xff\xd8\xff", b"\x89PNG\r\n\x1a\n")):
        from PIL import Image
        with Image.open(io.BytesIO(data)) as image:
            rgba = image.convert("RGBA")
            pixels = bytearray()
            for r, g, b, a in rgba.getdata():
                pixels.extend((a, (r * a + 127) // 255, (g * a + 127) // 255, (b * a + 127) // 255))
            return rgba.width, rgba.height, bytes(pixels)
    if len(data) < 26 or data[:2] != b"\x03\x05":
        raise ValueError("Unsupported DAT signature")
    stride, width, height = struct.unpack_from("<HHH", data, 2)
    if not width or not height or stride < width * 4:
        raise ValueError("Invalid DAT dimensions")
    expected = stride * height
    if data[25] == 0:
        pixels = data[26:]
        if len(pixels) != expected:
            raise ValueError(f"Invalid raw DAT size: {len(pixels)} / {expected}")
    elif data[25] == 1:
        pieces = []
        position = 26
        terminated = False
        while position + 2 <= len(data):
            length = struct.unpack_from("<H", data, position)[0]
            position += 2
            if not length:
                terminated = True
                break
            if position + length > len(data):
                raise ValueError("Truncated DAT fragment")
            pieces.append(data[position:position + length])
            position += length
        if not terminated or position != len(data):
            raise ValueError("Missing DAT terminator or trailing bytes")
        decoder = zlib.decompressobj()
        pixels = decoder.decompress(b"".join(pieces), expected + 1)
        # Genuine Adobe streams may use sync flush rather than zlib EOF.
        if len(pixels) != expected or decoder.unused_data or decoder.unconsumed_tail:
            raise ValueError(f"Invalid DAT decompressed size: {len(pixels)} / {expected}")
    else:
        raise ValueError(f"Unsupported DAT compression mode: {data[25]}")
    return width, height, b"".join(pixels[y * stride:y * stride + width * 4] for y in range(height))


def manifest_images(resource_root):
    paths, cells = {}, {}
    folder, prefix = "", ""
    for element in ET.parse(resource_root / "resources.xml").getroot().iter():
        kind = element.tag.rsplit("}", 1)[-1]
        if kind == "SetDefaults":
            folder, prefix = element.get("path", folder), element.get("idprefix", prefix)
        elif kind == "Image" and element.get("path"):
            stem = resource_root / folder.replace("\\", "/") / element.get("path").replace("\\", "/")
            identifier = (prefix + element.get("id", stem.stem)).upper()
            cells.setdefault(identifier, (int(element.get("cols", "1")), int(element.get("rows", "1"))))
            source = find_image(stem)
            if source:
                paths.setdefault(identifier, source)
    return paths, cells


def image_catalog(resource_root):
    """DefinitionLoadImage order, preserving literal resource ID characters."""
    declared, _ = manifest_images(resource_root)
    paths = dict(declared)
    for identifier, source in declared.items():
        if identifier.startswith("IMAGE_") and not identifier.startswith("IMAGE_REANIM_"):
            paths.setdefault("IMAGE_REANIM_" + identifier[len("IMAGE_"):], source)
    # The first two engine paths strip IMAGE_, even for IMAGE_REANIM_X.
    # Index relative paths rather than basenames: nested images can share names.
    for directory, prefix in (("", "IMAGE_"), ("particles", "IMAGE_"),
                              ("reanim", "IMAGE_REANIM_"), ("images", "IMAGE_REANIM_")):
        base = resource_root / directory
        candidates = {}
        for source in sorted(base.rglob("*")):
            if source.is_file() and source.suffix.lower() in (".png", ".jpg", ".jpeg"):
                key = prefix + source.relative_to(base).with_suffix("").as_posix().upper()
                previous = candidates.get(key)
                order = {".png": 0, ".jpg": 1, ".jpeg": 2}
                if previous is None or order[source.suffix.lower()] < order[previous.suffix.lower()]:
                    candidates[key] = source
        for key, source in candidates.items():
            paths.setdefault(key, source)
            paths.setdefault(key.replace("/", "\\"), source)
    return paths


def bitmap_resource(bitmap):
    stem = PurePosixPath(bitmap.get("name", "").replace("\\", "/")).stem
    cell = re.fullmatch(r"(.+)__REANIM_CEL_(\d+)", stem, re.IGNORECASE)
    cel = int(cell.group(2)) if cell else None
    bare = cell.group(1) if cell else stem
    identifier = bare if bare.upper().startswith("IMAGE_") else "IMAGE_REANIM_" + bare
    for property in bitmap.iter():
        if (property.tag.rsplit("}", 1)[-1] == "PD" and property.get("n") == "fla-reanim.resource-id"
                and property.get("t") == "s" and property.get("v")):
            identifier = property.get("v")
            pd_cell = re.fullmatch(r"(.+)__REANIM_CEL_(\d+)", identifier, re.IGNORECASE)
            if pd_cell:
                identifier = pd_cell.group(1)
                if cel is None:
                    cel = int(pd_cell.group(2))
            break
    return identifier, cel


def fla_archive(path):
    try:
        return zipfile.ZipFile(path)
    except zipfile.BadZipFile:
        # Original Adobe FLA files can overcount the central directory by 54.
        # Repair only this confirmed defect in memory; never modify the input.
        data = bytearray(Path(path).read_bytes())
        end = data.rfind(b"PK\x05\x06", max(0, len(data) - 65557))
        if end < 0 or end + 22 > len(data):
            raise
        declared, offset = struct.unpack_from("<II", data, end + 12)
        comment = struct.unpack_from("<H", data, end + 20)[0]
        if (end + 22 + comment != len(data) or offset >= end
                or data[offset:offset + 4] != b"PK\x01\x02" or declared != end - offset + 54):
            raise
        struct.pack_into("<I", data, end + 12, end - offset)
        return zipfile.ZipFile(io.BytesIO(data))


def find_image(stem):
    if stem.is_file() and stem.suffix.lower() in (".png", ".jpg", ".jpeg"):
        return stem
    for extension in (".png", ".jpg", ".jpeg"):
        candidate = stem.with_suffix(extension)
        if candidate.is_file():
            return candidate
    return None


def source_pixels(path):
    """Independent Pillow implementation of RGB plus engine blue-channel masks."""
    from PIL import Image
    image = Image.open(path).convert("RGBA")
    stem = path.stem
    mask = find_image(path.with_name("_" + stem)) or find_image(path.with_name(stem + "_"))
    if mask:
        alpha = Image.open(mask).convert("RGBA")
        if image.size != alpha.size:
            raise ValueError("Source alpha mask dimensions differ")
        image.putalpha(alpha.getchannel("B"))
    elif stem.startswith("_") or stem.endswith("_"):
        if not find_image(path.with_name(stem.strip("_"))):
            alpha = image.getchannel("B")
            image = Image.new("RGBA", image.size, (255, 255, 255, 255))
            image.putalpha(alpha)
    return image.size, image.tobytes()


def validate_fla(path, resource_root):
    errors = []
    checked = 0
    pixels_compared = 0
    pixel_errors = []
    jpeg_decoder_differences = []
    source_images = []
    unknown_images = []
    sources = image_catalog(resource_root)
    declared, cells = manifest_images(resource_root)
    for identifier in list(cells):
        if identifier.startswith("IMAGE_") and not identifier.startswith("IMAGE_REANIM_"):
            alias = "IMAGE_REANIM_" + identifier[len("IMAGE_"):]
            if alias not in declared and sources.get(alias) == declared.get(identifier):
                cells[alias] = cells[identifier]
    with fla_archive(path) as archive:
        document = ET.fromstring(archive.read("DOMDocument.xml"))
        for node in document.iter():
            node.tag = node.tag.rsplit("}", 1)[-1]
        bitmaps = document.findall("media/DOMBitmapItem")
        for bitmap in bitmaps:
            href = bitmap.get("bitmapDataHRef")
            if href:
                try:
                    width, height, argb = bitmap_dat(archive.read("bin/" + href))
                    checked += 1
                    identifier, cel = bitmap_resource(bitmap)
                    source = sources.get(identifier.upper())
                    if source is None:
                        unknown_images.append(identifier)
                        continue
                    (source_width, source_height), rgba = source_pixels(source)
                    if cel is not None:
                        columns, rows = cells.get(identifier.upper(), (1, 1))
                        if not (0 < columns <= source_width and 0 < rows <= source_height and 0 <= cel < columns):
                            raise ValueError(f"Invalid sprite layout/cel for {identifier}")
                        cw, ch = source_width // columns, source_height // rows
                        rgba = b"".join(rgba[(y * source_width + cel * cw) * 4:
                                            (y * source_width + (cel + 1) * cw) * 4] for y in range(ch))
                        source_width, source_height = cw, ch
                    source_images.append({"bitmap": identifier, "path": str(source), "sha256": sha256(source),
                                          "width": source_width, "height": source_height})
                    if (width, height) != (source_width, source_height):
                        pixel_errors.append({"bitmap": identifier, "error": "dimensions differ", "embedded": [width, height], "source": [source_width, source_height]})
                        continue
                    mismatches = 0
                    decoder_differences = 0
                    maximum_channel_error = 0
                    jpeg_source = source.suffix.lower() in (".jpg", ".jpeg")
                    for position in range(0, len(rgba), 4):
                        r, g, b, a = rgba[position:position + 4]
                        # Adobe stores premultiplied ARGB, proven independently
                        # against an Adobe-authored FLA and real Animate display.
                        expected = bytes((a, (r * a + 127) // 255, (g * a + 127) // 255, (b * a + 127) // 255))
                        actual = argb[position:position + 4]
                        channel_error = max(abs(a - b) for a, b in zip(actual[1:], expected[1:]))
                        maximum_channel_error = max(maximum_channel_error, channel_error)
                        decoder_differences += actual != expected
                        # The native JPEG decoder and Pillow/libjpeg can have
                        # small RGB rounding differences. Alpha is still exact.
                        mismatches += actual[0] != a or channel_error > (4 if jpeg_source else 0)
                    pixels_compared += width * height
                    if jpeg_source and decoder_differences:
                        jpeg_decoder_differences.append({"bitmap": identifier, "source": str(source),
                                                         "pixels_with_decoder_rounding": decoder_differences,
                                                         "maximum_premultiplied_rgb_channel_error": maximum_channel_error,
                                                         "rgb_tolerance": 4, "alpha_tolerance": 0})
                    if mismatches:
                        pixel_errors.append({"bitmap": identifier, "source": str(source), "pixel_mismatches": mismatches, "pixels": width * height})
                except Exception as error:
                    errors.append({"bitmap": bitmap.get("name"), "error": str(error)})
            else:
                errors.append({"bitmap": bitmap.get("name"), "error": "Missing embedded DAT reference"})
        for member in archive.namelist():
            if member.endswith(".xml"):
                ET.fromstring(archive.read(member))
    return {"all_embedded_bitmaps_decode": not errors and checked == len(bitmaps), "bitmaps_checked": checked,
            "bitmap_count": len(bitmaps), "errors": errors[:20],
            "all_source_premultiplied_pixels_match": not pixel_errors and not unknown_images,
            "source_pixels_compared": pixels_compared, "pixel_errors": pixel_errors[:20], "unresolved_images": unknown_images,
            "source_images": source_images, "jpeg_decoder_differences": jpeg_decoder_differences,
            "png_rgb_tolerance": 0, "jpeg_rgb_tolerance": 4, "alpha_tolerance": 0}


def png(width, height):
    def chunk(kind, payload):
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload))
    rows = b"".join(b"\0" + bytes(((x * 17) % 256, (y * 13) % 256, 80, 255)) * width for y in range(height) for x in [y])
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def write_plus_fixture(path, frames=512, bones=32):
    elements = "".join(f'<DOMBitmapInstance name="bone{i}" referenceID="b{i}" libraryItemName="tile.png"><matrix><Matrix tx="{i * 3}" ty="{i * 2}"/></matrix></DOMBitmapInstance>' for i in range(bones))
    matrices = "".join(f'<Matrix tx="{frame * 0.25 + i}" ty="{(frame + i) % 7}"/>' for i in range(bones) for frame in range(frames))
    # The chain tests setup-pose deltas, world-to-local conversion, and pivots.
    tree = "".join(f'<IKNode referenceID="b{i}" location="{i},0"><childNodes>' for i in range(bones)) + "".join('</childNodes></IKNode>' for _ in range(bones))
    xml = (f'<DOMDocument frameRate="30"><media><DOMBitmapItem name="tile.png" href="tile.png"/></media><timelines><DOMTimeline><layers>'
           f'<DOMLayer name="armature" animationType="IK pose"><frames><DOMFrame duration="{frames}" tweenType="IK pose"><elements>{elements}</elements><betweenFrameMatrixList>{matrices}</betweenFrameMatrixList><IKTree>{tree}</IKTree></DOMFrame></frames></DOMLayer>'
           '<DOMLayer name="folder" layerType="folder"/></layers></DOMTimeline></timelines></DOMDocument>')
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, content in (("DOMDocument.xml", xml.encode("utf-8")), ("tile.png", png(64, 64))):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, content)


def directory_bytes(path):
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


def benchmark_case(name, direction, source, old_command, rust_command, old_input, rust_input, output_name,
                   work, repetitions, resource_root):
    old_output = old_input.parent / output_name
    rust_output = rust_input.parent / output_name
    outputs = {"baseline": old_output, "rust": rust_output}

    def run_backend(backend, command):
        # Only delete this known generated output in the benchmark scratch tree.
        # A stale result must not make a silently failed conversion look correct.
        outputs[backend].unlink(missing_ok=True)
        result = measure(command, work)
        result["output_exists"] = outputs[backend].is_file()
        if result["output_exists"]:
            result["output_bytes"] = outputs[backend].stat().st_size
            result["output_sha256"] = sha256(outputs[backend])
        return result

    case = {"name": name, "direction": direction, "input": str(source), "input_sha256": sha256(source),
            "input_bytes": source.stat().st_size, "commands": {"baseline": old_command, "rust": rust_command},
            "warmup": {}, "runs": {"baseline": [], "rust": []}}
    for backend, command in (("baseline", old_command), ("rust", rust_command)):
        case["warmup"][backend] = run_backend(backend, command)
    for index in range(repetitions):
        order = (("baseline", old_command), ("rust", rust_command))
        if index % 2:
            order = tuple(reversed(order))
        for backend, command in order:
            run = run_backend(backend, command)
            run["round"] = index + 1
            run["order_within_round"] = [item[0] for item in order].index(backend) + 1
            case["runs"][backend].append(run)
    case["medians"] = {backend: median_runs(runs) for backend, runs in case["runs"].items()}
    case["outputs"] = {backend: {"path": str(path), "exists": path.exists(), "bytes": path.stat().st_size if path.exists() else 0,
                                   "sha256": sha256(path) if path.exists() else None,
                                   "directory_bytes_including_inputs": directory_bytes(path.parent)}
                       for backend, path in (("baseline", old_output), ("rust", rust_output))}
    try:
        if direction == "forward":
            validation = compare_compiled(old_output, rust_output)
            valid = validation["equivalent_within_1e_3"]
        elif direction == "plus":
            validation = compare_compiled(old_output, rust_output)
            validation["graphs_equal"] = graph(old_output) == graph(rust_output)
            validation["images_byte_equal"] = all((old_output.parent / p).read_bytes() == (rust_output.parent / p).read_bytes()
                                                       for p in graph(old_output)["images"])
            # PNG compression differs, so also compare decoded RGBA without Pillow
            # through the optional test dependency only when byte equality differs.
            if not validation["images_byte_equal"]:
                from PIL import Image
                validation["images_rgba_equal"] = all(Image.open(old_output.parent / p).convert("RGBA").tobytes() == Image.open(rust_output.parent / p).convert("RGBA").tobytes()
                                                       for p in graph(old_output)["images"])
            else:
                validation["images_rgba_equal"] = True
            valid = validation["equivalent_within_1e_3"] and validation["graphs_equal"] and validation["images_rgba_equal"]
        else:
            validation = {"baseline": validate_fla(old_output, resource_root), "rust": validate_fla(rust_output, resource_root)}
            verify = measure([str(rust_command[0]), "--verify", "--resource-root", str(resource_root), str(rust_input)], work)
            validation["rust_exact_round_trip"] = verify
            valid = (validation["baseline"]["all_embedded_bitmaps_decode"] and validation["rust"]["all_embedded_bitmaps_decode"]
                     and validation["baseline"]["all_source_premultiplied_pixels_match"]
                     and validation["rust"]["all_source_premultiplied_pixels_match"] and verify["exit_code"] == 0)
        case["validation"] = validation
    except Exception as error:
        valid = False
        case["validation"] = {"error": repr(error)}
    case["outputs_valid_and_comparable"] = valid
    a, b = case["medians"]["baseline"], case["medians"]["rust"]
    if (valid and a["successful_runs"] == repetitions and b["successful_runs"] == repetitions
            and all(r["output_exists"] for runs in case["runs"].values() for r in runs)):
        case["speedup"] = a["wall_seconds"] / b["wall_seconds"]
        case["sampled_peak_memory_reduction_fraction"] = 1 - b["sampled_tree_peak_rss_bytes"] / a["sampled_tree_peak_rss_bytes"]
    else:
        case["speedup"] = None
        case["comparison_note"] = "A failed conversion or invalid output is not counted as a speed improvement."
    return case


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project-root", type=Path, required=True)
    parser.add_argument("--rust", type=Path, required=True, help="Release fla-reanim-cli.exe")
    parser.add_argument("--baseline-root", type=Path, help="Historical project root containing tools/ and scripts/")
    parser.add_argument("--baseline-revision", help="Restore historical tool/runtime/scripts with git archive")
    parser.add_argument("--sample-dir", type=Path, help="Private ZIP-FLA samples; default: PROJECT/../寄存")
    parser.add_argument("--work-dir", type=Path, help="Default: PROJECT/.obj/fla-native-benchmark")
    parser.add_argument("--output", type=Path, default=Path(__file__).parent / "results/process-comparison.json")
    parser.add_argument("--repetitions", type=int, default=7)
    parser.add_argument("--only", nargs="*", help="Optional scenario names")
    args = parser.parse_args()
    project = args.project_root.resolve()
    rust = args.rust.resolve()
    work = (args.work_dir or project / ".obj/fla-native-benchmark").resolve()
    if not work.is_relative_to(project):
        parser.error("--work-dir must be inside the project so the historical resource lookup can find Resources")
    work.mkdir(parents=True, exist_ok=True)
    baseline = args.baseline_root.resolve() if args.baseline_root else project
    if args.baseline_revision:
        baseline = restore_baseline(project, args.baseline_revision, work / "historical-baseline")
    old_exe = baseline / "tools/fla_reanim_compiler/FlaReanimCompiler.exe"
    old_python = baseline / "scripts/animations/import_fla.py"
    samples = args.sample_dir or project.parent / "寄存"
    resources = project / "Resources"
    fixture = work / "bones-512x32.fla"
    write_plus_fixture(fixture)
    scenarios = []
    for name in ("Chomper", "Zombie_torch", "Zombie", "Zombie_polevaulter", "SelectorScreen"):
        source = samples / (name + ".fla")
        if source.exists():
            scenarios.append(("forward_" + name, "forward", source))
    for name in ("Zombie_boss", "Zombie_gargantuar"):
        candidates = list((resources / "compiled/reanim").glob(name + ".reanim.compiled"))
        if not candidates:
            candidates = [p for p in (resources / "compiled/reanim").glob("*.compiled") if p.name.lower() == name.lower() + ".reanim.compiled"]
        if candidates:
            scenarios.append(("reverse_" + name, "reverse", candidates[0]))
    scenarios.append(("plus_bones_512x32", "plus", fixture))
    result = {"schema_version": 1, "generated_local_time": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
              "method": {"repetitions": args.repetitions, "warmups_per_backend": 1, "order": "alternating AB/BA per round",
                         "timing": "full process wall time including startup and IO, warm filesystem cache",
                         "memory": "psutil live process-tree RSS sampled every 2ms; Windows lifetime peak working set also retained",
                         "limitations": ["No cold-cache eviction or game/Animate UI benchmark", "Shared system runtime memory is not charged to another process", "Real FLA samples are external inputs identified by SHA256", "Reverse output contains additional lossless metadata in Rust"]},
              "machine": {"platform": platform.platform(), "processor": platform.processor(), "logical_cpus": os.cpu_count(),
                          "memory_bytes": psutil.virtual_memory().total, "python": sys.version, "psutil": psutil.__version__, "Pillow": pillow_version},
              "provenance": {"project_head": git(project, "rev-parse", "HEAD"), "benchmark_script_sha256": sha256(Path(__file__)),
                             "baseline_revision": args.baseline_revision or "working copy executable (SHA256 below)",
                             "baseline_exe": {"path": str(old_exe), "bytes": old_exe.stat().st_size, "sha256": sha256(old_exe)},
                             "baseline_implementation": {"path": str(old_exe.parent / "runtime/FlaReanimCompiler.dll"), "sha256": sha256(old_exe.parent / "runtime/FlaReanimCompiler.dll")},
                             "baseline_python": {"path": str(old_python), "sha256": sha256(old_python)},
                             "baseline_python_dependencies": {p.name: sha256(p) for p in old_python.parent.glob("*.py") if not p.name.startswith("test_")},
                             "baseline_runtime_bytes": directory_bytes(old_exe.parent / "runtime"),
                             "resource_manifest_sha256": sha256(resources / "resources.xml"),
                             "rust_exe": {"path": str(rust), "bytes": rust.stat().st_size, "sha256": sha256(rust)}},
              "scenarios": []}
    selected = [s for s in scenarios if not args.only or s[0] in args.only]
    if args.only and set(args.only) != {s[0] for s in selected}:
        raise ValueError("Requested scenario inputs were not found")
    for name, direction, source in selected:
        print(f"Measuring {name}", flush=True)
        inputs = []
        for backend in ("baseline", "rust"):
            directory = work / name / backend
            directory.mkdir(parents=True, exist_ok=True)
            target = directory / source.name
            shutil.copy2(source, target)
            inputs.append(target)
        old_input, rust_input = inputs
        if direction == "plus":
            old_command = [sys.executable, str(old_python), str(old_input), "--compiled", "--resource-root", str(resources)]
        else:
            old_command = [str(old_exe), str(old_input)]
        rust_command = [str(rust), "--force", "--resource-root", str(resources)]
        if direction == "plus":
            rust_command.append("--plus")
        rust_command.append(str(rust_input))
        output_name = source.stem + ".reanim.compiled" if direction != "reverse" else source.name.removesuffix(".reanim.compiled") + ".fla"
        case = benchmark_case(name, direction, source, old_command, rust_command, old_input, rust_input, output_name,
                              work, args.repetitions, resources)
        result["scenarios"].append(case)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
        print(f"{name}: valid={case['outputs_valid_and_comparable']} speedup={case.get('speedup')}", flush=True)
    print(args.output)


if __name__ == "__main__":
    main()
