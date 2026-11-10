"""Extract only authoring stage RECTs from the original GOTY OLE FLA corpus.

Verification helper, never shipped as a production dependency. Requires olefile.
The property block follows the document RECT (twips), then grid/publish settings.
Every file must have exactly one candidate. Cross-check the resulting dimensions
with JSFL doc.width/doc.height or an Adobe-saved DOMDocument.xml before reuse on
another FLA version. The checked-in corpus was cross-checked with six Adobe ZIP
saves and nine JSFL document measurements.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import olefile


def collect(source):
    sizes, provenance = {}, []
    for path in sorted(source.glob("*.fla")):
        with olefile.OleFileIO(path) as document:
            data = document.openstream("Contents").read()
        candidates = []
        for offset in range(6, len(data) - 80):
            x, right, y, bottom = struct.unpack_from("<iiii", data, offset)
            if not (x == y == 0 and 0 < right <= 327680 and 0 < bottom <= 327680
                    and right % 20 == bottom % 20 == 0):
                continue
            # Stage RECT precedes this fixed part of the stock property block.
            # Display zoom, ruler/grid colours and flags before it may vary.
            if data[offset + 25:offset + 41] != bytes.fromhex("68010000680100006801000068010000"):
                continue
            candidates.append((offset, [right // 20, bottom // 20]))
        if len(candidates) != 1:
            raise ValueError(f"Ambiguous document RECT: {path.name}: {candidates}")
        offset, size = candidates[0]
        sizes[path.stem] = size
        provenance.append({"file": path.name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                           "contents_rect_offset": offset, "stage": size})
    return sizes, provenance


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--provenance", type=Path, required=True)
    parser.add_argument("--adobe-measurements", type=Path)
    args = parser.parse_args()
    sizes, provenance = collect(args.source)
    checked = {}
    if args.adobe_measurements:
        checked = json.loads(args.adobe_measurements.read_text(encoding="utf-8-sig"))
        for name, size in checked.items():
            assert sizes[name] == size, (name, size, sizes.get(name))
    args.output.write_text(json.dumps(sizes, indent=2) + "\n", encoding="utf-8")
    args.provenance.write_text(json.dumps({"method": "unique OLE Contents document RECT in twips",
        "adobe_cross_checks": checked, "files": provenance}, indent=2) + "\n", encoding="utf-8")
    print(f"{len(sizes)} source stages; {len(checked)} Adobe cross-checks")
