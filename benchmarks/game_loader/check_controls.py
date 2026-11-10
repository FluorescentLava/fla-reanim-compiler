"""Require the C++ loader to reject broken schema, image ID and missing assets."""
from pathlib import Path
import argparse
import hashlib
import json
import struct
import subprocess
import tempfile
import zlib

from validate import catalog


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--loader", type=Path, required=True)
    parser.add_argument("--resources", type=Path, required=True)
    parser.add_argument("--sample", type=Path, required=True,
                        help="A valid classic compiled file containing IMAGE_REANIM_ artwork, e.g. Blover")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.loader = args.loader.resolve(strict=True)
    args.resources = args.resources.resolve(strict=True)
    data = args.sample.read_bytes()
    if data.endswith(b"RPLS") or struct.unpack_from("<I", data)[0] != 0xDEADFED4:
        parser.error("Use a classic compiled sample")
    raw = zlib.decompress(data[8:])
    start = raw.index(b"IMAGE_REANIM_")
    length = struct.unpack_from("<I", raw, start - 4)[0]
    image = raw[start:start + length]
    if not (0 < length <= 100000 and image.startswith(b"IMAGE_REANIM_")):
        parser.error("Cannot locate a length-prefixed sample image ID")
    doubled = b"IMAGE_REANIM_" + image
    bad_image = raw[:start - 4] + struct.pack("<I", len(doubled)) + doubled + raw[start + length:]
    bad_schema = struct.pack("<I", struct.unpack_from("<I", raw)[0] ^ 1) + raw[4:]

    def compiled(payload):
        return struct.pack("<II", 0xDEADFED4, len(payload)) + zlib.compress(payload, 9)

    results = []
    with tempfile.TemporaryDirectory(prefix="game-loader-controls-") as temporary:
        work = Path(temporary)
        manifest = work / "catalog.tsv"
        manifest.write_text("\n".join(catalog(args.resources)), encoding="utf-8")
        empty_manifest = work / "empty.tsv"
        empty_manifest.write_text("")
        empty_root = work / "empty"
        empty_root.mkdir()
        for name, payload, root, cat, expected in [
            ("valid", data, args.resources, manifest, True),
            ("double-prefix", compiled(bad_image), args.resources, manifest, False),
            ("wrong-schema", compiled(bad_schema), args.resources, manifest, False),
            ("missing-resources", data, empty_root, empty_manifest, False),
        ]:
            path = work / (name + ".compiled")
            path.write_bytes(payload)
            process = subprocess.run([str(args.loader), str(cat), str(root), str(path)],
                                     capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=30)
            loaded = process.returncode == 0
            results.append({"control": name, "expected_loaded": expected,
                            "loaded": loaded, "passed": loaded == expected,
                            "sha256": hashlib.sha256(payload).hexdigest(),
                            "stdout": process.stdout, "stderr": process.stderr})
    report = {"loader_sha256": hashlib.sha256(args.loader.read_bytes()).hexdigest(),
              "source_sha256": hashlib.sha256(data).hexdigest(), "results": results,
              "passed": all(result["passed"] for result in results)}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"passed": report["passed"], "controls": len(results)}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
