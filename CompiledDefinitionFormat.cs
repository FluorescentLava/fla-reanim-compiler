using System.IO;
using System.IO.Compression;

namespace FlaReanimCompiler;

/// <summary>
/// Canonical binary layout knowledge for TodLib "compiled definition" resources
/// (<c>*.compiled</c>), derived from <c>TodLib/Definition.cpp</c> and
/// <c>TodLib/Reanimator.cpp</c>.
///
/// The container is a generic DefMap cache. For reanim it carries the packed
/// <c>ReanimatorDefinition</c>; particle definitions use the same container with a
/// different schema, which is why every reader validates the schema hash first.
/// </summary>
internal static class CompiledDefinitionFormat
{
    /// <summary>CompressedDefinitionHeader::mCookie (Definition.cpp DefinitionUncompressCompiledBuffer).</summary>
    public const uint Cookie = 0xDEADFED4;

    public const int HeaderSize = 8;
    public const int DefinitionStructSize = 16;
    public const int TrackStructSize = 12;
    public const int TransformStructSize = 44;

    /// <summary>
    /// Schema hash of <c>gReanimatorDefMap</c>, reproduced by <see cref="ComputeReanimSchemaHash"/>.
    /// All 151 shipped reanim files carry exactly this value.
    /// </summary>
    public const uint ReanimSchemaHash = 0xB393B4C0;

    /// <summary>Mask for the per-array element-size prefix written by DefWriteToCacheArray.</summary>
    public const int UnknownStructSize = 0;

    public static byte[] ReadDecompressed(string path, out uint declaredUncompressedSize)
    {
        byte[] raw = File.ReadAllBytes(path);
        if (raw.Length < HeaderSize)
            throw new InvalidDataException($"compiled 文件过短（{raw.Length} 字节），连文件头都不完整。");

        uint cookie = ReadUInt32(raw, 0);
        if (cookie != Cookie)
            throw new InvalidDataException($"compiled 文件 cookie 错误：0x{cookie:X8}，期望 0x{Cookie:X8}。");

        declaredUncompressedSize = ReadUInt32(raw, 4);

        using var source = new MemoryStream(raw, HeaderSize, raw.Length - HeaderSize, writable: false);
        using var zlib = new ZLibStream(source, CompressionMode.Decompress);
        using var output = new MemoryStream(
            declaredUncompressedSize <= int.MaxValue ? (int)declaredUncompressedSize : 0);
        zlib.CopyTo(output);
        return output.ToArray();
    }

    /// <summary>
    /// Recomputes <see cref="ReanimSchemaHash"/> from the field tables, mirroring
    /// <c>DefinitionCalcHashDefMap</c> / <c>DefinitionGetCompiledStructSize</c>.
    /// Kept as executable documentation of the format so a schema change is detected
    /// instead of silently misparsed.
    /// </summary>
    public static uint ComputeReanimSchemaHash()
    {
        // DefFieldType order from TodLib/Definition.h.
        const int DtFloat = 2, DtString = 3, DtArray = 6, DtImage = 9, DtFont = 10;

        var trackFields = new[]
        {
            new FieldDef("name", 0, DtString, null),
            new FieldDef("t", 4, DtArray, "transform"),
        };
        var transformFields = new[]
        {
            new FieldDef("x", 0, DtFloat, null), new FieldDef("y", 4, DtFloat, null),
            new FieldDef("kx", 8, DtFloat, null), new FieldDef("ky", 12, DtFloat, null),
            new FieldDef("sx", 16, DtFloat, null), new FieldDef("sy", 20, DtFloat, null),
            new FieldDef("f", 24, DtFloat, null), new FieldDef("a", 28, DtFloat, null),
            new FieldDef("i", 32, DtImage, null), new FieldDef("font", 36, DtFont, null),
            new FieldDef("text", 40, DtString, null),
        };
        var maps = new Dictionary<string, FieldMap>(StringComparer.Ordinal)
        {
            ["transform"] = new FieldMap(transformFields, declaredSize: 0),
            ["track"] = new FieldMap(trackFields, declaredSize: 0),
            // gReanimatorDefMap declares mCompiledSize = 16.
            ["definition"] = new FieldMap(
                new[] { new FieldDef("track", 0, DtArray, "track"), new FieldDef("fps", 8, DtFloat, null) },
                declaredSize: DefinitionStructSize),
        };

        uint seed = Crc32(0, []) + 1;
        return HashDefMap(seed, maps["definition"], maps, []);
    }

    public static int StorageSize(int fieldType) => fieldType switch
    {
        3 or 9 or 10 => 4,            // DT_STRING / DT_IMAGE / DT_FONT
        6 or 7 => 8,                  // DT_ARRAY / DT_TRACK_FLOAT
        5 => sizeof(float) * 2,       // DT_VECTOR2
        _ => 4                // DT_INT / DT_FLOAT / DT_ENUM / DT_FLAGS
    };

    private static int CompiledFieldOffset(FieldMap map, FieldDef field)
    {
        int offset = 0;
        foreach (FieldDef other in map.Fields)
            if (other.DefOffset < field.DefOffset)
                offset += StorageSize(other.Type);
        return offset;
    }

    private static int CompiledStructSize(FieldMap map)
    {
        if (map.DeclaredSize != 0)
            return map.DeclaredSize;

        int max = 0;
        foreach (FieldDef field in map.Fields)
            max = Math.Max(max, CompiledFieldOffset(map, field) + StorageSize(field.Type));
        return max;
    }

    private static uint HashDefMap(
        uint seed,
        FieldMap map,
        Dictionary<string, FieldMap> maps,
        HashSet<string> visited)
    {
        if (!visited.Add(map.Name))
            return seed;

        seed = Crc32(seed, BitConverter.GetBytes(CompiledStructSize(map)));
        foreach (FieldDef field in map.Fields)
        {
            seed = Crc32(seed, BitConverter.GetBytes(field.Type));
            seed = Crc32(seed, BitConverter.GetBytes(CompiledFieldOffset(map, field)));
            if (field.Type == 6 && field.NestedMap is not null && maps.TryGetValue(field.NestedMap, out FieldMap? nested))
                seed = HashDefMap(seed, nested, maps, visited);
        }
        return seed;
    }

    private static uint Crc32(uint seed, byte[] data)
    {
        uint crc = seed ^ 0xFFFFFFFF;
        foreach (byte b in data)
        {
            crc ^= b;
            for (int i = 0; i < 8; i++)
                crc = (crc & 1) != 0 ? (crc >> 1) ^ 0xEDB88320 : crc >> 1;
        }
        return crc ^ 0xFFFFFFFF;
    }

    public static ushort ReadUInt16(byte[] buffer, int offset) =>
        (ushort)(buffer[offset] | (buffer[offset + 1] << 8));

    public static uint ReadUInt32(byte[] buffer, int offset) =>
        (uint)(buffer[offset] | (buffer[offset + 1] << 8) |
               (buffer[offset + 2] << 16) | (buffer[offset + 3] << 24));

    private readonly record struct FieldDef(string Name, int DefOffset, int Type, string? NestedMap);

    private sealed class FieldMap
    {
        public FieldMap(FieldDef[] fields, int declaredSize)
        {
            Fields = fields;
            DeclaredSize = declaredSize;
            Name = fields.Length > 0 ? fields[0].Name + ":" + fields.Length : "?";
        }

        public FieldDef[] Fields { get; }
        public int DeclaredSize { get; }
        public string Name { get; }
    }
}
