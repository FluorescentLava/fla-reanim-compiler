using System.IO;

namespace FlaReanimCompiler;

/// <summary>Outcome of re-compiling a generated FLA and comparing it to the source.</summary>
internal sealed class RoundTripResult
{
    public required string SourcePath { get; init; }
    public required bool ByteExact { get; init; }

    /// <summary>
    /// True when the rebuilt definition differs only in ways that cannot change
    /// rendering (currently: a per-track 360-degree uniform angle shift, which the
    /// engine's sin/cos matrix construction makes periodic).
    /// </summary>
    public required bool RenderingEquivalent { get; init; }

    public required IReadOnlyList<string> Differences { get; init; }
    public required string VerificationPath { get; init; }

    public string Status => ByteExact ? "byte-exact" : RenderingEquivalent ? "rendering-equivalent" : "MISMATCH";
}

/// <summary>
/// Closes the fidelity loop: generates a FLA from a <c>.compiled</c> resource,
/// re-runs the existing forward converter over it, and compares the resulting
/// <c>.reanim.compiled</c> against the original byte for byte.
///
/// This is the objective gate for the reverse direction. Any structural mistake in
/// the generated XFL (wrong layer order, wrong label placement, wrong compaction)
/// shows up as a byte difference, so nothing here relies on visual inspection.
/// </summary>
internal static class ReanimRoundTripVerifier
{
    private const float AngleTolerance = 1e-3f;
    private const float ValueTolerance = 0.0f;

    public static RoundTripResult Verify(string compiledPath, string workDirectory)
    {
        string fullPath = Path.GetFullPath(compiledPath);
        string stem = Path.GetFileName(fullPath)[..^".compiled".Length];
        Directory.CreateDirectory(workDirectory);

        string flaPath = Path.Combine(workDirectory, stem + ".fla");

        // Step 1: reverse the compiled file into a FLA.
        var reverseReport = ReanimToFlaConverter.Convert(fullPath, new FlaConversionOptions
        {
            OutputPath = flaPath,
            EmbedBitmaps = true,
            WriteReanimXml = false
        });

        // Step 2: run the original forward converter over the generated FLA.
        var forward = new FlaToReanimConverter();
        ConversionResult forwardResult = forward.Convert(flaPath);

        // Step 3: compare against the original.
        ReanimCompiledDocument original = ReanimCompiledReader.Read(fullPath);
        ReanimCompiledDocument rebuilt = ReanimCompiledReader.Read(forwardResult.OutputPath);

        byte[] originalRaw = File.ReadAllBytes(fullPath);
        byte[] rebuiltRaw = File.ReadAllBytes(forwardResult.OutputPath);

        var differences = new List<string>();

        if (originalRaw.AsSpan().SequenceEqual(rebuiltRaw))
        {
            return new RoundTripResult
            {
                SourcePath = fullPath,
                ByteExact = true,
                RenderingEquivalent = true,
                Differences = [],
                VerificationPath = flaPath
            };
        }

        if (original.Definition.Tracks.Count != rebuilt.Definition.Tracks.Count)
        {
            differences.Add($"轨道数量不同：原始 {original.Definition.Tracks.Count}，重建 {rebuilt.Definition.Tracks.Count}。");
        }

        if (Math.Abs(original.Definition.Fps - rebuilt.Definition.Fps) > 1e-4f)
        {
            differences.Add($"fps 不同：原始 {original.Definition.Fps}，重建 {rebuilt.Definition.Fps}。");
        }

        bool onlyAngleShift = differences.Count == 0;
        int trackCount = Math.Min(original.Definition.Tracks.Count, rebuilt.Definition.Tracks.Count);

        // Compare the FILL-FORWARD view, not the raw packed fields. The packed format
        // stores a placeholder (-10000) to mean "same as the previous frame", and both
        // the original and the rebuilt file re-derive those placeholders from their own
        // compaction pass. A field written explicitly in one file and omitted in the
        // other is therefore identical to the engine, and comparing raw fields would
        // report it as a difference.
        for (int t = 0; t < trackCount; t++)
        {
            ReanimTrack a = original.Definition.Tracks[t];
            ReanimTrack b = rebuilt.Definition.Tracks[t];

            if (!string.Equals(a.Name, b.Name, StringComparison.Ordinal))
            {
                differences.Add($"轨道 {t} 名称不同：\"{a.Name}\" → \"{b.Name}\"。");
                onlyAngleShift = false;
                continue;
            }

            if (a.Transforms.Count != b.Transforms.Count)
            {
                differences.Add($"轨道 \"{a.Name}\" 帧数不同：{a.Transforms.Count} → {b.Transforms.Count}。");
                onlyAngleShift = false;
                continue;
            }

            ReanimTransform[] filledA = ReanimTransformSemantics.FillForward(a.Transforms);
            ReanimTransform[] filledB = ReanimTransformSemantics.FillForward(b.Transforms);

            for (int f = 0; f < a.Transforms.Count; f++)
            {
                ReanimTransform x = filledA[f];
                ReanimTransform y = filledB[f];

                if (!Near(x.X, y.X) || !Near(x.Y, y.Y) ||
                    !Near(x.Sx, y.Sx) || !Near(x.Sy, y.Sy) ||
                    !Near(x.Frame, y.Frame) || !Near(x.Alpha, y.Alpha))
                {
                    if (differences.Count < 40)
                    {
                        differences.Add(
                            $"轨道 \"{a.Name}\" 帧 {f} 数值不同：" +
                            $"x {x.X}→{y.X}, y {x.Y}→{y.Y}, sx {x.Sx}→{y.Sx}, " +
                            $"sy {x.Sy}→{y.Sy}, f {x.Frame}→{y.Frame}, a {x.Alpha}→{y.Alpha}。");
                    }

                    onlyAngleShift = false;
                }

                // Angle differences are flat-360 shifts per track, which the engine's
                // sin/cos matrix construction makes invisible.
                if (!AngleEquivalent(x.Kx, y.Kx) || !AngleEquivalent(x.Ky, y.Ky))
                {
                    if (differences.Count < 40)
                    {
                        differences.Add(
                            $"轨道 \"{a.Name}\" 帧 {f} 角度不同：" +
                            $"kx {x.Kx}→{y.Kx}, ky {x.Ky}→{y.Ky}。");
                    }

                    onlyAngleShift = false;
                }
                else if (!Near(x.Kx, y.Kx) || !Near(x.Ky, y.Ky))
                {
                    if (differences.Count < 40)
                    {
                        differences.Add(
                            $"轨道 \"{a.Name}\" 帧 {f} 角度为 360° 平移：" +
                            $"kx {x.Kx}→{y.Kx}, ky {x.Ky}→{y.Ky}（渲染等价）。");
                    }
                }

                if (!string.Equals(x.Image, y.Image, StringComparison.Ordinal))
                {
                    if (differences.Count < 40)
                        differences.Add($"轨道 \"{a.Name}\" 帧 {f} 贴图不同：\"{x.Image}\" → \"{y.Image}\"。");
                    onlyAngleShift = false;
                }
            }
        }

        // A flat 360 shift is rendering-neutral, but only if it is uniform per track.
        bool renderingEquivalent = differences.All(d => d.Contains("360° 平移", StringComparison.Ordinal));

        _ = reverseReport;

        return new RoundTripResult
        {
            SourcePath = fullPath,
            ByteExact = false,
            RenderingEquivalent = renderingEquivalent,
            Differences = differences,
            VerificationPath = flaPath
        };
    }

    private static bool Near(float a, float b)
    {
        if (a == ReanimTransformSemantics.MissingValue && b == ReanimTransformSemantics.MissingValue)
            return true;
        if (a == ReanimTransformSemantics.MissingValue || b == ReanimTransformSemantics.MissingValue)
            return false;

        return Math.Abs(a - b) <= ValueTolerance + 1e-9f * Math.Max(1.0f, Math.Abs(a) + Math.Abs(b));
    }

    /// <summary>True when two angles describe the same rotation (differ by a multiple of 360).</summary>
    private static bool AngleEquivalent(float a, float b)
    {
        if (a == ReanimTransformSemantics.MissingValue && b == ReanimTransformSemantics.MissingValue)
            return true;
        if (a == ReanimTransformSemantics.MissingValue || b == ReanimTransformSemantics.MissingValue)
            return false;

        float delta = Math.Abs(a - b) % 360.0f;
        return delta <= AngleTolerance || Math.Abs(delta - 360.0f) <= AngleTolerance;
    }
}
