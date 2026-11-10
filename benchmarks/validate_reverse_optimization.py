"""Verify that reverse asset lookup deduplication preserves complete output bytes.

Use the saved reports/output directories from the previous release and the
completed reports from the new release. Four user-resource reverse conversions
are rerun from the reference clean-delivery report. The original files remain
untouched. This checks correctness, not benchmark timing.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import subprocess
import tempfile
import zipfile

from compare import sha256
from validate_editable import write_report


def read_report(path):
    return json.loads(path.read_text(encoding="utf-8"))


def recorded_path(value):
    # Windows reports may contain an extended drive path while other records
    # use the ordinary drive spelling. They identify the same workspace.
    value = str(value)
    if value.startswith("\\\\?\\") and len(value) > 5 and value[5] == ":":
        value = value[4:]
    return Path(value)


def root_before_qa(value):
    parts = recorded_path(value).parts
    for index, part in enumerate(parts):
        if part.lower() == ".qa":
            return Path(*parts[:index])
    raise ValueError("Cannot infer the baseline project root; provide --previous-root")


def relocate(value, previous_root, current_root):
    path = recorded_path(value)
    try:
        relative = path.relative_to(previous_root)
    except ValueError:
        return path
    return current_root / relative


def compare_file(previous, current):
    a, b = sha256(previous), sha256(current)
    return {"previous": str(previous), "current": str(current),
            "previous_sha256": a, "current_sha256": b,
            "bytes_equal": a == b and previous.read_bytes() == current.read_bytes(),
            "previous_bytes": previous.stat().st_size, "current_bytes": current.stat().st_size}


def compare_fla(previous, current):
    result = compare_file(previous, current)
    with zipfile.ZipFile(previous) as a, zipfile.ZipFile(current) as b:
        names_a, names_b = set(a.namelist()), set(b.namelist())
        changed = [name for name in sorted(names_a & names_b) if a.read(name) != b.read(name)]
    result.update({"zip_member_count": len(names_a), "zip_member_names_equal": names_a == names_b,
                   "changed_zip_members": changed,
                   "zip_members_equal": names_a == names_b and not changed})
    return result


def compare_tree(previous, current):
    if not previous.is_dir() or not current.is_dir():
        raise ValueError(f"Missing comparison directory: {previous} or {current}")
    a = {path.relative_to(previous).as_posix(): path for path in previous.rglob("*") if path.is_file()}
    b = {path.relative_to(current).as_posix(): path for path in current.rglob("*") if path.is_file()}
    comparisons = [{"relative_path": name, **compare_file(a[name], b[name])} for name in sorted(a.keys() & b.keys())]
    return {"file_names_equal": a.keys() == b.keys(), "missing_current": sorted(a.keys() - b.keys()),
            "added_current": sorted(b.keys() - a.keys()), "files": comparisons,
            "bytes_equal": a.keys() == b.keys() and all(item["bytes_equal"] for item in comparisons)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", required=True, type=Path)
    parser.add_argument("--previous-editable", required=True, type=Path)
    parser.add_argument("--current-editable", required=True, type=Path)
    parser.add_argument("--previous-adobe", required=True, type=Path)
    parser.add_argument("--current-adobe", required=True, type=Path)
    parser.add_argument("--reference-clean", required=True, type=Path)
    parser.add_argument("--previous-root", type=Path,
                        help="Original project root; inferred from the previous CLI path before .qa by default")
    parser.add_argument("--current-root", type=Path,
                        help="Current project root; defaults to this script's parent project directory")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--work-dir", type=Path)
    args = parser.parse_args()
    args.rust = args.rust.resolve(strict=True)
    args.output = args.output.resolve()
    if args.work_dir:
        args.work_dir.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="reverse-dedup-equivalence-", dir=args.work_dir)).resolve()
    previous, current = read_report(args.previous_editable), read_report(args.current_editable)
    previous_adobe, current_adobe = read_report(args.previous_adobe), read_report(args.current_adobe)
    previous_root = recorded_path(args.previous_root) if args.previous_root else root_before_qa(previous["binary"]["path"])
    current_root = (args.current_root or Path(__file__).resolve().parents[1]).resolve(strict=True)
    location = lambda value: relocate(value, previous_root, current_root)
    before_sha, after_sha = previous["binary"]["sha256"], sha256(args.rust)
    previous_binary_path = location(previous["binary"]["path"])
    assert sha256(previous_binary_path) == before_sha
    assert all(report["passed"] and report["completed"] for report in (previous, current, previous_adobe, current_adobe))
    assert previous_adobe["binary"]["sha256"] == before_sha
    assert current["binary"]["sha256"] == current_adobe["binary"]["sha256"] == after_sha
    result = {"schema_version": 1, "created_utc": datetime.now(timezone.utc).isoformat(),
              "purpose": "complete output equivalence after moving duplicate asset filtering before resource lookup",
              "completed": False, "previous_binary": previous["binary"], "current_binary": current["binary"],
              "previous_binary_relocated_path": str(previous_binary_path),
              "path_mapping": {"previous_project_root": str(previous_root), "current_project_root": str(current_root),
                               "policy": "map relocated workspace paths when reading files; preserve the original reference reports unchanged"},
              "validator_sha256": sha256(Path(__file__)), "work_dir": str(work),
              "reference_report_sha256": {path.name: sha256(path) for path in (
                  args.previous_editable, args.current_editable, args.previous_adobe,
                  args.current_adobe, args.reference_clean)},
              "editable": [], "adobe_outputs": None, "user_clean_reverse": []}
    old_samples = {Path(sample["input"]).name: sample for sample in previous["samples"]}
    for sample in current["samples"]:
        name = Path(sample["input"]).name
        old = old_samples[name]
        assert old["input_sha256"] == sample["input_sha256"]
        old_fla, new_fla = location(old["reverse"]["report"]["output"]), location(sample["reverse"]["report"]["output"])
        result["editable"].append({"input": sample["input"], "input_sha256": sample["input_sha256"],
            "reverse_fla": compare_fla(old_fla, new_fla),
            "normal_outputs": compare_tree(location(old["normal_compile"]["report"]["output"]).parent,
                                            location(sample["normal_compile"]["report"]["output"]).parent)})
    result["adobe_outputs"] = compare_tree(location(previous_adobe["work_dir"]), location(current_adobe["work_dir"]))
    clean = read_report(args.reference_clean)
    assert clean["cli_sha256"] == before_sha
    for sample in clean["samples"]:
        source = location(sample["reverse"]["input"])
        resource_root = source.parents[2]
        output = work / (sample["name"] + ".fla")
        command = [str(args.rust), "--resource-root", str(resource_root), "-o", str(output), str(source)]
        run = subprocess.run(command, capture_output=True, timeout=180,
                             creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        if run.returncode:
            raise RuntimeError(run.stderr.decode("utf-8", "replace"))
        reference = location(sample["reverse"]["output"])
        assert sha256(reference) == sample["final_generated_sha256"]
        result["user_clean_reverse"].append({"name": sample["name"], "input": str(source),
            "input_sha256": sha256(source), "resource_root": str(resource_root), "command": command,
            "conversion_report": json.loads(run.stdout.decode("utf-8").splitlines()[-1]),
            "comparison": compare_fla(reference, output)})
    result["summary"] = {"editable_reverse_flas": len(result["editable"]),
        "editable_reverse_flas_bytes_equal": sum(item["reverse_fla"]["bytes_equal"] for item in result["editable"]),
        "normal_output_files": sum(len(item["normal_outputs"]["files"]) for item in result["editable"]),
        "normal_output_files_bytes_equal": sum(file["bytes_equal"] for item in result["editable"] for file in item["normal_outputs"]["files"]),
        "adobe_output_files": len(result["adobe_outputs"]["files"]),
        "adobe_output_files_bytes_equal": sum(file["bytes_equal"] for file in result["adobe_outputs"]["files"]),
        "user_clean_reverse_files": len(result["user_clean_reverse"]),
        "user_clean_reverse_bytes_equal": sum(item["comparison"]["bytes_equal"] for item in result["user_clean_reverse"])}
    result["completed"] = True
    result["passed"] = (all(item["reverse_fla"]["bytes_equal"] and item["reverse_fla"]["zip_members_equal"]
                            and item["normal_outputs"]["bytes_equal"] for item in result["editable"])
                        and result["adobe_outputs"]["bytes_equal"]
                        and all(item["comparison"]["bytes_equal"] and item["comparison"]["zip_members_equal"] for item in result["user_clean_reverse"]))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    write_report(args.output, result)
    print(json.dumps(result["summary"], ensure_ascii=False, indent=2), flush=True)
    raise SystemExit(0 if result["passed"] else 1)


if __name__ == "__main__":
    main()
