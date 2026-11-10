"""Audit ordinary native compilation against independent stock GOTY reanim XML.

This is an optional standard-library QA tool, never an application dependency.
Inputs must be original Adobe-saved ZIP FLA files without recovery metadata.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import xml.etree.ElementTree as ET
import zipfile


CASES = ("Blover", "Chomper", "Zombie", "Zombie_boss", "Zombie_polevaulter", "SelectorScreen")
FIELDS = ("x", "y", "kx", "ky", "sx", "sy", "f", "a")
DEFAULTS = (0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0)


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def validate_source(path):
    data = bytearray(path.read_bytes())
    footer = data.rfind(b"PK\x05\x06")
    if footer < 0:
        raise ValueError("The reference must be an Adobe-saved ZIP FLA")
    length, offset = struct.unpack_from("<II", data, footer + 12)
    if length == footer - offset + 54:
        struct.pack_into("<I", data, footer + 12, footer - offset)
    with zipfile.ZipFile(io.BytesIO(data)) as package:
        if "META-INF/reanim-source.compiled" in package.namelist():
            raise ValueError("Reference contains recovery metadata; use the original FLA")


def native(cli, *arguments):
    result = subprocess.run([str(cli), *map(str, arguments)], capture_output=True,
                            text=True, encoding="utf-8")
    if result.returncode:
        raise RuntimeError(result.stderr.strip())
    return result.stdout


def expanded_native(track):
    numbers = list(DEFAULTS)
    strings = ["", "", ""]
    for frame in track["transforms"]:
        for index, number in enumerate(frame["values"]):
            if number != -10000:
                numbers[index] = number
        for index, field in enumerate(("image", "font", "text")):
            if frame[field]:
                strings[index] = "" if field == "text" and frame[field] == "_" else frame[field]
        yield numbers.copy(), strings.copy()


def expanded_reference(track):
    numbers = list(DEFAULTS)
    strings = ["", "", ""]
    for frame in track.findall("t"):
        for index, field in enumerate(FIELDS):
            value = frame.findtext(field)
            if value is not None:
                numbers[index] = float(value)
        for index, field in enumerate(("i", "font", "text")):
            value = frame.findtext(field)
            if value:
                strings[index] = "" if field == "text" and value == "_" else value
        yield numbers.copy(), strings.copy()


def audit(cli, fla, reanim, output, resource_root=None):
    validate_source(fla)
    options = ["--force", "--output", output]
    if resource_root is not None:
        options += ["--resource-root", resource_root]
    native(cli, *options, fla)
    actual = json.loads(native(cli, "--dump", output))
    reference = ET.fromstring("<reference>" + reanim.read_text(encoding="utf-8-sig") + "</reference>")
    original_tracks = reference.findall("track")
    expected = {track.findtext("name"): track for track in original_tracks}
    actual_names = [track["name"] for track in actual["tracks"]]
    expected_names = [track.findtext("name") for track in original_tracks]
    counts = {"frame_count": 0, "visibility": 0, "image": 0, "font": 0, "text": 0}
    maximum = [0.0] * 8
    worst = [None] * 8
    compared = 0
    for track in actual["tracks"]:
        if track["name"] not in expected:
            continue
        old = expected[track["name"]]
        if len(track["transforms"]) != len(old.findall("t")):
            counts["frame_count"] += 1
        for frame, ((a, astr), (b, bstr)) in enumerate(zip(expanded_native(track), expanded_reference(old))):
            if (a[6] >= 0) != (b[6] >= 0):
                counts["visibility"] += 1
            if a[6] < 0 or b[6] < 0:
                continue
            compared += 1
            for index, field in enumerate(("image", "font", "text")):
                if astr[index] != bstr[index]:
                    counts[field] += 1
            for index, field in enumerate(FIELDS):
                delta = a[index] - b[index]
                if field in ("kx", "ky"):
                    delta = (delta + 180) % 360 - 180
                error = abs(delta)
                if error > maximum[index]:
                    maximum[index] = error
                    worst[index] = {"track": track["name"], "frame": frame,
                                    "native": a[index], "stock": b[index]}
    order_equal = actual_names == expected_names
    fps_equal = actual["fps"] == float(reference.findtext("fps"))
    return {
        "case": reanim.stem,
        "sources": {
            "fla": {"description": "Adobe Animate 24 ZIP save of GOTY/reanim/" + reanim.stem + ".fla",
                    "sha256": sha256(fla)},
            "reanim": {"description": "GOTY/reanim/" + reanim.name, "sha256": sha256(reanim)},
        },
        "recovery_metadata_present": False,
        "expected_tracks": len(expected_names),
        "actual_tracks": len(actual_names),
        "expected_frames": len(original_tracks[0].findall("t")),
        "actual_frames": len(actual["tracks"][0]["transforms"]),
        "track_order_equal": order_equal,
        "fps_equal": fps_equal,
        "mismatches": counts,
        "structure_and_resources_equal": order_equal and fps_equal and not any(counts.values()),
        "compared_active_track_frames": compared,
        "maximum_parameter_delta": {field: round(value, 8) for field, value in zip(FIELDS, maximum)},
        "worst_parameter_samples": dict(zip(FIELDS, worst)),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--fla-dir", type=Path, required=True,
                        help="Directory containing <case>-reference.fla Adobe saves")
    parser.add_argument("--reanim-dir", type=Path, required=True,
                        help="Original GOTY/reanim directory containing independent .reanim XML")
    parser.add_argument("--resource-root", type=Path,
                        help="Explicit target game Resources directory for bitmap export")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    cli = args.cli.resolve()
    with tempfile.TemporaryDirectory(prefix="goty-native-forward-") as scratch:
        cases = [audit(cli, args.fla_dir / (case + "-reference.fla"),
                       args.reanim_dir / (case + ".reanim"),
                       Path(scratch) / (case + ".reanim.compiled"), args.resource_root) for case in CASES]
    report = {
        "schema": 1,
        "method": "Native normal compile of original FLA without recovery metadata; compare with independently parsed stock JSFL .reanim output",
        "native_cli_sha256": sha256(cli),
        "resource_manifest_sha256": sha256(args.resource_root / "resources.xml") if args.resource_root else None,
        "numeric_delta_note": "Angles compare modulo 360. Numeric differences are reported, not hidden by structural equivalence. No claim of bitwise or rendered-pixel equivalence.",
        "cases": cases,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    for case in cases:
        print(case["case"], "structure/resources=" + str(case["structure_and_resources_equal"]),
              "max parameters=" + json.dumps(case["maximum_parameter_delta"]))
    return 0 if all(case["structure_and_resources_equal"] for case in cases) else 1


if __name__ == "__main__":
    raise SystemExit(main())
