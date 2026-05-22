using System.IO;
using System.IO.Compression;
using System.Text;

namespace FlaReanimCompiler;

internal static class ReanimCompiledWriter
{
    private const uint Cookie = 0xDEADFED4;
    private const uint ReanimSchemaHash = 0xB393B4C0;
    private const int ReanimatorDefinitionCompiledSize = 16;
    private const int ReanimatorTrackCompiledSize = 12;
    private const int ReanimatorTransformCompiledSize = 44;

    public static void Write(string outputPath, ReanimDefinition definition)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(outputPath) ?? ".");

        byte[] uncompressed = BuildUncompressedBuffer(definition);
        byte[] compressed = Compress(uncompressed);

        using var stream = File.Create(outputPath);
        using var writer = new BinaryWriter(stream, Encoding.UTF8, leaveOpen: false);
        writer.Write(Cookie);
        writer.Write((uint)uncompressed.Length);
        writer.Write(compressed);
    }

    private static byte[] BuildUncompressedBuffer(ReanimDefinition definition)
    {
        using var stream = new MemoryStream();
        using var writer = new BinaryWriter(stream, Encoding.UTF8, leaveOpen: true);

        writer.Write(ReanimSchemaHash);
        WriteDefinitionStruct(writer, definition);
        WriteTrackArray(writer, definition.Tracks);

        writer.Flush();
        return stream.ToArray();
    }

    private static void WriteDefinitionStruct(BinaryWriter writer, ReanimDefinition definition)
    {
        writer.Write(0);                       // mTracks pointer slot, ignored by canonical reader.
        writer.Write(definition.Tracks.Count); // mTrackCount.
        writer.Write(definition.Fps);          // mFPS.
        writer.Write(0);                       // mReanimAtlas pointer slot.
    }

    private static void WriteTrackArray(BinaryWriter writer, IReadOnlyList<ReanimTrack> tracks)
    {
        writer.Write(ReanimatorTrackCompiledSize);
        foreach (ReanimTrack track in tracks)
            WriteTrackStruct(writer, track);

        foreach (ReanimTrack track in tracks)
        {
            WriteString(writer, track.Name);
            WriteTransformArray(writer, track.Transforms);
        }
    }

    private static void WriteTrackStruct(BinaryWriter writer, ReanimTrack track)
    {
        writer.Write(0);                       // mName pointer slot.
        writer.Write(0);                       // mTransforms pointer slot.
        writer.Write(track.Transforms.Count);  // mTransformCount.
    }

    private static void WriteTransformArray(BinaryWriter writer, IReadOnlyList<ReanimTransform> transforms)
    {
        List<ReanimTransform> compactTransforms = CompactTransforms(transforms);

        writer.Write(ReanimatorTransformCompiledSize);
        foreach (ReanimTransform transform in compactTransforms)
            WriteTransformStruct(writer, transform);

        foreach (ReanimTransform transform in compactTransforms)
        {
            WriteString(writer, transform.Image);
            WriteString(writer, transform.Font);
            WriteString(writer, transform.Text);
        }
    }

    private static void WriteTransformStruct(BinaryWriter writer, ReanimTransform transform)
    {
        writer.Write(transform.X);
        writer.Write(transform.Y);
        writer.Write(transform.Kx);
        writer.Write(transform.Ky);
        writer.Write(transform.Sx);
        writer.Write(transform.Sy);
        writer.Write(transform.Frame);
        writer.Write(transform.Alpha);
        writer.Write(0); // mImage pointer slot.
        writer.Write(0); // mFont pointer slot.
        writer.Write(0); // mText pointer slot.
    }

    private static List<ReanimTransform> CompactTransforms(IReadOnlyList<ReanimTransform> transforms)
    {
        var compactTransforms = new List<ReanimTransform>(transforms.Count);

        float previousX = 0.0f;
        float previousY = 0.0f;
        float previousKx = 0.0f;
        float previousKy = 0.0f;
        float previousSx = 1.0f;
        float previousSy = 1.0f;
        float previousFrame = 0.0f;
        float previousAlpha = 1.0f;
        string previousImage = "";
        string previousFont = "";
        string previousText = "";

        foreach (ReanimTransform transform in transforms)
        {
            float x = CompactFloat(transform.X, ref previousX);
            float y = CompactFloat(transform.Y, ref previousY);
            float kx = CompactFloat(transform.Kx, ref previousKx);
            float ky = CompactFloat(transform.Ky, ref previousKy);
            float sx = CompactFloat(transform.Sx, ref previousSx);
            float sy = CompactFloat(transform.Sy, ref previousSy);
            float frame = CompactFloat(transform.Frame, ref previousFrame);
            float alpha = CompactFloat(transform.Alpha, ref previousAlpha);
            string image = CompactString(transform.Image, ref previousImage);
            string font = CompactString(transform.Font, ref previousFont);
            string text = CompactString(transform.Text, ref previousText);

            compactTransforms.Add(new ReanimTransform(x, y, kx, ky, sx, sy, frame, alpha, image, font, text));
        }

        return compactTransforms;
    }

    private static float CompactFloat(float value, ref float previous)
    {
        if (value == ReanimTransform.MissingValue || value.Equals(previous))
            return ReanimTransform.MissingValue;

        previous = value;
        return value;
    }

    private static string CompactString(string value, ref string previous)
    {
        if (string.IsNullOrEmpty(value) || string.Equals(value, previous, StringComparison.Ordinal))
            return "";

        previous = value;
        return value;
    }

    private static void WriteString(BinaryWriter writer, string value)
    {
        byte[] bytes = Encoding.UTF8.GetBytes(value);
        writer.Write(bytes.Length);
        writer.Write(bytes);
    }

    private static byte[] Compress(byte[] data)
    {
        using var output = new MemoryStream();
        using (var zlib = new ZLibStream(output, CompressionLevel.SmallestSize, leaveOpen: true))
            zlib.Write(data, 0, data.Length);

        return output.ToArray();
    }
}
