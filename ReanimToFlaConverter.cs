using System.Globalization;
using System.IO;
using System.IO.Compression;
using System.Text;

namespace FlaReanimCompiler;

/// <summary>How faithful the reverse conversion was, per file.</summary>
internal sealed class FlaConversionReport
{
    public required string InputPath { get; init; }
    public required string OutputPath { get; init; }
    public required int TrackCount { get; init; }
    public required int FrameCount { get; init; }
    public required float Fps { get; init; }
    public required int LabelTrackCount { get; init; }
    public required int RenderTrackCount { get; init; }
    public required int LocatorTrackCount { get; init; }
    public required int UnrepresentableTrackCount { get; init; }
    public required int BitmapCount { get; init; }
    public required IReadOnlyList<string> Warnings { get; init; }
}

internal sealed class FlaConversionOptions
{
    /// <summary>Explicit output path. Defaults to the source name with .fla.</summary>
    public string? OutputPath { get; init; }

    /// <summary>
    /// Embed the referenced bitmaps inside the FLA zip so the file is
    /// self-contained and opens correctly in Animate. When false the FLA links to
    /// the existing files in Resources.
    /// </summary>
    public bool EmbedBitmaps { get; init; } = true;

    /// <summary>Emit the lossless engine-loadable .reanim XML next to the FLA.</summary>
    public bool WriteReanimXml { get; init; } = true;
}

/// <summary>
/// Rebuilds an editable Animate ZIP/XFL document from a <c>.reanim.compiled</c>
/// resource.
///
/// Fidelity strategy — each rule below exists to make a specific piece of packed
/// information survive:
///
/// * <b>One layer per reanim track.</b> The engine draws tracks in definition order
///   and the forward converter emits tracks in <c>Array.Reverse(layers)</c> order, so
///   layers are written reversed and labels last, which restores the original order.
/// * <b>A keyframe per distinct frame, never a tween.</b> The forward converter
///   samples classic tweens to a straight line, so tween data is already gone;
///   re-encoding the sampled values as keyframes is lossless instead of approximating
///   twice.
/// * <b>Matrix entries at full double precision.</b> The forward converter re-derives
///   scale and skew with <c>hypot</c>/<c>atan2</c>, which amplifies rounding error.
///   Carrying doubles keeps the recovered values landing on the same 0.1/0.001
///   rounding grid as the original.
/// * <b>Three element kinds, matching the three track shapes in the wild.</b>
///   Bitmap layers reproduce image tracks. Locator layers
///   (<c>libraryItemName="locator"</c>) reproduce tracks that carry transforms but no
///   image — the engine's own convention, which the forward converter recognises by
///   name. Named <c>DOMFrame</c>s reproduce the pure marker tracks, because the
///   forward converter rebuilds those from frame names.
/// * <b>Missing-field placeholders are emitted as their fill-forward value.</b> The
///   writer's compaction turns an unchanged value back into the placeholder, so the
///   packed byte pattern is reproduced.
/// * <b>Library items are named after the packed image id.</b> The forward image
///   resolver re-derives that id from the name, verified for 1468 of the 1471 ids.
///
/// Tracks that a FLA genuinely cannot carry (attacher/text strings, non-rendering
/// data tracks) are reported and preserved in the lossless <c>.reanim.xml</c> sidecar.
/// </summary>
internal static class ReanimToFlaConverter
{
    private const float MissingValue = ReanimTransformSemantics.MissingValue;
    private const int KeyFrameMode = 9728;
    private const string LocatorLibraryItem = "locator";
    private const string LabelLayerName = "_reanim_labels";

    public static FlaConversionReport Convert(string compiledPath, FlaConversionOptions options)
    {
        string fullPath = Path.GetFullPath(compiledPath);
        if (!File.Exists(fullPath))
            throw new FileNotFoundException("找不到 compiled 文件。", fullPath);

        if (!fullPath.EndsWith(".compiled", StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException("只接受 .compiled 文件。");

        ReanimCompiledDocument document = ReanimCompiledReader.Read(fullPath);
        ImageResourceCatalog? catalog = ImageResourceCatalog.TryLoadFor(fullPath);

        var warnings = new List<string>();
        string outputPath = options.OutputPath is { Length: > 0 } explicitPath
            ? Path.GetFullPath(explicitPath)
            : Path.Combine(
                Path.GetDirectoryName(fullPath) ?? "",
                BuildDefaultOutputName(Path.GetFileName(fullPath)));

        FlaModel model = BuildModel(document, catalog, warnings);
        WriteXflArchive(outputPath, model, catalog, options, warnings);

        if (options.WriteReanimXml)
        {
            string xmlPath = Path.ChangeExtension(outputPath, null) + ".reanim.xml";
            ReanimXmlWriter.Write(xmlPath, document, warnings);
        }

        return new FlaConversionReport
        {
            InputPath = fullPath,
            OutputPath = outputPath,
            TrackCount = document.Definition.Tracks.Count,
            FrameCount = document.Definition.FrameCount,
            Fps = document.Definition.Fps,
            LabelTrackCount = model.LabelLayers.Count,
            RenderTrackCount = model.Layers.Count(l => l.Kind == LayerKind.Bitmap),
            LocatorTrackCount = model.Layers.Count(l => l.Kind == LayerKind.Locator),
            UnrepresentableTrackCount = model.UnrepresentableTracks.Count,
            BitmapCount = model.Bitmaps.Count,
            Warnings = warnings
        };
    }

    /// <summary>"Anim.reanim.compiled" -> "Anim.fla".</summary>
    private static string BuildDefaultOutputName(string fileName)
    {
        string stem = fileName[..^".compiled".Length];
        if (stem.EndsWith(".reanim", StringComparison.OrdinalIgnoreCase))
            stem = stem[..^".reanim".Length];

        return stem + ".fla";
    }

    private enum LayerKind
    {
        Bitmap,
        Locator,
        Label
    }

    /// <summary>Intermediate model: track classification plus per-layer frame segments.</summary>
    private sealed class FlaModel
    {
        public required float Fps { get; init; }
        public required int FrameCount { get; init; }

        /// <summary>
        /// Content layers in document order. The order is the reverse of the desired
        /// track order so the forward converter's own reversal restores it.
        /// </summary>
        public required List<LayerModel> Layers { get; init; }

        /// <summary>
        /// Label layers, emitted after the content layers. The forward converter
        /// discovers frame names while scanning reversed layers and always emits label
        /// tracks before render tracks, so these must come last to surface first.
        /// </summary>
        public required List<LayerModel> LabelLayers { get; init; }

        public required List<TrackModel> UnrepresentableTracks { get; init; }
        public required List<ImageResource> Bitmaps { get; init; }
        public required IReadOnlyDictionary<string, string> LibraryNames { get; init; }
        public (int Width, int Height) Canvas { get; set; } = (800, 600);
    }

    private sealed class LayerModel
    {
        public required string Name { get; init; }
        public required LayerKind Kind { get; init; }
        public required List<Segment> Segments { get; init; }
    }

    private sealed class TrackModel
    {
        public required string Name { get; init; }
        public required string Reason { get; init; }
    }

    /// <summary>
    /// One emitted DOMFrame: a run of frames sharing identical content. The writer's
    /// compaction already stores only changed values, so every member of a run would
    /// carry the placeholder for all fields; emitting the run as one frame with a
    /// duration reproduces the same packed bytes while keeping the file small.
    /// </summary>
    private sealed class Segment
    {
        public required int Index { get; init; }
        public required int Duration { get; init; }

        /// <summary>Null for a blank (nothing drawn) run.</summary>
        public ReanimTransform? Transform { get; init; }

        /// <summary>Label name for animation-marker runs.</summary>
        public string? Label { get; init; }
    }

    private static FlaModel BuildModel(
        ReanimCompiledDocument document,
        ImageResourceCatalog? catalog,
        List<string> warnings)
    {
        ReanimDefinition definition = document.Definition;
        int frameCount = definition.FrameCount;

        // Classification must use the RAW packed fields, not the fill-forward view: a
        // pure marker track stores only Frame values, while a locator stores real
        // transform values, and filling blurs that distinction.
        //
        // Content layers (bitmaps and locators) are collected in ORIGINAL TRACK ORDER
        // and reversed as a single list. Grouping them by kind would reorder the
        // definition after the forward converter's own Array.Reverse, and the engine
        // uses track order as draw order, so the grouping would change rendering.
        var content = new List<(TrackContent Track, LayerKind Kind)>();
        var labelTracks = new List<TrackContent>();
        var unrepresentable = new List<TrackModel>();

        foreach (ReanimTrack track in definition.Tracks)
        {
            ReanimTransform[] raw = track.Transforms.ToArray();
            ReanimTransform[] filled = ReanimTransformSemantics.FillForward(raw);
            var trackContent = new TrackContent(track.Name, raw, filled);

            bool hasImage = ReanimTransformSemantics.TrackHasImage(track);
            bool hasText = ReanimTransformSemantics.TrackHasTextData(track);

            // Check the image FIRST. A handful of attacher tracks also carry a real
            // bitmap (a shadow or overlay) that the engine genuinely draws; emitting the
            // bitmap layer keeps that artwork. Only the attacher command itself is lost,
            // and that is reported below and preserved in .reanim.xml.
            if (hasImage)
            {
                if (hasText)
                {
                    AddWarningOnce(
                        warnings,
                        $"轨道 \"{track.Name}\" 同时携带贴图与 attacher 指令：贴图已保留，" +
                        "attacher 指令无法在 FLA 中表达，仅保留在 .reanim.xml 中。");
                }

                content.Add((trackContent, LayerKind.Bitmap));
                continue;
            }

            // Never drawn: the engine bails out of DrawTrack on every frame, and a FLA
            // layer without elements emits no track at all.
            if (ReanimTransformSemantics.IsInert(track) && !hasText)
            {
                unrepresentable.Add(new TrackModel
                {
                    Name = track.Name,
                    Reason = "轨道全程为空白帧，引擎从不绘制，FLA 无法表达空层"
                });
                continue;
            }

            if (hasText)
            {
                unrepresentable.Add(new TrackModel
                {
                    Name = track.Name,
                    Reason = "轨道携带 attacher/文本数据，FLA 无对应元素类型"
                });
                continue;
            }

            if (ReanimTransformSemantics.TrackHasExplicitTransform(track))
            {
                // Drawable but imageless: the engine's own "locator" convention. This
                // also covers anim_* tracks that carry real transforms rather than pure
                // markers, which are locator layers in the source FLA.
                content.Add((trackContent, LayerKind.Locator));
            }
            else if (IsAnimationLabel(track.Name))
            {
                labelTracks.Add(trackContent);
            }
            else
            {
                unrepresentable.Add(new TrackModel
                {
                    Name = track.Name,
                    Reason = DescribeDataReason(track.Name)
                });
            }
        }

        foreach (TrackModel t in unrepresentable)
        {
            AddWarningOnce(
                warnings,
                $"轨道 \"{t.Name}\" 无法在 FLA 中表达（{t.Reason}），已仅保留在 .reanim.xml 中。");
        }

        var bitmapTracks = content.Where(c => c.Kind == LayerKind.Bitmap).Select(c => c.Track).ToList();
        Dictionary<string, string> libraryNames = ResolveBitmaps(bitmapTracks, catalog, warnings, out List<ImageResource> bitmaps);

        // Reverse the single content list so the forward converter's own reversal
        // restores the original track order.
        var layers = new List<LayerModel>();
        for (int i = content.Count - 1; i >= 0; i--)
        {
            (TrackContent trackContent, LayerKind kind) = content[i];
            layers.Add(new LayerModel
            {
                Name = trackContent.Name,
                Kind = kind,
                Segments = kind == LayerKind.Bitmap
                    ? BuildBitmapSegments(trackContent, frameCount, warnings)
                    : BuildLocatorSegments(trackContent, frameCount)
            });
        }

        // Label layers last in the document, themselves reversed so discovery order
        // matches the desired label order.
        var labelLayers = new List<LayerModel>();
        for (int i = labelTracks.Count - 1; i >= 0; i--)
        {
            TrackContent labelTrack = labelTracks[i];
            List<Segment> segments = BuildLabelSegments(labelTrack, frameCount);
            if (segments.Count == 0)
                continue;

            labelLayers.Add(new LayerModel
            {
                Name = $"{LabelLayerName}_{Sanitize(labelTrack.Name)}",
                Kind = LayerKind.Label,
                Segments = segments
            });
        }

        if (labelLayers.Count > 0)
        {
            AddWarningOnce(
                warnings,
                $"已将 {labelTracks.Count} 条 anim_ 标记轨转为命名帧（层 \"{LabelLayerName}\"），" +
                "正向转换会据此重建同名标记轨。");
        }

        var model = new FlaModel
        {
            Fps = definition.Fps,
            FrameCount = frameCount,
            Layers = layers,
            LabelLayers = labelLayers,
            UnrepresentableTracks = unrepresentable,
            Bitmaps = bitmaps,
            LibraryNames = libraryNames
        };

        model.Canvas = ComputeCanvas(model, catalog);
        return model;
    }

    private sealed record TrackContent(string Name, ReanimTransform[] Raw, ReanimTransform[] Filled);

    private static string DescribeDataReason(string trackName)
    {
        if (trackName.StartsWith("_", StringComparison.Ordinal))
            return "非渲染数据轨道（名称以下划线开头）";

        if (trackName.Equals("fullscreen", StringComparison.OrdinalIgnoreCase))
            return "无贴图的全屏填充轨道";

        return "轨道没有任何贴图与变换，且不是 anim_ 动画标记";
    }

    private static bool IsAnimationLabel(string trackName) =>
        trackName.Trim().StartsWith("anim_", StringComparison.OrdinalIgnoreCase);

    private static Dictionary<string, string> ResolveBitmaps(
        List<TrackContent> bitmapTracks,
        ImageResourceCatalog? catalog,
        List<string> warnings,
        out List<ImageResource> bitmaps)
    {
        bitmaps = [];
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var names = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);

        foreach (TrackContent content in bitmapTracks)
        {
            foreach (ReanimTransform t in content.Raw)
            {
                if (string.IsNullOrEmpty(t.Image) || !seen.Add(t.Image))
                    continue;

                ImageResource? resource = catalog?.Resolve(t.Image);
                if (resource is null)
                {
                    // Keep the reference round-trippable: name the library item after
                    // the packed id so the forward resolver rebuilds that same id.
                    string fallback = ImageResourceCatalog.GetFallbackLibraryItemName(t.Image);
                    names[t.Image] = fallback;
                    AddWarningOnce(
                        warnings,
                        $"贴图 \"{t.Image}\" 在磁盘上找不到文件，已按资源 id 生成库项目 \"{fallback}\"。");
                    continue;
                }

                bitmaps.Add(resource);
                names[t.Image] = ImageResourceCatalog.GetLibraryItemName(resource);
            }
        }

        return names;
    }

    /// <summary>
    /// Collapses the frame sequence into runs of identical <b>drawable</b> content.
    /// </summary>
    private static List<Segment> BuildBitmapSegments(TrackContent content, int frameCount, List<string> warnings)
    {
        ReanimTransform[] filled = content.Filled;
        var segments = new List<Segment>();
        bool warnedMissingImage = false;
        int index = 0;

        while (index < frameCount)
        {
            ReanimTransform current = filled[index];
            bool drawable = current.Frame >= 0.0f && !string.IsNullOrEmpty(current.Image);

            if (current.Frame >= 0.0f && string.IsNullOrEmpty(current.Image) && !warnedMissingImage)
            {
                warnedMissingImage = true;
                AddWarningOnce(
                    warnings,
                    $"轨道 \"{content.Name}\" 在部分帧上有帧号但没有贴图，这些帧已按空白帧写出。");
            }

            int runEnd = index + 1;
            while (runEnd < frameCount && IsSameBitmapContent(filled[runEnd], current, drawable))
                runEnd++;

            segments.Add(new Segment
            {
                Index = index,
                Duration = runEnd - index,
                Transform = drawable ? current : null
            });

            index = runEnd;
        }

        return segments;
    }

    private static bool IsSameBitmapContent(ReanimTransform a, ReanimTransform b, bool drawable)
    {
        if (!drawable)
            return a.Frame < 0.0f || string.IsNullOrEmpty(a.Image);

        if (a.Frame < 0.0f || string.IsNullOrEmpty(a.Image))
            return false;

        return a == b;
    }

    /// <summary>
    /// Collapses a locator track into runs of constant transform. Locator frames are
    /// "drawable" purely by virtue of Frame &gt;= 0; there is no image to test.
    /// </summary>
    private static List<Segment> BuildLocatorSegments(TrackContent content, int frameCount)
    {
        ReanimTransform[] filled = content.Filled;
        var segments = new List<Segment>();
        int index = 0;

        while (index < frameCount)
        {
            ReanimTransform current = filled[index];
            bool drawable = current.Frame >= 0.0f;

            int runEnd = index + 1;
            while (runEnd < frameCount &&
                   (filled[runEnd].Frame >= 0.0f) == drawable &&
                   (!drawable || filled[runEnd] == current))
            {
                runEnd++;
            }

            segments.Add(new Segment
            {
                Index = index,
                Duration = runEnd - index,
                Transform = drawable ? current : null
            });

            index = runEnd;
        }

        return segments;
    }

    /// <summary>
    /// A pure marker track is a run of <c>Marker()</c> values bounded by blanks. The
    /// forward converter rebuilds exactly that shape from a named DOMFrame spanning the
    /// run, so each run becomes one named frame. The layer name deliberately avoids the
    /// <c>anim_</c> prefix so it produces no track of its own.
    /// </summary>
    private static List<Segment> BuildLabelSegments(TrackContent content, int frameCount)
    {
        ReanimTransform[] filled = content.Filled;
        var segments = new List<Segment>();
        int index = 0;

        while (index < frameCount)
        {
            bool marker = filled[index].Frame >= 0.0f;

            int runEnd = index + 1;
            while (runEnd < frameCount && (filled[runEnd].Frame >= 0.0f) == marker)
                runEnd++;

            if (marker)
            {
                segments.Add(new Segment
                {
                    Index = index,
                    Duration = runEnd - index,
                    Label = content.Name
                });
            }

            index = runEnd;
        }

        return segments;
    }

    private static string Sanitize(string name)
    {
        Span<char> buffer = stackalloc char[name.Length];
        for (int i = 0; i < name.Length; i++)
            buffer[i] = char.IsLetterOrDigit(name[i]) || name[i] == '_' ? name[i] : '_';

        return new string(buffer);
    }

    /// <summary>
    /// Sizes the document from the real transformed bitmap bounds so the rebuilt FLA
    /// opens with the artwork framed correctly in Animate.
    /// </summary>
    private static (int Width, int Height) ComputeCanvas(FlaModel model, ImageResourceCatalog? catalog)
    {
        double minX = double.MaxValue, minY = double.MaxValue;
        double maxX = double.MinValue, maxY = double.MinValue;
        bool any = false;

        foreach (LayerModel layer in model.Layers)
        {
            if (layer.Kind != LayerKind.Bitmap)
                continue;

            foreach (Segment segment in layer.Segments)
            {
                if (segment.Transform is not { } transform)
                    continue;

                ImageResource? resource = catalog?.Resolve(transform.Image);
                int celWidth = resource?.Width ?? 0;
                int celHeight = resource?.Height ?? 0;
                if (celWidth <= 0 || celHeight <= 0)
                    continue;

                (double a, double b, double c, double d) = Decompose(transform);

                // The engine centres each cel on its own half-size before applying the
                // transform, so the corners sit at +/- half the cel size in local space.
                foreach ((double lx, double ly) in new[]
                {
                    (-celWidth / 2.0, -celHeight / 2.0), (celWidth / 2.0, -celHeight / 2.0),
                    (-celWidth / 2.0, celHeight / 2.0), (celWidth / 2.0, celHeight / 2.0)
                })
                {
                    double px = a * lx + c * ly + transform.X;
                    double py = b * lx + d * ly + transform.Y;
                    minX = Math.Min(minX, px);
                    minY = Math.Min(minY, py);
                    maxX = Math.Max(maxX, px);
                    maxY = Math.Max(maxY, py);
                    any = true;
                }
            }
        }

        if (!any)
            return (800, 600);

        const double margin = 40.0;
        return ((int)Math.Ceiling(Math.Max(64.0, maxX - minX + margin * 2.0)),
                (int)Math.Ceiling(Math.Max(64.0, maxY - minY + margin * 2.0)));
    }

    /// <summary>
    /// Inverse of the forward converter's <c>BuildTransform</c> decomposition,
    /// mirroring the engine's <c>MatrixFromTransform</c>.
    /// </summary>
    private static (double A, double B, double C, double D) Decompose(ReanimTransform transform)
    {
        double radX = transform.Kx * Math.PI / 180.0;
        double radY = transform.Ky * Math.PI / 180.0;
        return (
            transform.Sx * Math.Cos(radX),
            transform.Sx * Math.Sin(radX),
            -transform.Sy * Math.Sin(radY),
            transform.Sy * Math.Cos(radY));
    }

    private static void WriteXflArchive(
        string outputPath,
        FlaModel model,
        ImageResourceCatalog? catalog,
        FlaConversionOptions options,
        List<string> warnings)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(outputPath) ?? ".");

        using FileStream stream = File.Create(outputPath);
        using var archive = new ZipArchive(stream, ZipArchiveMode.Create);

        // The FLA package must begin with an uncompressed "mimetype" entry naming the
        // document type, placed before any other entry. Animate refuses to open the
        // file when this is missing or not first, so it is written up front.
        WriteMimeTypeEntry(archive);

        // An empty symbol named "locator": the forward converter looks it up by name,
        // finds no renderable content, and therefore keeps the instance as a pure
        // transform placeholder. Shipping it avoids a spurious "missing library item"
        // warning and keeps the FLA self-contained.
        if (model.Layers.Any(l => l.Kind == LayerKind.Locator))
        {
            WriteEntry(archive, $"LIBRARY/{LocatorLibraryItem}.xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n" +
                "<DOMSymbolItem xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" " +
                $"name=\"{LocatorLibraryItem}\" symbolType=\"movie clip\">\n" +
                "  <timeline>\n" +
                $"    <DOMTimeline name=\"{LocatorLibraryItem}\"/>\n" +
                "  </timeline>\n" +
                "</DOMSymbolItem>\n");
        }

        // Maps each library item id to the path of the bitmap entry actually stored in
        // the zip, so the media list (href/bitmapDataHRef) and the embedded files agree.
        // Embedding names the entry "images/<ImageId>.<ext>", while the link path keeps
        // the original RelativePath; the DOM references whichever was produced here.
        var bitmapHrefs = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);

        if (options.EmbedBitmaps && catalog is not null)
        {
            var written = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (ImageResource resource in model.Bitmaps)
            {
                string source = Path.Combine(catalog.ResourcesRoot, resource.RelativePath);
                string entryName = "images/" + resource.ImageId + Path.GetExtension(resource.RelativePath).ToLowerInvariant();

                if (model.LibraryNames.TryGetValue(resource.ImageId, out string? name))
                    bitmapHrefs[name] = entryName;

                if (!written.Add(entryName) || !File.Exists(source))
                    continue;

                ZipArchiveEntry entry = archive.CreateEntry(entryName, CompressionLevel.SmallestSize);
                using Stream target = entry.Open();
                using FileStream origin = File.OpenRead(source);
                origin.CopyTo(target);
            }
        }
        else if (!options.EmbedBitmaps)
        {
            // Link mode: the href points at the on-disk location under Resources.
            foreach (ImageResource resource in model.Bitmaps)
                if (model.LibraryNames.TryGetValue(resource.ImageId, out string? name))
                    bitmapHrefs[name] = resource.RelativePath;

            AddWarningOnce(warnings, "已按链接方式输出：FLA 通过 href 引用 Resources 中的原始贴图。");
        }

        WriteEntry(archive, "DOMDocument.xml", BuildDomDocument(model, bitmapHrefs));
    }

    /// <summary>
    /// Writes the mandatory FLA mimetype entry. It must be the first entry and stored
    /// uncompressed, or Animate rejects the document as not a valid FLA.
    /// </summary>
    private static void WriteMimeTypeEntry(ZipArchive archive)
    {
        ZipArchiveEntry entry = archive.CreateEntry("mimetype", CompressionLevel.NoCompression);
        using Stream stream = entry.Open();
        using var writer = new StreamWriter(stream, new UTF8Encoding(false));
        writer.Write("application/vnd.adobe.flash.file");
    }

    private static void WriteEntry(ZipArchive archive, string name, string content)
    {
        ZipArchiveEntry entry = archive.CreateEntry(name, CompressionLevel.SmallestSize);
        using Stream stream = entry.Open();
        using var writer = new StreamWriter(stream, new UTF8Encoding(false));
        writer.Write(content);
    }

    private static string BuildDomDocument(FlaModel model, IReadOnlyDictionary<string, string> bitmapHrefs)
    {
        var sb = new StringBuilder();
        sb.Append("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        sb.Append("<DOMDocument xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"")
          .Append(" xsi:noNamespaceSchemaLocation=\"http://ns.adobe.com/xfl/2008/\"")
          .Append(" width=\"").Append(model.Canvas.Width).Append('"')
          .Append(" height=\"").Append(model.Canvas.Height).Append('"')
          .Append(" frameRate=\"").Append(FormatDouble(model.Fps)).Append('"')
          .Append(" currentTimeline=\"1\">\n");

        sb.Append("  <timelines>\n");
        sb.Append("    <DOMTimeline name=\"Scene 1\">\n");
        sb.Append("      <layers>\n");

        foreach (LayerModel layer in model.Layers)
            AppendLayer(sb, layer, model.FrameCount);

        foreach (LayerModel layer in model.LabelLayers)
            AppendLayer(sb, layer, model.FrameCount);

        sb.Append("      </layers>\n");
        sb.Append("    </DOMTimeline>\n");
        sb.Append("  </timelines>\n");

        AppendMedia(sb, model, bitmapHrefs);
        sb.Append("</DOMDocument>\n");
        return sb.ToString();
    }

    private static void AppendLayer(StringBuilder sb, LayerModel layer, int frameCount)
    {
        sb.Append("        <DOMLayer name=\"").Append(Escape(layer.Name))
          .Append("\" color=\"#4F81BD\" current=\"true\" isSelected=\"true\">\n");
        sb.Append("          <frames>\n");

        int cursor = 0;
        foreach (Segment segment in layer.Segments)
        {
            // DOMFrame indexes must tile the timeline contiguously for the forward
            // converter's index arithmetic to line up with the packed frame numbers.
            int index = segment.Index;
            int duration = segment.Duration;

            if (index > cursor)
            {
                AppendEmptyFrame(sb, cursor, index - cursor, null);
                cursor = index;
            }

            if (segment.Transform is { } transform)
            {
                sb.Append("            <DOMFrame index=\"").Append(index).Append('"')
                  .Append(" duration=\"").Append(duration).Append('"')
                  .Append(" keyMode=\"").Append(KeyFrameMode).Append("\">\n");
                sb.Append("              <elements>\n");
                if (layer.Kind == LayerKind.Bitmap)
                    AppendBitmapInstance(sb, transform);
                else
                    AppendLocatorInstance(sb, transform);
                sb.Append("              </elements>\n");
                sb.Append("            </DOMFrame>\n");
            }
            else
            {
                AppendEmptyFrame(sb, index, duration, segment.Label);
            }

            cursor = index + duration;
        }

        if (cursor < frameCount)
            AppendEmptyFrame(sb, cursor, frameCount - cursor, null);

        sb.Append("          </frames>\n");
        sb.Append("        </DOMLayer>\n");
    }

    private static void AppendEmptyFrame(StringBuilder sb, int index, int duration, string? label)
    {
        if (duration <= 0)
            return;

        sb.Append("            <DOMFrame index=\"").Append(index).Append('"')
          .Append(" duration=\"").Append(duration).Append('"')
          .Append(" keyMode=\"").Append(KeyFrameMode).Append('"');

        if (label is { Length: > 0 })
            sb.Append(" name=\"").Append(Escape(label)).Append('"');

        sb.Append("/>\n");
    }

    /// <summary>
    /// Writes the decomposed transform back as an affine matrix. Entries are emitted at
    /// full double precision because the forward converter re-derives scale and skew
    /// with <c>hypot</c>/<c>atan2</c>, which amplifies rounding error; truncating to
    /// float makes a small number of frames land on the wrong side of its 0.001
    /// rounding grid.
    /// </summary>
    private static void AppendBitmapInstance(StringBuilder sb, ReanimTransform transform)
    {
        (double a, double b, double c, double d) = Decompose(transform);

        sb.Append("                <DOMBitmapInstance libraryItemName=\"")
          .Append(Escape(transform.Image)).Append('"');

        // Cel index is carried by firstFrame; the forward converter reads it back with
        // GetFloat(element, "firstFrame", 0) and writes it verbatim.
        int cel = (int)Math.Floor(transform.Frame);
        if (cel != 0)
            sb.Append(" firstFrame=\"").Append(cel).Append('"');

        sb.Append(">\n");
        AppendMatrixAndColor(sb, transform, a, b, c, d);
        sb.Append("                </DOMBitmapInstance>\n");
    }

    /// <summary>
    /// A locator is a symbol instance, not a bitmap one, because its library item is an
    /// empty movie clip. The forward converter resolves the missing renderable through
    /// <c>IsLocatorLibraryItem</c> and keeps the instance as a transform placeholder.
    /// </summary>
    private static void AppendLocatorInstance(StringBuilder sb, ReanimTransform transform)
    {
        (double a, double b, double c, double d) = Decompose(transform);

        sb.Append("                <DOMSymbolInstance libraryItemName=\"")
          .Append(LocatorLibraryItem).Append('"');

        int cel = (int)Math.Floor(transform.Frame);
        if (cel != 0)
            sb.Append(" firstFrame=\"").Append(cel).Append('"');

        sb.Append(">\n");
        AppendMatrixAndColor(sb, transform, a, b, c, d);
        sb.Append("                </DOMSymbolInstance>\n");
    }

    private static void AppendMatrixAndColor(
        StringBuilder sb,
        ReanimTransform transform,
        double a, double b, double c, double d)
    {
        sb.Append("                  <matrix>\n");
        sb.Append("                    <Matrix a=\"").Append(FormatDouble(a))
          .Append("\" b=\"").Append(FormatDouble(b))
          .Append("\" c=\"").Append(FormatDouble(c))
          .Append("\" d=\"").Append(FormatDouble(d))
          .Append("\" tx=\"").Append(FormatDouble(transform.X))
          .Append("\" ty=\"").Append(FormatDouble(transform.Y)).Append("\"/>\n");
        sb.Append("                  </matrix>\n");
        sb.Append("                  <color>\n");
        sb.Append("                    <Color alphaMultiplier=\"").Append(FormatDouble(transform.Alpha)).Append("\"/>\n");
        sb.Append("                  </color>\n");
    }

    private static void AppendMedia(StringBuilder sb, FlaModel model, IReadOnlyDictionary<string, string> bitmapHrefs)
    {
        if (model.Bitmaps.Count == 0)
            return;

        sb.Append("  <media>\n");
        foreach (ImageResource resource in model.Bitmaps)
        {
            if (!model.LibraryNames.TryGetValue(resource.ImageId, out string? name))
                name = resource.ImageId;

            // Reference the same path that was actually embedded in the zip (or, in link
            // mode, the on-disk RelativePath). This keeps the DOM bitmap reference and the
            // stored file in sync, otherwise Animate cannot resolve the artwork.
            string href = bitmapHrefs.TryGetValue(name, out string? resolved) && resolved.Length > 0
                ? resolved
                : resource.RelativePath;

            sb.Append("    <DOMBitmapItem name=\"").Append(Escape(name))
              .Append("\" href=\"").Append(Escape(href)).Append('"')
              .Append(" bitmapDataHRef=\"").Append(Escape(href)).Append('"');

            if (resource.Width > 0 && resource.Height > 0)
            {
                // Flash stores the frame rect as an exclusive right/bottom corner in twips.
                sb.Append(" frameRight=\"").Append(resource.Width * 20).Append('"')
                  .Append(" frameBottom=\"").Append(resource.Height * 20).Append('"');
            }

            sb.Append("/>\n");
        }
        sb.Append("  </media>\n");
    }

    internal static string FormatFloat(float value) =>
        value.ToString("R", CultureInfo.InvariantCulture);

    /// <summary>Full-precision double formatting for matrix entries.</summary>
    internal static string FormatDouble(double value)
    {
        if (double.IsNaN(value) || double.IsInfinity(value))
            return "0";

        string text = value.ToString("R", CultureInfo.InvariantCulture);
        return text.Contains('E', StringComparison.OrdinalIgnoreCase)
            ? value.ToString("0.0################", CultureInfo.InvariantCulture)
            : text;
    }

    private static string Escape(string value) => value
        .Replace("&", "&amp;", StringComparison.Ordinal)
        .Replace("<", "&lt;", StringComparison.Ordinal)
        .Replace(">", "&gt;", StringComparison.Ordinal)
        .Replace("\"", "&quot;", StringComparison.Ordinal);

    internal static void AddWarningOnce(List<string> warnings, string warning)
    {
        if (!warnings.Contains(warning))
            warnings.Add(warning);
    }
}
