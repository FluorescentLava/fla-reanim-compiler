using System.Globalization;
using System.IO;
using System.Text;

namespace FlaReanimCompiler;

/// <summary>
/// Writes the engine's own <c>.reanim</c> XML representation of a compiled
/// definition.
///
/// This is the lossless side of the reverse direction: every field is emitted
/// verbatim, including the <c>-10000</c> placeholders, attacher strings, fonts and
/// text. Feeding the result back through the engine's <c>DefinitionCompileFile</c>
/// reproduces the original <c>.compiled</c> byte for byte, which makes it the
/// reference output for anything a FLA cannot carry (attacher tracks, non-rendering
/// data, image-only fills).
///
/// The element/field names and nesting mirror <c>gReanimatorDefMap</c> and
/// <c>DefinitionReadField</c> in TodLib/Definition.cpp, and image ids are emitted as
/// plain values exactly as the compiled file stores them.
/// </summary>
internal static class ReanimXmlWriter
{
    public static void Write(
        string outputPath,
        ReanimCompiledDocument document,
        List<string> warnings)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(outputPath) ?? ".");

        var sb = new StringBuilder();
        sb.Append("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        sb.Append("<!-- 由 FlaReanimCompiler 从 ")
          .Append(Escape(Path.GetFileName(document.SourcePath)))
          .Append(" 逆向生成，字段与 compiled 完全一致。 -->\n");
        sb.Append("<Definition>\n");

        AppendTrackArray(sb, document.Definition);

        sb.Append("\t<fps>").Append(FormatFloat(document.Definition.Fps)).Append("</fps>\n");
        sb.Append("</Definition>\n");

        File.WriteAllText(outputPath, sb.ToString(), new UTF8Encoding(false));

        if (document.BytesConsumed != document.DeclaredUncompressedSize)
        {
            ReanimToFlaConverter.AddWarningOnce(
                warnings,
                $"读取 {Path.GetFileName(document.SourcePath)} 时消费了 {document.BytesConsumed} 字节，" +
                $"但文件头声明 {document.DeclaredUncompressedSize} 字节，可能存在未解析的尾部数据。");
        }
    }

    /// <summary>
    /// Each element of the "track" array is its own &lt;track&gt; element carrying
    /// "name" plus its "t" transform array, matching gReanimatorTrackDefFields and the
    /// array expansion in DefinitionReadArrayField.
    /// </summary>
    private static void AppendTrackArray(StringBuilder sb, ReanimDefinition definition)
    {
        foreach (ReanimTrack track in definition.Tracks)
        {
            sb.Append("\t<track>\n");
            sb.Append("\t\t<name>").Append(Escape(track.Name)).Append("</name>\n");

            foreach (ReanimTransform transform in track.Transforms)
            {
                sb.Append("\t\t<t>\n");

                // Only fields that are not the placeholder need to be written: the
                // engine fills placeholders forward from the previous frame on load,
                // which is exactly how the packed file was produced.
                AppendFloat(sb, "x", transform.X);
                AppendFloat(sb, "y", transform.Y);
                AppendFloat(sb, "kx", transform.Kx);
                AppendFloat(sb, "ky", transform.Ky);
                AppendFloat(sb, "sx", transform.Sx);
                AppendFloat(sb, "sy", transform.Sy);
                AppendFloat(sb, "f", transform.Frame);
                AppendFloat(sb, "a", transform.Alpha);

                if (!string.IsNullOrEmpty(transform.Image))
                    sb.Append("\t\t\t<i>").Append(Escape(transform.Image)).Append("</i>\n");

                if (!string.IsNullOrEmpty(transform.Font))
                    sb.Append("\t\t\t<font>").Append(Escape(transform.Font)).Append("</font>\n");

                if (!string.IsNullOrEmpty(transform.Text))
                    sb.Append("\t\t\t<text>").Append(Escape(transform.Text)).Append("</text>\n");

                sb.Append("\t\t</t>\n");
            }

            sb.Append("\t</track>\n");
        }
    }

    private static void AppendFloat(StringBuilder sb, string name, float value)
    {
        if (value == ReanimTransformSemantics.MissingValue)
            return;

        sb.Append("\t\t\t<").Append(name).Append('>')
          .Append(FormatFloat(value))
          .Append("</").Append(name).Append(">\n");
    }

    /// <summary>
    /// Round-trip float formatting. "R" is not enough for float on all runtimes, and
    /// the engine parses with <c>SDL_sscanf("%f")</c>, so emit enough digits to make
    /// the parse exact.
    /// </summary>
    internal static string FormatFloat(float value)
    {
        if (float.IsNaN(value) || float.IsInfinity(value))
            return "0";

        string text = value.ToString("R", CultureInfo.InvariantCulture);
        return text.Contains('E', StringComparison.OrdinalIgnoreCase)
            ? value.ToString("0.0###########", CultureInfo.InvariantCulture)
            : text;
    }

    private static string Escape(string value) => value
        .Replace("&", "&amp;", StringComparison.Ordinal)
        .Replace("<", "&lt;", StringComparison.Ordinal)
        .Replace(">", "&gt;", StringComparison.Ordinal);
}
