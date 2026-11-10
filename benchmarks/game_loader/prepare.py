"""Build an isolated test translation unit from actual game loader functions.

No Rust codec functions or conversion metadata participate in this harness.
The source functions and field tables are copied without changing their logic.
"""
from pathlib import Path
import argparse
import hashlib
import json


DEFINITION_FUNCTIONS = [
    "void* DefinitionAlloc(", "void DefinitionFree(",
    "int DefinitionGetCompiledFieldStorageSize(", "int DefinitionGetCompiledFieldOffset(",
    "int DefinitionGetCompiledStructSize(", "void DefinitionReadCompiledStruct(",
    "bool DefinitionLoadImage(", "bool DefinitionLoadFont(",
    "bool DefReadFromCacheArray(", "bool DefReadFromCacheFloatTrack(",
    "bool DefReadFromCacheString(", "bool DefReadFromCacheImage(",
    "bool DefReadFromCacheFont(", "bool DefMapReadFromCache(",
    "uint DefinitionCalcHashSymbolMap(", "uint DefinitionCalcHashDefMap(",
    "uint DefinitionCalcHash(", "void* DefinitionUncompressCompiledBuffer(",
    "bool DefinitionReadCompiledFile(",
]
REANIMATOR_FUNCTIONS = [
    "void* ReanimatorTrackConstructor(", "void* ReanimatorDefinitionConstructor(",
    "void ReanimationFillInMissingData(float&", "void ReanimationFillInMissingData(void*&",
    "bool ReanimationLoadDefinition(", "int Reanimation::FindTrackIndex(",
    "void Reanimation::GetFramesForLayer(",
]


def extract(source, signature):
    start = source.index(signature)
    if source[start - 7:start] == "static ":
        start -= 7
    # Functions in these two game files close at column zero. Nested blocks do
    # not, so the extraction preserves their complete original bodies.
    end = source.index("\n}", start) + 2
    return source[start:end] + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--game-source", type=Path,
                        help="Game source containing TodLib; defaults to the nearest matching ancestor")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.game_source is None:
        args.game_source = next(
            (ancestor for ancestor in Path(__file__).resolve().parents
             if (ancestor / "TodLib/Definition.cpp").is_file()
             and (ancestor / "TodLib/Reanimator.cpp").is_file()), None)
        if args.game_source is None:
            parser.error("Cannot find game source above this harness; pass --game-source PATH containing TodLib")
    root = args.game_source.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=True)
    here = Path(__file__).parent
    definition_path = root / "TodLib/Definition.cpp"
    reanimator_path = root / "TodLib/Reanimator.cpp"
    definition = definition_path.read_text(encoding="utf-8-sig")
    reanimator = reanimator_path.read_text(encoding="utf-8-sig")
    source = (here / "host.hpp").read_text(encoding="utf-8")
    start = reanimator.index("DefField gReanimatorTransformDefFields[]")
    end = reanimator.index("\n", reanimator.index("DefMap gReanimatorDefMap =", start))
    tables = reanimator[start:end] + "\n"
    source += "\n// Original game field tables.\n" + tables
    function_hashes = {}
    for filename, content, functions in [
        ("Definition.cpp", definition, DEFINITION_FUNCTIONS),
        ("Reanimator.cpp", reanimator, REANIMATOR_FUNCTIONS),
    ]:
        for signature in functions:
            body = extract(content, signature)
            function_hashes[filename + ":" + signature] = hashlib.sha256(body.encode()).hexdigest()
            source += "\n// Original " + filename + ": " + signature + "\n" + body
    source += (here / "driver.cpp.in").read_text(encoding="utf-8")
    (args.output / "loader.cpp").write_text(source, encoding="utf-8")
    provenance = {
        "method": "Actual game C++ loader functions; filesystem/manifest test callbacks replace rendering runtime",
        "game_source": str(root),
        "Definition_cpp_sha256": hashlib.sha256(definition_path.read_bytes()).hexdigest(),
        "Reanimator_cpp_sha256": hashlib.sha256(reanimator_path.read_bytes()).hexdigest(),
        "field_tables_sha256": hashlib.sha256(tables.encode()).hexdigest(),
        "function_sha256": function_hashes,
        "host_sha256": hashlib.sha256((here / "host.hpp").read_bytes()).hexdigest(),
        "driver_sha256": hashlib.sha256((here / "driver.cpp.in").read_bytes()).hexdigest(),
        "generated_source_sha256": hashlib.sha256(source.encode()).hexdigest(),
    }
    (args.output / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")
    zlib = root / "SexyAppFramework/zlib"
    zlib_sources = [zlib / name for name in [
        "uncompr.c", "inflate.c", "inffast.c", "inftrees.c", "adler32.c", "crc32.c", "zutil.c",
    ]]
    response = ["/nologo", "/MD", "/O2", "/EHsc", "/std:c++20", "/utf-8",
                f'/I"{zlib}"', "loader.cpp", *(f'"{path}"' for path in zlib_sources), "/Fe:loader.exe"]
    (args.output / "sources.rsp").write_text("\n".join(response) + "\n", encoding="utf-8")
    print(f"Prepared {len(function_hashes)} original loader functions in {args.output}")


if __name__ == "__main__":
    main()
