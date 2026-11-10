"""Independent validation of Adobe XFL acceleration against compiled samples.

This deliberately reads source XML and evaluates affine endpoint transforms in
Python rather than asking the Rust converter to compute its own expected answer.
It samples the outer tween before composing static bitmap/symbol contents,
including registration-space changes between wrappers of the same bitmap.
Unsupported content is reported rather than approximated.
"""
from __future__ import annotations

import argparse
import io
import json
import math
from pathlib import Path
import struct
import xml.etree.ElementTree as ET
import zipfile

from compare import compiled, sha256

IDENTITY = (1., 0., 0., 1., 0., 0.)
TOLERANCES = (0.05001, 0.05001, 0.05002, 0.05002, 0.00051, 0.00051, 0., 0.00101)


def xml(data):
    root = ET.fromstring(data)
    for element in root.iter():
        element.tag = element.tag.rsplit("}", 1)[-1]
    return root


def matrix(element):
    element = element.find("matrix/Matrix")
    return tuple(float(element.get(key, default)) for key, default in
                 (("a", 1), ("b", 0), ("c", 0), ("d", 1), ("tx", 0), ("ty", 0))) if element is not None else IDENTITY


def multiply(a, b):
    return (a[0] * b[0] + a[2] * b[1], a[1] * b[0] + a[3] * b[1],
            a[0] * b[2] + a[2] * b[3], a[1] * b[2] + a[3] * b[3],
            a[0] * b[4] + a[2] * b[5] + a[4], a[1] * b[4] + a[3] * b[5] + a[5])


def point(element):
    value = element.find("transformationPoint/Point")
    return (float(value.get("x", 0)), float(value.get("y", 0))) if value is not None else (0., 0.)


def alpha(element):
    color = element.find("color/Color")
    if color is None:
        return 1.
    return float(color.get("alphaMultiplier", float(color.get("alphaPercent", 100)) / 100))


def ignored(layer):
    return layer.get("layerType") in ("guide", "folder")


class Source:
    def __init__(self, path):
        data = bytearray(path.read_bytes())
        end = data.rfind(b"PK\x05\x06")
        if end < 0:
            raise ValueError("ZIP/XFL required")
        offset = struct.unpack_from("<I", data, end + 16)[0]
        declared = struct.unpack_from("<I", data, end + 12)[0]
        if declared not in (end - offset, end - offset + 54):
            raise ValueError("Unexpected central-directory defect")
        struct.pack_into("<I", data, end + 12, end - offset)
        self.archive = zipfile.ZipFile(io.BytesIO(data))
        self.document = xml(self.archive.read("DOMDocument.xml"))
        self.symbols = {}
        for entry in self.document.findall("symbols/Include"):
            root = xml(self.archive.read("LIBRARY/" + entry.get("href")))
            self.symbols[root.get("name")] = root

    def symbol_child(self, element):
        name = element.get("libraryItemName", "")
        if name.rsplit("/", 1)[-1].lower() == "locator":
            return None
        symbol = self.symbols.get(name)
        if symbol is None:
            raise ValueError("Unknown static source symbol " + name)
        # Stock JSFL unwraps layer 0 / frame 0, regardless of editor visibility
        # or a regular symbol instance's firstFrame setting.
        layer = symbol.find("timeline/DOMTimeline/layers/DOMLayer")
        span = next((span for span in layer.findall("frames/DOMFrame")
                     if int(span.get("index", "0")) == 0), None) if layer is not None else None
        children = list(span.find("elements")) if span is not None and span.find("elements") is not None else []
        if len(children) != 1:
            raise ValueError("Static symbol does not contain exactly one bitmap chain: " + name)
        return children[0]

    def static_contents(self, element, depth=0):
        if depth > 64:
            raise ValueError("Excessive source nesting")
        if element.tag == "DOMBitmapInstance":
            return {"inner": matrix(element), "alpha": alpha(element),
                    "image": element.get("libraryItemName")}
        if element.tag in ("DOMSymbolInstance", "DOMGraphicInstance"):
            child = self.symbol_child(element)
            if child is None:
                raise ValueError("Nested locator is not a bitmap trajectory")
            result = self.static_contents(child, depth + 1)
            result["inner"] = multiply(matrix(element), result["inner"])
            result["alpha"] *= alpha(element)
            return result
        raise ValueError("Unsupported static bitmap content " + element.tag)

    def collect(self, elements):
        if not elements:
            return []
        if len(elements) != 1:
            raise ValueError("Root frame does not contain exactly one instance")
        root = elements[0]
        if root.tag == "DOMBitmapInstance":
            result = {"inner": IDENTITY, "alpha": 1., "image": root.get("libraryItemName")}
        elif root.tag in ("DOMSymbolInstance", "DOMGraphicInstance"):
            child = self.symbol_child(root)
            if child is None:
                return []
            result = self.static_contents(child)
        else:
            raise ValueError("Unsupported source root element " + root.tag)
        result.update(outer=matrix(root), point=point(root), alpha=result["alpha"] * alpha(root))
        return [result]


def components(m):
    return (math.hypot(m[0], m[1]), math.hypot(m[2], m[3]),
            math.degrees(math.atan2(m[1], m[0])), math.degrees(math.atan2(-m[2], m[3])))


def nearest_angle(value, reference):
    return reference + (value - reference + 180.) % 360. - 180.


def inverse(matrix):
    a, b, c, d, x, y = matrix
    determinant = a * d - b * c
    if abs(determinant) < 1e-12:
        raise ValueError("Singular wrapper registration transform")
    return (d / determinant, -b / determinant, -c / determinant, a / determinant,
            (c * y - d * x) / determinant, (b * x - a * y) / determinant)


def transformed(matrix, point):
    a, b, c, d, x, y = matrix
    return a * point[0] + c * point[1] + x, b * point[0] + d * point[1] + y


def source_sample(start, end, progress):
    """Sample the root instance, then transform its static bitmap contents."""
    a, b = start["outer"], end["outer"]
    pa, pb = start["point"], end["point"]
    center_a, center_b = transformed(a, pa), transformed(b, pb)
    # Re-express an endpoint wrapping the same bitmap in the starting wrapper's
    # coordinates. This preserves the displayed endpoint and its world pivot.
    if start["image"] == end["image"] and start["inner"] != end["inner"]:
        b = multiply(b, multiply(end["inner"], inverse(start["inner"])))
        pb = transformed(multiply(start["inner"], inverse(end["inner"])), pb)
    sa, sb = components(a), components(b)
    lerp = lambda x, y: x + (y - x) * progress
    sx, sy = lerp(sa[0], sb[0]), lerp(sa[1], sb[1])
    kx, ky = lerp(sa[2], nearest_angle(sb[2], sa[2])), lerp(sa[3], nearest_angle(sb[3], sa[3]))
    ma, mb = sx * math.cos(math.radians(kx)), sx * math.sin(math.radians(kx))
    mc, md = -sy * math.sin(math.radians(ky)), sy * math.cos(math.radians(ky))
    px, py = lerp(pa[0], pb[0]), lerp(pa[1], pb[1])
    outer = (ma, mb, mc, md, lerp(center_a[0], center_b[0]) - ma * px - mc * py,
             lerp(center_a[1], center_b[1]) - mb * px - md * py)
    flattened = multiply(outer, start["inner"])
    sx, sy, kx, ky = components(flattened)
    return [flattened[4], flattened[5], kx, ky, sx, sy, 0., lerp(start["alpha"], end["alpha"])]


def validate(source_path, compiled_path):
    source = Source(source_path)
    definition, _ = compiled(compiled_path)
    tracks = {t["name"]: t for t in definition["tracks"]}
    timeline = source.document.find("timelines/DOMTimeline")
    if timeline is None:
        raise ValueError("Missing main timeline")
    errors = []
    unsupported = []
    held_before_disappearance = []
    maximum_errors = [0.] * 8
    maximum_linear_differences = [0.] * 8
    checked_spans = checked_samples = acceleration_spans = 0
    static_child_spans = wrapper_registration_spans = 0
    max_distance_from_linear = 0.
    for layer in timeline.findall("layers/DOMLayer"):
        name = layer.get("name", "")
        if ignored(layer) or (name.startswith("_") and name != "_ground"):
            continue
        spans = sorted(layer.findall("frames/DOMFrame"), key=lambda f: int(f.get("index", 0)))
        for index, span in enumerate(spans[:-1]):
            acceleration = float(span.get("acceleration", -float(span.get("tweenEasing", 0))))
            if not acceleration or span.get("tweenType") != "motion":
                continue
            acceleration_spans += 1
            following = spans[index + 1]
            start_frame, frames = int(span.get("index", 0)), int(span.get("duration", 1))
            try:
                start_elements, end_elements = span.find("elements"), following.find("elements")
                start = source.collect(list(start_elements) if start_elements is not None else [])
                end = source.collect(list(end_elements) if end_elements is not None else [])
                if start and not end:
                    # A key span whose following display is empty has no second
                    # affine endpoint. Verify its held pose before disappearance;
                    # do not invent a curve toward a non-existent bitmap.
                    end = start
                    held_before_disappearance.append({"layer": name, "frame": start_frame, "duration": frames})
                if len(start) != len(end) or not start:
                    raise ValueError("Non-static source display change")
            except ValueError as error:
                unsupported.append({"layer": name, "frame": start_frame, "reason": str(error)})
                continue
            for part_index, (a, b) in enumerate(zip(start, end)):
                if a["image"] != b["image"]:
                    unsupported.append({"layer": name, "frame": start_frame, "reason": "Discrete image change"})
                    continue
                try:
                    source_sample(a, b, 0.)
                except ValueError as error:
                    unsupported.append({"layer": name, "frame": start_frame, "reason": str(error)})
                    continue
                track_name = name if part_index == 0 else name + str(part_index + 1)
                if track_name not in tracks:
                    errors.append({"layer": track_name, "error": "Missing compiled track"})
                    continue
                checked_spans += 1
                static_child_spans += a["inner"] != IDENTITY
                wrapper_registration_spans += a["inner"] != b["inner"]
                for offset in range(frames):
                    t = offset / frames
                    # Adobe's serialized acceleration has the opposite sign to
                    # JSFL's tweenEasing property: positive acceleration eases in.
                    u = t - max(-1., min(1., acceleration / 100.)) * t * (1. - t)
                    expected = source_sample(a, b, u)
                    linear = source_sample(a, b, t)
                    actual = tracks[track_name]["transforms"][start_frame + offset]["values"]
                    checked_samples += 1
                    for channel in (0, 1, 2, 3, 4, 5, 7):
                        error = abs(actual[channel] - expected[channel])
                        distance_from_linear = abs(expected[channel] - linear[channel])
                        if channel in (2, 3):
                            error = abs((actual[channel] - expected[channel] + 180.) % 360. - 180.)
                            distance_from_linear = abs((expected[channel] - linear[channel] + 180.) % 360. - 180.)
                        maximum_errors[channel] = max(maximum_errors[channel], error)
                        maximum_linear_differences[channel] = max(maximum_linear_differences[channel], distance_from_linear)
                        max_distance_from_linear = max(max_distance_from_linear, distance_from_linear)
                        # Independent source evaluation is double precision.
                        # Compiled positions/angles quantize to .1 and scale /
                        # alpha to .001, including endpoint f32 conversion.
                        tolerance = TOLERANCES[channel]
                        if error > tolerance:
                            errors.append({"layer": track_name, "frame": start_frame + offset,
                                           "channel": channel, "actual": actual[channel],
                                           "expected_from_xfl_acceleration": expected[channel], "error": error})
    source.archive.close()
    return {"validator_script_sha256": sha256(Path(__file__)),
            "method": "Independent root-instance interpolation followed by static child composition; same-bitmap wrapper endpoints normalized to starting registration coordinates",
            "source": str(source_path), "source_sha256": sha256(source_path),
            "compiled": str(compiled_path), "compiled_sha256": sha256(compiled_path),
            "nonzero_acceleration_spans": acceleration_spans, "checked_renderable_spans": checked_spans,
            "checked_frame_samples": checked_samples, "maximum_source_evaluation_error_by_channel": maximum_errors,
            "checked_spans_with_nonidentity_static_children": static_child_spans,
            "checked_spans_with_wrapper_registration_changes": wrapper_registration_spans,
            "channels": ["x_px", "y_px", "kx_degrees", "ky_degrees", "scale_x", "scale_y", "frame", "alpha"],
            "quantization_tolerances_by_channel": TOLERANCES,
            "maximum_distance_from_ignoring_acceleration_by_channel": maximum_linear_differences,
            "maximum_parameter_distance_from_ignoring_acceleration": max_distance_from_linear,
            "unsupported_spans": unsupported, "error_count": len(errors), "errors": errors[:25],
            "held_before_disappearance_spans": held_before_disappearance,
            "covers_all_nonzero_acceleration_spans": not unsupported,
            "pass": checked_samples > 0 and not errors and not unsupported}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--compiled", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = validate(args.source, args.compiled)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({k: v for k, v in result.items() if k not in ("errors", "unsupported_spans")}, ensure_ascii=False))
    if not result["pass"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
