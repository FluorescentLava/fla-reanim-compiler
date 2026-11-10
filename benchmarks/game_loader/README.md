# Game C++ loader integration check

This harness executes 26 original functions extracted from `TodLib/Definition.cpp`
and `TodLib/Reanimator.cpp`, with the game's original DefMap field tables and
vendored zlib. It exercises schema calculation, decompression, packed-to-native
structure conversion, recursive arrays/strings/image/font loading, missing-value
expansion, track lookup and `GetFramesForLayer`.

It does not use the Rust decoder to decide whether a file loads, and it does not
restore original bytes from FLA metadata. Use it on files produced by normal
recompilation after deleting every `META-INF/reanim-source.*` member or after
Adobe saves an edited document.

`host.hpp` supplies small filesystem/manifest resource callbacks in place of the
game's rendering framework. The original `DefinitionLoadImage` prefix/fallback
logic runs unchanged. The callbacks check exact resource IDs and existing image
files and give each resolved path a distinct stable `Image*`. After the original
game fills missing values, the driver records ordered tracks, FPS, frame counts,
engine clip ranges and consecutive active image path spans. Invisible cached
image references are recorded in the raw load trace but are not treated as
visible output. The callbacks do not initialize SDL, decode pixels, build
atlases, or render the animation; those require separate image validation and
game/Adobe acceptance.
The `AnimationRegistry::Frames` callback returns false to exercise the classic
game clip rules. An entirely blank track has engine clip `(start=0, count=1)`.

On Windows the driver uses `wmain`, converts native UTF-16 arguments to UTF-8,
and passes paths to `std::filesystem::u8path`. The test `p_fopen` callback uses
`_wfopen` at the native file boundary. This avoids an earlier harness-only bug:
`main(char**)` supplied ANSI-code-page arguments that were incorrectly treated
as UTF-8, so a Chinese resource directory aborted before loading even an
original valid file. Both the resource root and compiled file paths have been
checked in place under the actual Chinese user directory. These are test-host
I/O adapters; the 26 extracted game functions are unchanged. The harness does
not test the full game's original filesystem/pak framework implementation.

Build on Windows with Python 3 and Visual Studio C++ build tools:

Run these commands from the independent converter repository at
`<gameRoot>/tools/fla_to_compiled/src` (the Rust source folder is its nested
`src/`). The game resources are at `../../../Resources`. Both build scripts find
the nearest ancestor containing `TodLib/Definition.cpp` and
`TodLib/Reanimator.cpp`, so the additional repository level is supported. For a
standalone converter checkout elsewhere, pass `-SourceRoot <gameRoot>` to
`Build.ps1`, or `--game-source <gameRoot>` to `prepare.py`.

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File benchmarks/game_loader/Build.ps1 -OutputDirectory "$env:TEMP/pvz-game-loader"
python benchmarks/game_loader/validate.py --loader "$env:TEMP/pvz-game-loader/loader.exe" --resources path/to/Resources --inputs path/to/recompiled --recursive --output path/to/loader-results.json
python benchmarks/game_loader/check_controls.py --loader "$env:TEMP/pvz-game-loader/loader.exe" --resources path/to/Resources --sample path/to/Resources/compiled/reanim/Blover.reanim.compiled --output path/to/negative-controls.json
```

`Build.ps1` generates and compiles into the selected temporary directory. It
does not modify the game source or add binaries to this directory. `prepare.py`
can also generate `loader.cpp`, `sources.rsp` and provenance for a developer shell
or another build system. The provenance records the complete game file hashes,
every copied function hash, the field table hash, and generated source hash.
`source-provenance.json` records the source revision used for the initial audit.

The initial audit loaded all 157 repository classic resources and five forced
normal recompiles. Three negative controls were rejected: a doubled
`IMAGE_REANIM_` prefix, an incorrect schema hash, and a valid file with an empty
resource root/catalog. Keep these controls when changing the test host.
For a comparison with the original files and a binding to the independently
validated normal conversion outputs, run:

```powershell
python benchmarks/game_loader/validate.py --loader "$env:TEMP/pvz-game-loader/loader.exe" --resources ../../../Resources --inputs ../../../Resources/compiled/reanim --controls-sample ../../../Resources/compiled/reanim/Blover.reanim.compiled --output benchmarks/results/game-loader-source.json
python benchmarks/game_loader/validate.py --loader "$env:TEMP/pvz-game-loader/loader.exe" --resources ../../../Resources --inputs path/to/normal-conversion-tree --recursive --source-report benchmarks/results/game-loader-source.json --conversion-report benchmarks/results/editable-goty-final.json --output benchmarks/results/game-loader-editable.json
```

The two runs must use the same loader binary and resource manifest. Each file
must have an identical filename in the source report. A comparison requires
matching FPS, ordered track names, frame counts, active masks, original engine
clip ranges and visible resolved image path spans. Raw image ID/path load sets
are compared separately because they may contain unused hidden references.
The optional conversion report binds both source and output SHA256s to its Rust
binary SHA256 and includes its independent semantic/pixel-validation result.
The editable report carries the source run's four controls. `passed` covers the
C++ loader checks; `combined_validation_passed` additionally requires the
independent editable conversion report to pass.

The checked-in reports cover all 157 original classic resources and all 157
normal recompiles after removing source recovery metadata and changing DAT
pixels. All original/normal loader observations matched, and all four controls
passed. This is game-loader acceptance and the observed checks above, not a full
game render or an Adobe save/preview test. Python and Visual Studio are needed
only to reproduce this development harness, not to run the Rust converter.
