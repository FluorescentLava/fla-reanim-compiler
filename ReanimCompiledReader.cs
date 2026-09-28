using System.Globalization;
using System.IO;

namespace FlaReanimCompiler;

/// <summary>
/// A parsed <c>.reanim.compiled</c> resource plus the format details that were
/// observed while reading it.
/// </summary>
internal sealed class ReanimCompiledDocument
{
    public required string SourcePath { get; init; }
    public required uint SchemaHash { get; init; }
    public required int TrackStructSize { get; init; }
    public required int TransformStructSize { get; init; }
    public required ReanimDefinition Definition { get; init; }

    /// <summary>Bytes of the decompressed buffer consumed by the reader.</summary>
    public required int BytesConsumed { get; init; }

    /// <summary>Declared uncompressed size from the file header.</summary>
    public required int DeclaredUncompressedSize { get; init; }
}

/// <summary>
/// Reads the canonical packed <c>ReanimatorDefinition</c> from a
/// <c>.reanim.compiled</c> file.
///
/// The reader reproduces the engine's own load path
/// (<c>DefinitionReadCompiledFile</c> → <c>DefReadFromCacheArray</c> →
/// <c>DefMapReadFromCache</c>) exactly: fixed structs first, then the variable
/// length fields in DefMap field order. Values are kept <b>raw</b>, including the
/// <c>-10000</c> "unchanged" placeholder, so re-serialising reproduces the original
/// bytes.
/// </summary>
internal static class ReanimCompiledReader
{
    /// <summary>Mirror of DefReadFromCacheString's sanity bound.</summary>
    private const int StringLengthLimit = 100000;

    public static ReanimCompiledDocument Read(string path)
    {
        string fullPath = Path.GetFullPath(path);
        byte[] payload = CompiledDefinitionFormat.ReadDecompressed(fullPath, out uint declaredSize);
        var cursor = new Cursor(payload, fullPath);

        uint schemaHash = cursor.ReadUInt32();
        if (schemaHash != CompiledDefinitionFormat.ReanimSchemaHash)
        {
            throw new InvalidDataException(
                $"{Path.GetFileName(fullPath)} 的 schema 为 0x{schemaHash:X8}，" +
                $"不是 reanim 定义（期望 0x{CompiledDefinitionFormat.ReanimSchemaHash:X8}）。" +
                "该文件可能属于粒子等其它 DefMap 资源。");
        }

        // ReanimatorDefinition: mTracks(8) + mFPS(4) + mReanimAtlas ptr slot(4) = 16 bytes.
        cursor.Skip(4);                                   // mTracks data pointer slot
        int trackCount = cursor.ReadInt32();              // mTracks.mArrayCount
        float fps = cursor.ReadSingle();                  // mFPS
        cursor.Skip(4);                                   // mReanimAtlas pointer slot

        if (trackCount < 0 || trackCount > 100000)
            throw new InvalidDataException($"轨道数量异常：{trackCount}。");

        // DefWriteToCacheArray(track): [struct size][N fixed structs][N variable parts]
        int trackStructSize = cursor.ReadInt32();
        if (trackStructSize != CompiledDefinitionFormat.TrackStructSize)
        {
            throw new InvalidDataException(
                $"轨道结构大小 {trackStructSize} 与预期的 " +
                $"{CompiledDefinitionFormat.TrackStructSize} 不符，格式可能已变更。");
        }

        int transformStructSize = 0;
        var transformCounts = new int[trackCount];
        for (int i = 0; i < trackCount; i++)
        {
            cursor.Skip(8);                               // mName / mTransforms pointer slots
            transformCounts[i] = cursor.ReadInt32();      // mTransformCount
            if (transformCounts[i] < 0 || transformCounts[i] > 1000000)
                throw new InvalidDataException($"轨道 {i} 的帧数量异常：{transformCounts[i]}。");
        }

        int frameCount = transformCounts.Length > 0 ? transformCounts[0] : 0;
        var definition = new ReanimDefinition(fps, frameCount);

        for (int trackIndex = 0; trackIndex < trackCount; trackIndex++)
        {
            int count = transformCounts[trackIndex];

            if (trackIndex > 0 && count != frameCount)
            {
                throw new InvalidDataException(
                    $"轨道 \"{trackIndex}\" 的帧数量 {count} 与首轨道的 {frameCount} 不一致，" +
                    "游戏侧要求所有轨道帧数相同。");
            }

            string name = cursor.ReadString();

            int thisTransformStructSize = cursor.ReadInt32();
            if (thisTransformStructSize != CompiledDefinitionFormat.TransformStructSize)
            {
                throw new InvalidDataException(
                    $"轨道 \"{name}\" 的变换结构大小 {thisTransformStructSize} 与预期的 " +
                    $"{CompiledDefinitionFormat.TransformStructSize} 不符。");
            }
            transformStructSize = thisTransformStructSize;

            var transforms = new ReanimTransform[count];
            for (int frame = 0; frame < count; frame++)
            {
                float x = cursor.ReadSingle();
                float y = cursor.ReadSingle();
                float kx = cursor.ReadSingle();
                float ky = cursor.ReadSingle();
                float sx = cursor.ReadSingle();
                float sy = cursor.ReadSingle();
                float f = cursor.ReadSingle();
                float alpha = cursor.ReadSingle();
                cursor.Skip(12);                          // mImage / mFont / mText pointer slots
                transforms[frame] = new ReanimTransform(x, y, kx, ky, sx, sy, f, alpha, "", "", "");
            }

            // Variable length fields. DefWriteToCacheArray writes the fixed structs for
            // the whole array first, then calls DefMapWriteToCache once per element, and
            // that walks the DefMap field order (i, font, text) for a single element.
            // The grouping is therefore per element, not per field.
            for (int frame = 0; frame < count; frame++)
            {
                string image = cursor.ReadString();
                string font = cursor.ReadString();
                string text = cursor.ReadString();
                transforms[frame] = transforms[frame] with { Image = image, Font = font, Text = text };
            }

            var track = new ReanimTrack(name, count);
            for (int frame = 0; frame < count; frame++)
                track.Transforms[frame] = transforms[frame];

            definition.Tracks.Add(track);
        }

        return new ReanimCompiledDocument
        {
            SourcePath = fullPath,
            SchemaHash = schemaHash,
            TrackStructSize = trackStructSize,
            TransformStructSize = transformStructSize,
            Definition = definition,
            BytesConsumed = cursor.Position,
            DeclaredUncompressedSize = (int)declaredSize
        };
    }

    /// <summary>Walks a decompressed compiled buffer with bounds checking.</summary>
    private sealed class Cursor
    {
        private readonly byte[] _buffer;
        private readonly string _path;
        private int _position;

        public Cursor(byte[] buffer, string path)
        {
            _buffer = buffer;
            _path = path;
        }

        public int Position => _position;

        public void Skip(int count)
        {
            Ensure(count);
            _position += count;
        }

        public int ReadInt32()
        {
            Ensure(4);
            int value = BitConverter.ToInt32(_buffer, _position);
            _position += 4;
            return value;
        }

        public uint ReadUInt32()
        {
            Ensure(4);
            uint value = BitConverter.ToUInt32(_buffer, _position);
            _position += 4;
            return value;
        }

        public float ReadSingle()
        {
            Ensure(4);
            float value = BitConverter.ToSingle(_buffer, _position);
            _position += 4;
            return value;
        }

        /// <summary>DefReadFromCacheString / Image / Font: int length then raw bytes.</summary>
        public string ReadString()
        {
            int length = ReadInt32();
            if (length < 0 || length > StringLengthLimit)
            {
                throw new InvalidDataException(
                    $"{Path.GetFileName(_path)} 在偏移 {_position - 4} 处出现非法字符串长度 {length}，" +
                    "文件结构可能已损坏或格式不同。");
            }

            if (length == 0)
                return "";

            Ensure(length);
            string value = System.Text.Encoding.UTF8.GetString(_buffer, _position, length);
            _position += length;
            return value;
        }

        private void Ensure(int count)
        {
            if (count < 0 || _position + count > _buffer.Length)
            {
                throw new InvalidDataException(
                    $"{Path.GetFileName(_path)} 数据在偏移 {_position} 处越界" +
                    $"（需要 {count} 字节，剩余 {_buffer.Length - _position} 字节）。");
            }
        }
    }
}

/// <summary>
/// Helpers mirroring the engine's runtime treatment of packed transform data.
/// </summary>
internal static class ReanimTransformSemantics
{
    public const float MissingValue = -10000.0f;

    /// <summary>
    /// Mirror of <c>ReanimationFillInMissingData</c> (Reanimator.cpp): a field equal to
    /// the placeholder inherits the previous frame's value, starting from the
    /// engine's constructor defaults (x=0, y=0, skew=0, scale=1, frame=0, alpha=1).
    /// </summary>
    public static ReanimTransform[] FillForward(IReadOnlyList<ReanimTransform> source)
    {
        var result = new ReanimTransform[source.Count];

        float prevX = 0.0f, prevY = 0.0f, prevKx = 0.0f, prevKy = 0.0f;
        float prevSx = 1.0f, prevSy = 1.0f, prevFrame = 0.0f, prevAlpha = 1.0f;
        string prevImage = "", prevFont = "", prevText = "";

        for (int i = 0; i < source.Count; i++)
        {
            ReanimTransform t = source[i];

            float x = Fill(t.X, ref prevX);
            float y = Fill(t.Y, ref prevY);
            float kx = Fill(t.Kx, ref prevKx);
            float ky = Fill(t.Ky, ref prevKy);
            float sx = Fill(t.Sx, ref prevSx);
            float sy = Fill(t.Sy, ref prevSy);
            float frame = Fill(t.Frame, ref prevFrame);
            float alpha = Fill(t.Alpha, ref prevAlpha);

            string image = string.IsNullOrEmpty(t.Image) ? prevImage : (prevImage = t.Image);
            string font = string.IsNullOrEmpty(t.Font) ? prevFont : (prevFont = t.Font);
            string text = string.IsNullOrEmpty(t.Text) ? prevText : (prevText = t.Text);

            result[i] = new ReanimTransform(x, y, kx, ky, sx, sy, frame, alpha, image, font, text);
        }

        return result;
    }

    private static float Fill(float value, ref float previous)
    {
        if (value == MissingValue)
            return previous;

        previous = value;
        return value;
    }

    /// <summary>True when the frame draws nothing (DrawTrack bails out for mFrame &lt; 0).</summary>
    public static bool IsBlank(ReanimTransform t) => t.Frame < 0.0f;

    /// <summary>
    /// True when any packed float field was explicitly written, i.e. the track carries a
    /// transform rather than being a pure marker.
    /// </summary>
    public static bool HasExplicitTransform(ReanimTransform t) =>
        t.X != MissingValue || t.Y != MissingValue ||
        t.Kx != MissingValue || t.Ky != MissingValue ||
        t.Sx != MissingValue || t.Sy != MissingValue ||
        t.Alpha != MissingValue;

    /// <summary>True when any frame carries an image reference.</summary>
    public static bool TrackHasImage(ReanimTrack track) =>
        track.Transforms.Any(t => !string.IsNullOrEmpty(t.Image));

    /// <summary>
    /// True when the track carries attacher commands or credit text. Those live in the
    /// text/font fields, which a FLA element cannot represent.
    /// </summary>
    public static bool TrackHasTextData(ReanimTrack track) =>
        track.Transforms.Any(t => !string.IsNullOrEmpty(t.Text) || !string.IsNullOrEmpty(t.Font));

    public static bool TrackHasExplicitTransform(ReanimTrack track) =>
        track.Transforms.Any(HasExplicitTransform);

    /// <summary>
    /// True when the track never becomes drawable: after fill-forward no frame reaches
    /// mFrame &gt;= 0, and it carries no image, text or transform. DrawTrack returns false
    /// for every frame of such a track, and the forward converter emits nothing for a
    /// layer with no elements, so an inert track cannot survive a FLA round trip.
    /// </summary>
    public static bool IsInert(ReanimTrack track)
    {
        if (TrackHasImage(track) || TrackHasTextData(track) || TrackHasExplicitTransform(track))
            return false;

        return !FillForward(track.Transforms).Any(t => t.Frame >= 0.0f);
    }

    /// <summary>
    /// True when a FLA cannot carry this track at all, so it is expected to be absent
    /// from a reverse conversion and preserved in the .reanim.xml sidecar instead.
    /// </summary>
    public static bool IsFlaUnrepresentable(ReanimTrack track) =>
        TrackHasTextData(track) || IsInert(track);

    /// <summary>
    /// True when every field still carries a placeholder, i.e. the frame was never
    /// written. The engine only produces this shape for hand-authored definitions
    /// (for example the <c>fullscreen</c> track), never via the forward converter.
    /// </summary>
    public static bool IsAllMissing(ReanimTransform t) =>
        t.X == MissingValue && t.Y == MissingValue &&
        t.Kx == MissingValue && t.Ky == MissingValue &&
        t.Sx == MissingValue && t.Sy == MissingValue &&
        t.Frame == MissingValue && t.Alpha == MissingValue;

    public static string FormatFloat(float value) =>
        value.ToString("R", CultureInfo.InvariantCulture);
}
