using System.Globalization;
using System.IO;
using System.IO.Compression;
using System.Text;
using System.Text.RegularExpressions;
using System.Xml.Linq;

namespace FlaReanimCompiler;

public sealed class ConversionResult
{
    public required string InputPath { get; init; }
    public required string OutputPath { get; init; }
    public required int TrackCount { get; init; }
    public required int FrameCount { get; init; }
    public required float Fps { get; init; }
    public required IReadOnlyList<string> Warnings { get; init; }
}

public sealed partial class FlaToReanimConverter
{
    private const float DefaultFps = 12.0f;

    public ConversionResult Convert(string flaPath)
    {
        if (string.IsNullOrWhiteSpace(flaPath))
            throw new ArgumentException("FLA 路径为空。", nameof(flaPath));

        string fullPath = Path.GetFullPath(flaPath);
        if (!File.Exists(fullPath))
            throw new FileNotFoundException("找不到 FLA 文件。", fullPath);

        if (!string.Equals(Path.GetExtension(fullPath), ".fla", StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException("只接受 .fla 文件。");

        var warnings = new List<string>();
        using FlaPackage package = FlaPackage.Open(fullPath);
        TimelineSource timeline = package.SelectTimeline(warnings);
        ReanimDefinition definition = BuildDefinition(package, timeline, warnings);

        if (definition.Tracks.Count == 0)
            throw new InvalidOperationException("没有在 FLA 时间轴里找到可转换的图层或动画标签。");

        string outputPath = Path.Combine(
            Path.GetDirectoryName(fullPath) ?? "",
            Path.GetFileNameWithoutExtension(fullPath) + ".reanim.compiled");

        ReanimCompiledWriter.Write(outputPath, definition);

        return new ConversionResult
        {
            InputPath = fullPath,
            OutputPath = outputPath,
            TrackCount = definition.Tracks.Count,
            FrameCount = definition.FrameCount,
            Fps = definition.Fps,
            Warnings = warnings
        };
    }

    private static ReanimDefinition BuildDefinition(FlaPackage package, TimelineSource source, List<string> warnings)
    {
        XElement timeline = source.Timeline;
        XElement[] layers = timeline
            .ElementsLocal("layers")
            .ElementsLocal("DOMLayer")
            .Where(layer => !IsGuideOrFolderLayer(layer))
            .ToArray();
        Array.Reverse(layers);

        int totalFrames = Math.Max(1, layers
            .SelectMany(layer => GetFrames(layer))
            .Select(frame => GetInt(frame, "index", 0) + Math.Max(1, GetInt(frame, "duration", 1)))
            .DefaultIfEmpty(1)
            .Max());

        float fps = GetFloat(source.DocumentRoot, "frameRate", DefaultFps);
        if (fps <= 0.0f)
            fps = DefaultFps;

        var definition = new ReanimDefinition(fps, totalFrames);
        AddFrameLabelTracks(definition, layers);

        int layerIndex = 0;
        foreach (XElement layer in layers)
        {
            layerIndex++;
            string layerName = GetString(layer, "name");
            XElement[] frames = GetFrames(layer).ToArray();
            if (frames.Length == 0)
                continue;

            int renderableCount = frames
                .Select(frame => FindRenderables(package, frame, warnings, layerName, GetInt(frame, "index", 0)).Count)
                .DefaultIfEmpty(0)
                .Max();
            if (renderableCount == 0)
            {
                if (IsAnimationTrackName(layerName) && !definition.Tracks.Any(track => SameTrackName(track.Name, layerName)))
                    definition.Tracks.Add(BuildLayerMarkerTrack(layerName, frames, totalFrames));
                continue;
            }

            string baseTrackName = string.IsNullOrWhiteSpace(layerName)
                ? $"track_{layerIndex}"
                : layerName.Trim();

            for (int renderableIndex = 0; renderableIndex < renderableCount; renderableIndex++)
            {
                string trackName = MakeRenderableTrackName(baseTrackName, renderableIndex);
                var track = new ReanimTrack(trackName, totalFrames);
                FillRenderableTrack(package, track, frames, totalFrames, warnings, renderableIndex);
                NormalizeTrackAngles(track);
                definition.Tracks.Add(track);
            }
        }

        if (!definition.Tracks.Any(track => IsAnimationTrackName(track.Name)))
        {
            var allTrack = new ReanimTrack("anim_full", totalFrames);
            for (int i = 0; i < totalFrames; i++)
                allTrack.Transforms[i] = ReanimTransform.Marker();
            definition.Tracks.Insert(0, allTrack);
            warnings.Add("未找到 anim_* 标签轨，已生成 anim_full 覆盖全部帧。");
        }

        return definition;
    }

    private static void AddFrameLabelTracks(ReanimDefinition definition, IEnumerable<XElement> layers)
    {
        var labels = new Dictionary<string, List<(int Index, int Duration)>>(StringComparer.OrdinalIgnoreCase);

        foreach (XElement frame in layers.SelectMany(GetFrames))
        {
            string label = GetString(frame, "name");
            if (!IsAnimationTrackName(label))
                continue;

            int index = GetInt(frame, "index", 0);
            int duration = Math.Max(1, GetInt(frame, "duration", 1));
            if (!labels.TryGetValue(label, out var ranges))
            {
                ranges = new List<(int Index, int Duration)>();
                labels.Add(label, ranges);
            }
            ranges.Add((index, duration));
        }

        foreach ((string label, List<(int Index, int Duration)> ranges) in labels)
            definition.Tracks.Add(BuildFrameLabelTrack(label, ranges, definition.FrameCount));
    }

    private static ReanimTrack BuildLayerMarkerTrack(string layerName, XElement[] frames, int totalFrames)
    {
        var track = new ReanimTrack(layerName.Trim(), totalFrames);
        XElement[] sortedFrames = frames
            .OrderBy(frame => GetInt(frame, "index", 0))
            .ToArray();

        if (sortedFrames.Length == 0)
            return track;

        int activeFrameIndex = sortedFrames.Length == 2 && IsOpeningTrackName(layerName) ? 0 : 1;
        if (activeFrameIndex >= sortedFrames.Length)
            return track;

        XElement activeFrame = sortedFrames[activeFrameIndex];
        int index = GetInt(activeFrame, "index", 0);
        int duration = Math.Max(1, GetInt(activeFrame, "duration", 1));
        int end = Math.Min(totalFrames, index + duration);
        for (int i = Math.Max(0, index); i < end; i++)
            track.Transforms[i] = ReanimTransform.Marker();

        return track;
    }

    private static ReanimTrack BuildFrameLabelTrack(string label, List<(int Index, int Duration)> ranges, int totalFrames)
    {
        var track = new ReanimTrack(label, totalFrames);
        foreach ((int index, int duration) in ranges)
        {
            int end = Math.Min(totalFrames, index + duration);
            for (int i = Math.Max(0, index); i < end; i++)
                track.Transforms[i] = ReanimTransform.Marker();
        }
        return track;
    }

    private static void FillRenderableTrack(
        FlaPackage package,
        ReanimTrack track,
        XElement[] frames,
        int totalFrames,
        List<string> warnings,
        int renderableIndex)
    {
        Array.Sort(frames, (a, b) => GetInt(a, "index", 0).CompareTo(GetInt(b, "index", 0)));

        for (int keyIndex = 0; keyIndex < frames.Length; keyIndex++)
        {
            XElement frame = frames[keyIndex];
            int frameStart = GetInt(frame, "index", 0);
            int frameDuration = Math.Max(1, GetInt(frame, "duration", 1));
            int frameEnd = Math.Min(totalFrames, frameStart + frameDuration);

            ResolvedRenderable? renderable = GetRenderableAt(package, frame, warnings, track.Name, frameStart, renderableIndex);
            if (renderable is null)
            {
                FillRange(track, frameStart, frameEnd, ReanimTransform.Blank());
                continue;
            }

            ReanimTransform start = BuildTransform(package, renderable.Value, warnings, track.Name, frameStart);
            ResolvedRenderable? nextRenderable = keyIndex + 1 < frames.Length
                ? GetRenderableAt(package, frames[keyIndex + 1], warnings, track.Name, GetInt(frames[keyIndex + 1], "index", frameStart), renderableIndex)
                : null;

            if (!IsMotionTween(frame) || nextRenderable is not ResolvedRenderable endRenderable)
            {
                FillRange(track, frameStart, frameEnd, start);
                continue;
            }

            ReanimTransform end = BuildTransform(package, endRenderable, warnings, track.Name, frameStart);
            int span = Math.Max(1, frameEnd - frameStart);
            for (int frameOffset = 0; frameOffset < span; frameOffset++)
            {
                float t = frameOffset / (float)span;
                track.Transforms[frameStart + frameOffset] = BuildTweenTransform(renderable.Value, endRenderable, start, end, t);
            }
        }
    }

    private static string MakeRenderableTrackName(string baseTrackName, int renderableIndex) =>
        renderableIndex == 0 ? baseTrackName : $"{baseTrackName}{renderableIndex + 1}";

    private static ResolvedRenderable? GetRenderableAt(
        FlaPackage package,
        XElement frame,
        List<string> warnings,
        string trackName,
        int frameIndex,
        int renderableIndex)
    {
        List<ResolvedRenderable> renderables = FindRenderables(package, frame, warnings, trackName, frameIndex);
        return renderableIndex >= 0 && renderableIndex < renderables.Count
            ? renderables[renderableIndex]
            : null;
    }

    private static List<ResolvedRenderable> FindRenderables(
        FlaPackage package,
        XElement frame,
        List<string> warnings,
        string trackName,
        int frameIndex)
    {
        XElement? elements = frame.ElementsLocal("elements").FirstOrDefault();
        if (elements is null)
            return [];

        var renderables = new List<ResolvedRenderable>();
        CollectRenderables(package, elements, Matrix2D.Identity, 1.0f, warnings, trackName, frameIndex, 0, renderables);
        return renderables;
    }

    private static void CollectRenderables(
        FlaPackage package,
        XElement container,
        Matrix2D parentMatrix,
        float parentAlpha,
        List<string> warnings,
        string trackName,
        int frameIndex,
        int depth,
        List<ResolvedRenderable> renderables)
    {
        foreach (XElement element in container.Elements())
        {
            Matrix2D matrix = parentMatrix * Matrix2D.FromElement(element);
            float alpha = parentAlpha * GetAlpha(GetElementColor(element));
            string elementName = element.Name.LocalName;

            if (elementName == "DOMBitmapInstance")
            {
                renderables.Add(new ResolvedRenderable(element, matrix, alpha, TransformationPoint: GetTransformationPoint(element)));
                continue;
            }

            if (elementName is "DOMSymbolInstance" or "DOMGraphicInstance")
            {
                if (depth >= 16)
                {
                    AddWarningOnce(warnings, $"{trackName}: frame {frameIndex} symbol nesting is too deep; skipped.");
                    continue;
                }

                string libraryItemName = GetString(element, "libraryItemName");
                XElement? libraryRoot = package.FindLibraryRoot(libraryItemName, warnings);
                if (libraryRoot is null)
                {
                    AddWarningOnce(warnings, $"{trackName}: frame {frameIndex} missing library item \"{libraryItemName}\"; skipped.");
                    continue;
                }

                int symbolFrame = Math.Max(0, (int)Math.Floor(GetFloat(element, "firstFrame", 0.0f)));
                Point2D transformationPoint = GetTransformationPoint(element);
                int renderableCountBeforeLibrary = renderables.Count;
                ResolveLibraryFrameRenderables(
                    package,
                    libraryRoot,
                    symbolFrame,
                    matrix,
                    alpha,
                    warnings,
                    trackName,
                    frameIndex,
                    depth + 1,
                    transformationPoint,
                    renderables);

                if (renderables.Count == renderableCountBeforeLibrary && IsLocatorLibraryItem(libraryItemName))
                    renderables.Add(new ResolvedRenderable(element, matrix, alpha, "", transformationPoint));
                continue;
            }

            CollectRenderables(package, element, matrix, alpha, warnings, trackName, frameIndex, depth, renderables);
        }
    }

    private static void ResolveLibraryFrameRenderables(
        FlaPackage package,
        XElement libraryRoot,
        int symbolFrame,
        Matrix2D parentMatrix,
        float parentAlpha,
        List<string> warnings,
        string trackName,
        int frameIndex,
        int depth,
        Point2D transformationPoint,
        List<ResolvedRenderable> renderables)
    {
        List<ResolvedRenderable>? fallback = null;

        foreach (XElement timeline in FlaPackage.GetTimelineElements(libraryRoot))
        {
            XElement[] layers = timeline
                .ElementsLocal("layers")
                .ElementsLocal("DOMLayer")
                .Where(layer => !IsGuideOrFolderLayer(layer))
                .ToArray();

            foreach (XElement frame in layers.SelectMany(GetFrames))
            {
                int index = GetInt(frame, "index", 0);
                int duration = Math.Max(1, GetInt(frame, "duration", 1));

                XElement? elements = frame.ElementsLocal("elements").FirstOrDefault();
                if (elements is null)
                    continue;

                var frameRenderables = new List<ResolvedRenderable>();
                CollectRenderables(
                    package,
                    elements,
                    parentMatrix,
                    parentAlpha,
                    warnings,
                    trackName,
                    frameIndex,
                    depth,
                    frameRenderables);

                if (frameRenderables.Count == 0)
                    continue;

                for (int i = 0; i < frameRenderables.Count; i++)
                    frameRenderables[i] = frameRenderables[i] with { TransformationPoint = transformationPoint };

                fallback ??= frameRenderables;
                if (symbolFrame >= index && symbolFrame < index + duration)
                {
                    renderables.AddRange(frameRenderables);
                    return;
                }
            }
        }

        if (fallback is not null)
            renderables.AddRange(fallback);
    }

    private static ResolvedRenderable? FindRenderable(
        FlaPackage package,
        XElement frame,
        List<string> warnings,
        string trackName,
        int frameIndex)
    {
        XElement? elements = frame.ElementsLocal("elements").FirstOrDefault();
        if (elements is null)
            return null;

        return ResolveFirstRenderable(package, elements, Matrix2D.Identity, 1.0f, warnings, trackName, frameIndex, 0);
    }

    private static int CountRenderables(XElement frame)
    {
        XElement? elements = frame.ElementsLocal("elements").FirstOrDefault();
        return elements?.DescendantsAndSelf().Count(IsRenderableElement) ?? 0;
    }

    private static bool IsRenderableElement(XElement element)
    {
        string name = element.Name.LocalName;
        return name is "DOMSymbolInstance" or "DOMBitmapInstance" or "DOMGraphicInstance";
    }

    private static ResolvedRenderable? ResolveFirstRenderable(
        FlaPackage package,
        XElement container,
        Matrix2D parentMatrix,
        float parentAlpha,
        List<string> warnings,
        string trackName,
        int frameIndex,
        int depth)
    {
        foreach (XElement element in container.Elements())
        {
            ResolvedRenderable? resolved = ResolveRenderable(
                package,
                element,
                parentMatrix,
                parentAlpha,
                warnings,
                trackName,
                frameIndex,
                depth);

            if (resolved is not null)
                return resolved;
        }

        return null;
    }

    private static ResolvedRenderable? ResolveRenderable(
        FlaPackage package,
        XElement element,
        Matrix2D parentMatrix,
        float parentAlpha,
        List<string> warnings,
        string trackName,
        int frameIndex,
        int depth)
    {
        Matrix2D matrix = parentMatrix * Matrix2D.FromElement(element);
        float alpha = parentAlpha * GetAlpha(GetElementColor(element));

        if (element.Name.LocalName == "DOMBitmapInstance")
            return new ResolvedRenderable(element, matrix, alpha);

        if (element.Name.LocalName is "DOMSymbolInstance" or "DOMGraphicInstance")
        {
            if (depth >= 16)
            {
                AddWarningOnce(warnings, $"{trackName}: frame {frameIndex} 的符号嵌套过深，已停止继续解析。");
                return null;
            }

            string libraryItemName = GetString(element, "libraryItemName");
            XElement? libraryRoot = package.FindLibraryRoot(libraryItemName, warnings);
            if (libraryRoot is null)
            {
                AddWarningOnce(warnings, $"{trackName}: frame {frameIndex} 找不到库项目 \"{libraryItemName}\"，该元素已留空。");
                return null;
            }

            int symbolFrame = Math.Max(0, (int)Math.Floor(GetFloat(element, "firstFrame", 0.0f)));
            return ResolveLibraryFrame(
                package,
                libraryRoot,
                symbolFrame,
                matrix,
                alpha,
                warnings,
                trackName,
                frameIndex,
                depth + 1);
        }

        return ResolveFirstRenderable(package, element, matrix, alpha, warnings, trackName, frameIndex, depth);
    }

    private static ResolvedRenderable? ResolveLibraryFrame(
        FlaPackage package,
        XElement libraryRoot,
        int symbolFrame,
        Matrix2D parentMatrix,
        float parentAlpha,
        List<string> warnings,
        string trackName,
        int frameIndex,
        int depth)
    {
        ResolvedRenderable? fallback = null;

        foreach (XElement timeline in FlaPackage.GetTimelineElements(libraryRoot))
        {
            XElement[] layers = timeline
                .ElementsLocal("layers")
                .ElementsLocal("DOMLayer")
                .Where(layer => !IsGuideOrFolderLayer(layer))
                .ToArray();

            foreach (XElement frame in layers.SelectMany(GetFrames))
            {
                int index = GetInt(frame, "index", 0);
                int duration = Math.Max(1, GetInt(frame, "duration", 1));

                XElement? elements = frame.ElementsLocal("elements").FirstOrDefault();
                if (elements is null)
                    continue;

                ResolvedRenderable? resolved = ResolveFirstRenderable(
                    package,
                    elements,
                    parentMatrix,
                    parentAlpha,
                    warnings,
                    trackName,
                    frameIndex,
                    depth);

                if (resolved is null)
                    continue;

                fallback ??= resolved;
                if (symbolFrame >= index && symbolFrame < index + duration)
                    return resolved;
            }
        }

        return fallback;
    }

    private static ResolvedRenderable? ResolveLibraryImage(
        FlaPackage package,
        XElement element,
        float parentAlpha,
        List<string> warnings,
        string trackName,
        int frameIndex,
        int depth)
    {
        float alpha = parentAlpha * GetAlpha(GetElementColor(element));

        if (element.Name.LocalName == "DOMBitmapInstance")
            return new ResolvedRenderable(element, Matrix2D.Identity, alpha);

        if (element.Name.LocalName is "DOMSymbolInstance" or "DOMGraphicInstance")
        {
            if (depth >= 16)
            {
                AddWarningOnce(warnings, $"{trackName}: frame {frameIndex} 的符号嵌套过深，已停止继续解析。");
                return null;
            }

            string libraryItemName = GetString(element, "libraryItemName");
            XElement? libraryRoot = package.FindLibraryRoot(libraryItemName, warnings);
            if (libraryRoot is null)
            {
                AddWarningOnce(warnings, $"{trackName}: frame {frameIndex} 找不到库项目 \"{libraryItemName}\"，该元素已留空。");
                return null;
            }

            int symbolFrame = Math.Max(0, (int)Math.Floor(GetFloat(element, "firstFrame", 0.0f)));
            return null;
        }

        return null;
    }

    private static ReanimTransform BuildTransform(
        FlaPackage package,
        ResolvedRenderable renderable,
        List<string> warnings,
        string trackName,
        int frameIndex)
    {
        Matrix2D matrix = renderable.Matrix;

        double scaleX = Math.Sqrt(matrix.A * matrix.A + matrix.B * matrix.B);
        double scaleY = Math.Sqrt(matrix.C * matrix.C + matrix.D * matrix.D);
        double skewX = scaleX > 0.000001 ? RadiansToDegrees(Math.Atan2(matrix.B, matrix.A)) : 0.0;
        double skewY = scaleY > 0.000001 ? -RadiansToDegrees(Math.Atan2(matrix.C, matrix.D)) : 0.0;

        string image = IsNonRenderingTrackName(trackName)
            ? ""
            : renderable.ImageOverride
                ?? ResolveImageResourceId(package, GetImageLibraryName(renderable.Element), warnings, trackName, frameIndex);
        float frame = GetFloat(renderable.Element, "firstFrame", 0.0f);

        return QuantizeTransform(new ReanimTransform(
            (float)matrix.Tx,
            (float)matrix.Ty,
            (float)skewX,
            (float)skewY,
            (float)scaleX,
            (float)scaleY,
            frame,
            renderable.Alpha,
            image,
            "",
            ""));
    }

    private static ReanimTransform BuildTweenTransform(
        ResolvedRenderable startRenderable,
        ResolvedRenderable endRenderable,
        ReanimTransform start,
        ReanimTransform end,
        float t)
    {
        if (!string.Equals(start.Image, end.Image, StringComparison.Ordinal))
        {
            return QuantizeTransform(start with
            {
                X = Lerp(start.X, end.X, t),
                Y = Lerp(start.Y, end.Y, t),
                Alpha = Lerp(start.Alpha, end.Alpha, t)
            });
        }

        TransformComponents startComponents = DecomposeMatrix(startRenderable.Matrix);
        TransformComponents endComponents = DecomposeMatrix(endRenderable.Matrix);
        Point2D startPoint = startRenderable.TransformationPoint ?? Point2D.Zero;
        Point2D endPoint = endRenderable.TransformationPoint ?? Point2D.Zero;
        Point2D startCenter = TransformPoint(startRenderable.Matrix, startPoint);
        Point2D endCenter = TransformPoint(endRenderable.Matrix, endPoint);

        double scaleX = Lerp(startComponents.ScaleX, endComponents.ScaleX, t);
        double scaleY = Lerp(startComponents.ScaleY, endComponents.ScaleY, t);
        double skewX = LerpAngle(startComponents.SkewX, endComponents.SkewX, t);
        double skewY = LerpAngle(startComponents.SkewY, endComponents.SkewY, t);
        Point2D point = Lerp(startPoint, endPoint, t);
        Point2D center = Lerp(startCenter, endCenter, t);

        double skewXRadians = DegreesToRadians(skewX);
        double skewYRadians = DegreesToRadians(skewY);
        double a = scaleX * Math.Cos(skewXRadians);
        double b = scaleX * Math.Sin(skewXRadians);
        double c = -scaleY * Math.Sin(skewYRadians);
        double d = scaleY * Math.Cos(skewYRadians);
        double x = center.X - (a * point.X + c * point.Y);
        double y = center.Y - (b * point.X + d * point.Y);

        return QuantizeTransform(start with
        {
            X = (float)x,
            Y = (float)y,
            Kx = (float)skewX,
            Ky = (float)skewY,
            Sx = (float)scaleX,
            Sy = (float)scaleY,
            Alpha = Lerp(start.Alpha, end.Alpha, t)
        });
    }

    private static ReanimTransform QuantizeTransform(ReanimTransform transform) => transform with
    {
        X = RoundTo(transform.X, 0.1f),
        Y = RoundTo(transform.Y, 0.1f),
        Kx = RoundTo(transform.Kx, 0.1f),
        Ky = RoundTo(transform.Ky, 0.1f),
        Sx = RoundTo(transform.Sx, 0.001f),
        Sy = RoundTo(transform.Sy, 0.001f),
        Alpha = RoundTo(transform.Alpha, 0.001f)
    };

    private static void NormalizeTrackAngles(ReanimTrack track)
    {
        float? previousKx = null;
        float? previousKy = null;

        for (int i = 0; i < track.Transforms.Count; i++)
        {
            ReanimTransform transform = track.Transforms[i];
            float kx = transform.Kx;
            float ky = transform.Ky;

            if (kx != ReanimTransform.MissingValue)
            {
                kx = previousKx.HasValue ? NormalizeAngleNear(kx, previousKx.Value) : kx;
                previousKx = kx;
            }

            if (ky != ReanimTransform.MissingValue)
            {
                ky = previousKy.HasValue ? NormalizeAngleNear(ky, previousKy.Value) : ky;
                previousKy = ky;
            }

            if (kx != transform.Kx || ky != transform.Ky)
                track.Transforms[i] = transform with { Kx = kx, Ky = ky };
        }
    }

    private static float NormalizeAngleNear(float angle, float previous)
    {
        while (angle > previous + 180.0f)
            angle -= 360.0f;
        while (angle < previous - 180.0f)
            angle += 360.0f;
        return angle;
    }

    private static float RoundTo(float value, float unit)
    {
        if (value == ReanimTransform.MissingValue)
            return value;

        float rounded = (float)(Math.Round(value / unit, MidpointRounding.AwayFromZero) * unit);
        return Math.Abs(rounded) < unit * 0.5f ? 0.0f : rounded;
    }

    private static TransformComponents DecomposeMatrix(Matrix2D matrix)
    {
        double scaleX = Math.Sqrt(matrix.A * matrix.A + matrix.B * matrix.B);
        double scaleY = Math.Sqrt(matrix.C * matrix.C + matrix.D * matrix.D);
        double skewX = scaleX > 0.000001 ? RadiansToDegrees(Math.Atan2(matrix.B, matrix.A)) : 0.0;
        double skewY = scaleY > 0.000001 ? -RadiansToDegrees(Math.Atan2(matrix.C, matrix.D)) : 0.0;
        return new TransformComponents(scaleX, scaleY, skewX, skewY);
    }

    private static Point2D TransformPoint(Matrix2D matrix, Point2D point) => new(
        matrix.A * point.X + matrix.C * point.Y + matrix.Tx,
        matrix.B * point.X + matrix.D * point.Y + matrix.Ty);

    private static Point2D Lerp(Point2D start, Point2D end, double t) => new(
        Lerp(start.X, end.X, t),
        Lerp(start.Y, end.Y, t));

    private static double Lerp(double start, double end, double t) => start + (end - start) * t;

    private static float Lerp(float start, float end, float t) => start + (end - start) * t;

    private static double LerpAngle(double start, double end, double t)
    {
        while (end > start + 180.0)
            end -= 360.0;
        while (end < start - 180.0)
            end += 360.0;
        return Lerp(start, end, t);
    }

    private static string GetImageLibraryName(XElement element)
    {
        string value = GetString(element, "libraryItemName");
        if (!string.IsNullOrWhiteSpace(value))
            return value;

        value = GetString(element, "href");
        if (!string.IsNullOrWhiteSpace(value))
            return value;

        value = GetString(element, "bitmapDataHRef");
        if (!string.IsNullOrWhiteSpace(value))
            return value;

        return GetString(element, "name");
    }

    private static XElement? GetElementColor(XElement element) =>
        element.ElementsLocal("color").ElementsLocal("Color").FirstOrDefault()
        ?? element.ElementsLocal("Color").FirstOrDefault();

    private static Point2D GetTransformationPoint(XElement element)
    {
        XElement? point = element.ElementsLocal("transformationPoint").ElementsLocal("Point").FirstOrDefault()
            ?? element.ElementsLocal("Point").FirstOrDefault();
        return new Point2D(
            GetDouble(point, "x", 0.0),
            GetDouble(point, "y", 0.0));
    }

    private static string ResolveImageResourceId(
        FlaPackage package,
        string libraryItemName,
        List<string> warnings,
        string trackName,
        int frameIndex)
    {
        ImageResolveResult result = package.ResourceCatalog?.ResolveImage(libraryItemName)
            ?? ImageResolveResult.Unknown(ToImageResourceId(libraryItemName));

        if (result.IsLoadable || package.ResourceCatalog is null)
            return result.ImageId;

        AddWarningOnce(
            warnings,
            $"{trackName}: frame {frameIndex} 的贴图 \"{libraryItemName}\" 未命中 resources.xml 或 Resources/reanim，已留空以保证 compiled 可读取。");

        return "";
    }

    private static float GetAlpha(XElement? color)
    {
        if (color is null)
            return 1.0f;

        float alpha = GetFloat(color, "alphaMultiplier", float.NaN);
        if (!float.IsNaN(alpha))
            return alpha;

        alpha = GetFloat(color, "alphaPercent", float.NaN);
        if (!float.IsNaN(alpha))
            return alpha / 100.0f;

        return 1.0f;
    }

    private static void FillRange(ReanimTrack track, int start, int end, ReanimTransform value)
    {
        for (int i = Math.Max(0, start); i < Math.Min(track.Transforms.Count, end); i++)
            track.Transforms[i] = value;
    }

    private static bool IsMotionTween(XElement frame)
    {
        string tweenType = GetString(frame, "tweenType");
        return tweenType.Equals("motion", StringComparison.OrdinalIgnoreCase);
    }

    private static bool IsGuideOrFolderLayer(XElement layer)
    {
        string layerType = GetString(layer, "layerType");
        return layerType.Equals("guide", StringComparison.OrdinalIgnoreCase)
            || layerType.Equals("folder", StringComparison.OrdinalIgnoreCase);
    }

    private static IEnumerable<XElement> GetFrames(XElement layer) =>
        layer.ElementsLocal("frames").ElementsLocal("DOMFrame");

    private static bool IsAnimationTrackName(string name) =>
        name.Trim().StartsWith("anim_", StringComparison.OrdinalIgnoreCase);

    private static bool IsOpeningTrackName(string name) =>
        name.Trim().Equals("anim_open", StringComparison.OrdinalIgnoreCase);

    private static bool IsNonRenderingTrackName(string name) =>
        name.TrimStart().StartsWith("_", StringComparison.Ordinal);

    private static bool IsLocatorLibraryItem(string libraryItemName)
    {
        if (string.IsNullOrWhiteSpace(libraryItemName))
            return false;

        string normalized = libraryItemName.Trim().Replace('\\', '/');
        return Path.GetFileNameWithoutExtension(normalized).Equals("locator", StringComparison.OrdinalIgnoreCase);
    }

    private static bool SameTrackName(string left, string right) =>
        string.Equals(left.Trim(), right.Trim(), StringComparison.OrdinalIgnoreCase);

    private static double RadiansToDegrees(double radians) => radians * 180.0 / Math.PI;

    private static double DegreesToRadians(double degrees) => degrees * Math.PI / 180.0;

    private static string ToImageResourceId(string libraryItemName)
    {
        if (string.IsNullOrWhiteSpace(libraryItemName))
            return "";

        string name = libraryItemName.Trim().Replace('\\', '/');
        int slash = name.LastIndexOf('/');
        if (slash >= 0)
            name = name[(slash + 1)..];

        name = Path.GetFileNameWithoutExtension(name);
        string resourceId = NormalizeResourceId(name);

        if (resourceId.StartsWith("IMAGE_REANIM_", StringComparison.OrdinalIgnoreCase))
            return resourceId;

        return "IMAGE_REANIM_" + resourceId;
    }

    internal static void AddWarningOnce(List<string> warnings, string warning)
    {
        if (!warnings.Contains(warning))
            warnings.Add(warning);
    }

    internal static string NormalizeResourceId(string value) =>
        ResourceIdRegex().Replace(value.ToUpperInvariant(), "_").Trim('_');

    internal static double GetDoubleValue(XElement? element, string name, double fallback) =>
        GetDouble(element, name, fallback);

    private static string GetString(XElement? element, string name, string fallback = "") =>
        element?.Attribute(name)?.Value ?? fallback;

    private static int GetInt(XElement? element, string name, int fallback)
    {
        string? value = element?.Attribute(name)?.Value;
        return int.TryParse(value, NumberStyles.Integer, CultureInfo.InvariantCulture, out int result)
            ? result
            : fallback;
    }

    private static float GetFloat(XElement? element, string name, float fallback)
    {
        string? value = element?.Attribute(name)?.Value;
        return float.TryParse(value, NumberStyles.Float, CultureInfo.InvariantCulture, out float result)
            ? result
            : fallback;
    }

    private static double GetDouble(XElement? element, string name, double fallback)
    {
        string? value = element?.Attribute(name)?.Value;
        return double.TryParse(value, NumberStyles.Float, CultureInfo.InvariantCulture, out double result)
            ? result
            : fallback;
    }

    [GeneratedRegex("[^A-Z0-9]+")]
    private static partial Regex ResourceIdRegex();
}

internal readonly record struct ResolvedRenderable(
    XElement Element,
    Matrix2D Matrix,
    float Alpha,
    string? ImageOverride = null,
    Point2D? TransformationPoint = null);

internal readonly record struct Point2D(double X, double Y)
{
    public static Point2D Zero { get; } = new(0.0, 0.0);
}

internal readonly record struct TransformComponents(double ScaleX, double ScaleY, double SkewX, double SkewY);

internal readonly record struct Matrix2D(double A, double B, double C, double D, double Tx, double Ty)
{
    public static Matrix2D Identity { get; } = new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);

    public static Matrix2D FromElement(XElement element)
    {
        XElement? matrix = element.ElementsLocal("matrix").ElementsLocal("Matrix").FirstOrDefault()
            ?? element.ElementsLocal("Matrix").FirstOrDefault();
        return new Matrix2D(
            FlaToReanimConverter.GetDoubleValue(matrix, "a", 1.0),
            FlaToReanimConverter.GetDoubleValue(matrix, "b", 0.0),
            FlaToReanimConverter.GetDoubleValue(matrix, "c", 0.0),
            FlaToReanimConverter.GetDoubleValue(matrix, "d", 1.0),
            FlaToReanimConverter.GetDoubleValue(matrix, "tx", 0.0),
            FlaToReanimConverter.GetDoubleValue(matrix, "ty", 0.0));
    }

    public static Matrix2D operator *(Matrix2D left, Matrix2D right) => new(
        left.A * right.A + left.C * right.B,
        left.B * right.A + left.D * right.B,
        left.A * right.C + left.C * right.D,
        left.B * right.C + left.D * right.D,
        left.A * right.Tx + left.C * right.Ty + left.Tx,
        left.B * right.Tx + left.D * right.Ty + left.Ty);
}

internal readonly record struct ImageResolveResult(string ImageId, bool IsLoadable)
{
    public static ImageResolveResult Unknown(string imageId) => new(imageId, false);
}

internal sealed class ResourceCatalog
{
    private static readonly string[] ImageExtensions = [".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp"];

    private readonly string _resourcesDirectory;
    private readonly HashSet<string> _imageIds = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, string> _pathToImageId = new(StringComparer.OrdinalIgnoreCase);

    private ResourceCatalog(string resourcesDirectory)
    {
        _resourcesDirectory = resourcesDirectory;
    }

    public static ResourceCatalog? TryLoadFor(string sourcePath)
    {
        DirectoryInfo? directory = new FileInfo(Path.GetFullPath(sourcePath)).Directory;
        while (directory is not null)
        {
            string resourcesXml = Path.Combine(directory.FullName, "Resources", "resources.xml");
            if (File.Exists(resourcesXml))
                return Load(resourcesXml);

            directory = directory.Parent;
        }

        return null;
    }

    public ImageResolveResult ResolveImage(string libraryItemName)
    {
        string candidate = MakeCandidateImageId(libraryItemName);
        if (string.IsNullOrWhiteSpace(candidate))
            return new ImageResolveResult("", true);

        if (_imageIds.Contains(candidate) || HasFallbackImageFile(candidate))
            return new ImageResolveResult(candidate, true);

        return new ImageResolveResult(candidate, false);
    }

    private static ResourceCatalog Load(string resourcesXml)
    {
        string resourcesDirectory = Path.GetDirectoryName(resourcesXml) ?? ".";
        var catalog = new ResourceCatalog(resourcesDirectory);
        XDocument document = XDocument.Load(resourcesXml, LoadOptions.None);

        string currentPath = "";
        string currentIdPrefix = "";
        foreach (XElement element in document.Descendants())
        {
            if (element.Name.LocalName == "SetDefaults")
            {
                string? path = element.Attribute("path")?.Value;
                if (path is not null)
                    currentPath = path.Trim().TrimEnd('/', '\\');

                string? idPrefix = element.Attribute("idprefix")?.Value;
                if (idPrefix is not null)
                    currentIdPrefix = idPrefix.Trim();

                continue;
            }

            if (element.Name.LocalName != "Image")
                continue;

            string pathValue = element.Attribute("path")?.Value ?? "";
            if (string.IsNullOrWhiteSpace(pathValue))
                continue;

            string? idValue = element.Attribute("id")?.Value;
            string imageId = currentIdPrefix + (string.IsNullOrWhiteSpace(idValue)
                ? Path.GetFileNameWithoutExtension(pathValue)
                : idValue.Trim());

            catalog._imageIds.Add(imageId);

            string resourcePath = string.IsNullOrWhiteSpace(currentPath)
                ? pathValue
                : currentPath + "/" + pathValue;

            catalog.AddPathLookup(resourcePath, imageId);
            catalog.AddPathLookup(pathValue, imageId);
        }

        return catalog;
    }

    private void AddPathLookup(string path, string imageId)
    {
        string normalized = NormalizePathKey(path);
        if (!string.IsNullOrWhiteSpace(normalized))
            _pathToImageId.TryAdd(normalized, imageId);

        string fileName = Path.GetFileName(normalized);
        if (!string.IsNullOrWhiteSpace(fileName))
            _pathToImageId.TryAdd(fileName, imageId);
    }

    private string MakeCandidateImageId(string libraryItemName)
    {
        if (string.IsNullOrWhiteSpace(libraryItemName))
            return "";

        string normalizedPath = NormalizePathKey(libraryItemName);
        if (_pathToImageId.TryGetValue(normalizedPath, out string? imageId))
            return imageId;

        string fileName = Path.GetFileName(normalizedPath);
        if (_pathToImageId.TryGetValue(fileName, out imageId))
            return imageId;

        string rawName = Path.GetFileNameWithoutExtension(libraryItemName.Trim().Replace('\\', '/'));
        string resourceId = FlaToReanimConverter.NormalizeResourceId(rawName);
        if (resourceId.StartsWith("IMAGE_", StringComparison.OrdinalIgnoreCase))
            return resourceId;

        return "IMAGE_REANIM_" + resourceId;
    }

    private bool HasFallbackImageFile(string imageId)
    {
        foreach (string relativePath in GetFallbackImagePaths(imageId))
        {
            string directory = Path.Combine(_resourcesDirectory, Path.GetDirectoryName(relativePath) ?? "");
            string fileName = Path.GetFileName(relativePath);
            if (!Directory.Exists(directory))
                continue;

            foreach (string extension in ImageExtensions)
            {
                string pattern = fileName + extension;
                if (Directory.EnumerateFiles(directory, pattern, SearchOption.TopDirectoryOnly).Any())
                    return true;
            }
        }

        return false;
    }

    private static IEnumerable<string> GetFallbackImagePaths(string imageId)
    {
        const string imagePrefix = "IMAGE_";
        const string reanimPrefix = "IMAGE_REANIM_";

        if (imageId.StartsWith(reanimPrefix, StringComparison.OrdinalIgnoreCase))
        {
            string name = imageId[reanimPrefix.Length..];
            yield return Path.Combine("reanim", name);
            yield return Path.Combine("images", name);
            yield break;
        }

        if (imageId.StartsWith(imagePrefix, StringComparison.OrdinalIgnoreCase))
        {
            string name = imageId[imagePrefix.Length..];
            yield return name;
            yield return Path.Combine("particles", name);
        }
    }

    private static string NormalizePathKey(string value)
    {
        string normalized = value.Trim().Replace('\\', '/');
        if (normalized.StartsWith("LIBRARY/", StringComparison.OrdinalIgnoreCase))
            normalized = normalized["LIBRARY/".Length..];

        normalized = Path.ChangeExtension(normalized, null);
        return normalized.ToUpperInvariant();
    }
}

internal sealed class FlaEntry
{
    private readonly byte[] _data;

    public FlaEntry(string fullName, byte[] data)
    {
        FullName = fullName.Replace('\\', '/');
        _data = data;
    }

    public string FullName { get; }

    public Stream Open() => new MemoryStream(_data, writable: false);
}

internal sealed class FlaPackage : IDisposable
{
    private readonly IReadOnlyList<FlaEntry> _entries;
    private readonly XDocument _document;
    private readonly Dictionary<string, XDocument> _libraryDocuments = new(StringComparer.OrdinalIgnoreCase);
    private bool _libraryDocumentsLoaded;

    private FlaPackage(IReadOnlyList<FlaEntry> entries, XDocument document, ResourceCatalog? resourceCatalog)
    {
        _entries = entries;
        _document = document;
        ResourceCatalog = resourceCatalog;
    }

    public ResourceCatalog? ResourceCatalog { get; }

    public static FlaPackage Open(string path)
    {
        List<FlaEntry> entries = ReadPackageEntries(path);

        FlaEntry? documentEntry = entries.FirstOrDefault(entry =>
            entry.FullName.Equals("DOMDocument.xml", StringComparison.OrdinalIgnoreCase)
            || entry.FullName.EndsWith("/DOMDocument.xml", StringComparison.OrdinalIgnoreCase));

        if (documentEntry is null)
            throw new InvalidOperationException("FLA 中没有 DOMDocument.xml。");

        using Stream stream = documentEntry.Open();
        XDocument document = XDocument.Load(stream, LoadOptions.None);
        return new FlaPackage(entries, document, ResourceCatalog.TryLoadFor(path));
    }

    public TimelineSource SelectTimeline(List<string> warnings)
    {
        var candidates = new List<TimelineSource>();

        foreach (XElement timeline in GetTimelineElements(_document.Root))
            candidates.Add(new TimelineSource(timeline, _document.Root!, "DOMDocument.xml", true));

        EnsureLibraryDocuments(warnings);
        foreach (IGrouping<XDocument, KeyValuePair<string, XDocument>> libraryGroup in _libraryDocuments.GroupBy(pair => pair.Value))
        {
            string sourceName = libraryGroup.First().Key;
            XDocument libraryDocument = libraryGroup.Key;
            foreach (XElement timeline in GetTimelineElements(libraryDocument.Root))
                candidates.Add(new TimelineSource(timeline, _document.Root!, sourceName, false));
        }

        if (candidates.Count == 0)
            throw new InvalidOperationException("FLA 中没有可用时间轴。");

        TimelineSource? documentTimeline = candidates.FirstOrDefault(candidate => candidate.IsDocumentTimeline && candidate.HasContent);
        if (documentTimeline is not null)
            return documentTimeline;

        TimelineSource selected = candidates.OrderByDescending(candidate => candidate.Score).First();
        if (!selected.IsDocumentTimeline)
            warnings.Add($"主时间轴为空，已使用库时间轴：{selected.SourceName}");

        return selected;
    }

    public void Dispose()
    {
    }

    public XElement? FindLibraryRoot(string libraryItemName, List<string> warnings)
    {
        EnsureLibraryDocuments(warnings);

        foreach (string key in GetLibraryLookupKeys(libraryItemName))
        {
            if (_libraryDocuments.TryGetValue(key, out XDocument? document))
                return document.Root;
        }

        return null;
    }

    public static IEnumerable<XElement> GetTimelineElements(XElement? root)
    {
        if (root is null)
            yield break;

        foreach (XElement timeline in root.ElementsLocal("timelines").ElementsLocal("DOMTimeline"))
            yield return timeline;

        foreach (XElement timeline in root.ElementsLocal("timeline").ElementsLocal("DOMTimeline"))
            yield return timeline;
    }

    private void EnsureLibraryDocuments(List<string> warnings)
    {
        if (_libraryDocumentsLoaded)
            return;

        foreach (FlaEntry entry in _entries.Where(IsLibraryXml))
        {
            try
            {
                using Stream stream = entry.Open();
                XDocument libraryDocument = XDocument.Load(stream, LoadOptions.None);
                AddLibraryDocument(entry.FullName, libraryDocument);
            }
            catch
            {
                FlaToReanimConverter.AddWarningOnce(warnings, $"无法读取库项目：{entry.FullName}");
            }
        }

        _libraryDocumentsLoaded = true;
    }

    private void AddLibraryDocument(string entryName, XDocument document)
    {
        foreach (string key in GetLibraryLookupKeys(entryName))
            _libraryDocuments.TryAdd(key, document);

        string rootName = document.Root?.Attribute("name")?.Value ?? "";
        foreach (string key in GetLibraryLookupKeys(rootName))
            _libraryDocuments.TryAdd(key, document);
    }

    private static List<FlaEntry> ReadPackageEntries(string path)
    {
        try
        {
            using ZipArchive archive = ZipFile.OpenRead(path);
            var entries = new List<FlaEntry>();
            foreach (ZipArchiveEntry entry in archive.Entries)
            {
                using Stream stream = entry.Open();
                using var buffer = new MemoryStream();
                stream.CopyTo(buffer);
                entries.Add(new FlaEntry(entry.FullName, buffer.ToArray()));
            }

            return entries;
        }
        catch (InvalidDataException)
        {
            return ReadLooseZipEntries(path);
        }
    }

    private static List<FlaEntry> ReadLooseZipEntries(string path)
    {
        byte[] packageBytes = File.ReadAllBytes(path);
        var entries = new List<FlaEntry>();
        int position = 0;

        while (position + 30 <= packageBytes.Length && ReadUInt32(packageBytes, position) == 0x04034B50)
        {
            ushort flags = ReadUInt16(packageBytes, position + 6);
            ushort method = ReadUInt16(packageBytes, position + 8);
            uint compressedSize = ReadUInt32(packageBytes, position + 18);
            uint uncompressedSize = ReadUInt32(packageBytes, position + 22);
            ushort fileNameLength = ReadUInt16(packageBytes, position + 26);
            ushort extraLength = ReadUInt16(packageBytes, position + 28);

            if ((flags & 0x0008) != 0)
                throw new InvalidOperationException("该 FLA 使用 data descriptor ZIP 条目，暂不支持直接读取。");

            int nameStart = position + 30;
            int dataStart = nameStart + fileNameLength + extraLength;
            int dataEnd = checked(dataStart + (int)compressedSize);
            if (dataEnd > packageBytes.Length)
                throw new InvalidOperationException("FLA ZIP 条目长度异常，无法继续读取。");

            string name = Encoding.UTF8.GetString(packageBytes, nameStart, fileNameLength);
            byte[] payload = packageBytes[dataStart..dataEnd];
            byte[] data = method switch
            {
                0 => payload,
                8 => InflateRawDeflate(payload, checked((int)uncompressedSize)),
                _ => throw new InvalidOperationException($"FLA ZIP 条目使用了暂不支持的压缩方式：{method}。")
            };

            entries.Add(new FlaEntry(name, data));
            position = dataEnd;
        }

        if (entries.Count == 0)
            throw new InvalidOperationException("该 FLA 不是 ZIP/XFL 格式，暂不支持旧版二进制 FLA。");

        return entries;
    }

    private static byte[] InflateRawDeflate(byte[] payload, int expectedSize)
    {
        using var input = new MemoryStream(payload);
        using var deflate = new DeflateStream(input, CompressionMode.Decompress);
        using var output = expectedSize > 0 ? new MemoryStream(expectedSize) : new MemoryStream();
        deflate.CopyTo(output);
        return output.ToArray();
    }

    private static ushort ReadUInt16(byte[] buffer, int offset) =>
        (ushort)(buffer[offset] | (buffer[offset + 1] << 8));

    private static uint ReadUInt32(byte[] buffer, int offset) =>
        (uint)(buffer[offset] | (buffer[offset + 1] << 8) | (buffer[offset + 2] << 16) | (buffer[offset + 3] << 24));

    private static bool IsLibraryXml(FlaEntry entry)
    {
        string name = entry.FullName.Replace('\\', '/');
        return name.StartsWith("LIBRARY/", StringComparison.OrdinalIgnoreCase)
            && name.EndsWith(".xml", StringComparison.OrdinalIgnoreCase);
    }

    private static IEnumerable<string> GetLibraryLookupKeys(string value)
    {
        if (string.IsNullOrWhiteSpace(value))
            yield break;

        string key = value.Trim().Replace('\\', '/');
        if (key.StartsWith("LIBRARY/", StringComparison.OrdinalIgnoreCase))
            key = key["LIBRARY/".Length..];

        if (key.EndsWith(".xml", StringComparison.OrdinalIgnoreCase))
            key = key[..^4];

        if (!string.IsNullOrWhiteSpace(key))
            yield return key;

        string fileName = Path.GetFileName(key);
        if (!string.IsNullOrWhiteSpace(fileName) && !fileName.Equals(key, StringComparison.OrdinalIgnoreCase))
            yield return fileName;
    }
}

internal sealed class TimelineSource
{
    public TimelineSource(XElement timeline, XElement documentRoot, string sourceName, bool isDocumentTimeline)
    {
        Timeline = timeline;
        DocumentRoot = documentRoot;
        SourceName = sourceName;
        IsDocumentTimeline = isDocumentTimeline;
        (Score, HasContent) = CalculateScore(timeline);
    }

    public XElement Timeline { get; }
    public XElement DocumentRoot { get; }
    public string SourceName { get; }
    public bool IsDocumentTimeline { get; }
    public int Score { get; }
    public bool HasContent { get; }

    private static (int Score, bool HasContent) CalculateScore(XElement timeline)
    {
        XElement[] frames = timeline
            .DescendantsLocal("DOMFrame")
            .ToArray();

        int renderables = frames.Count(frame => frame.Descendants().Any(element =>
            element.Name.LocalName is "DOMSymbolInstance" or "DOMBitmapInstance" or "DOMGraphicInstance"));
        int labels = frames.Count(frame =>
            (frame.Attribute("name")?.Value ?? "").StartsWith("anim_", StringComparison.OrdinalIgnoreCase));

        return (renderables * 100 + labels * 20 + frames.Length, renderables > 0 || labels > 0);
    }
}

internal static class XflXmlExtensions
{
    public static IEnumerable<XElement> ElementsLocal(this XContainer? container, string localName)
    {
        if (container is null)
            return Enumerable.Empty<XElement>();

        return container.Elements().Where(element => element.Name.LocalName == localName);
    }

    public static IEnumerable<XElement> ElementsLocal(this IEnumerable<XElement> elements, string localName) =>
        elements.SelectMany(element => element.ElementsLocal(localName));

    public static IEnumerable<XElement> DescendantsLocal(this XContainer? container, string localName)
    {
        if (container is null)
            return Enumerable.Empty<XElement>();

        return container.Descendants().Where(element => element.Name.LocalName == localName);
    }
}

internal sealed class ReanimDefinition
{
    public ReanimDefinition(float fps, int frameCount)
    {
        Fps = fps;
        FrameCount = frameCount;
    }

    public float Fps { get; }
    public int FrameCount { get; }
    public List<ReanimTrack> Tracks { get; } = new();
}

internal sealed class ReanimTrack
{
    public ReanimTrack(string name, int frameCount)
    {
        Name = name;
        Transforms = Enumerable.Repeat(ReanimTransform.Blank(), frameCount).ToList();
    }

    public string Name { get; }
    public List<ReanimTransform> Transforms { get; }
}

internal readonly record struct ReanimTransform(
    float X,
    float Y,
    float Kx,
    float Ky,
    float Sx,
    float Sy,
    float Frame,
    float Alpha,
    string Image,
    string Font,
    string Text)
{
    public const float MissingValue = -10000.0f;

    public static ReanimTransform Blank() => new(
        MissingValue,
        MissingValue,
        MissingValue,
        MissingValue,
        MissingValue,
        MissingValue,
        -1.0f,
        MissingValue,
        "",
        "",
        "");

    public static ReanimTransform Marker() => new(
        MissingValue,
        MissingValue,
        MissingValue,
        MissingValue,
        MissingValue,
        MissingValue,
        0.0f,
        MissingValue,
        "",
        "",
        "");

    public static ReanimTransform Lerp(ReanimTransform a, ReanimTransform b, float t) => a with
    {
        X = Lerp(a.X, b.X, t),
        Y = Lerp(a.Y, b.Y, t),
        Kx = LerpAngle(a.Kx, b.Kx, t),
        Ky = LerpAngle(a.Ky, b.Ky, t),
        Sx = Lerp(a.Sx, b.Sx, t),
        Sy = Lerp(a.Sy, b.Sy, t),
        Alpha = Lerp(a.Alpha, b.Alpha, t)
    };

    private static float Lerp(float a, float b, float t) => a + (b - a) * t;

    private static float LerpAngle(float a, float b, float t)
    {
        while (b > a + 180.0f)
            b -= 360.0f;
        while (b < a - 180.0f)
            b += 360.0f;
        return Lerp(a, b, t);
    }
}
