using System.IO;
using System.Text.RegularExpressions;
using System.Xml.Linq;

namespace FlaReanimCompiler;

/// <summary>Where a resource id actually lives, and how big the image is.</summary>
internal sealed record ImageResource(string ImageId, string RelativePath, int Width, int Height)
{
    public string FileName => Path.GetFileName(RelativePath);
}

/// <summary>
/// Resolves packed <c>IMAGE_*</c> resource ids back to real files, mirroring the
/// engine's lookup order in <c>DefinitionLoadImage</c> (TodLib/Definition.cpp):
/// first <c>Resources/resources.xml</c> via the registered id/prefix tables, then the
/// <c>gDefLoadResPaths</c> directories (<c>reanim\</c>, <c>images\</c>,
/// <c>particles\</c>).
///
/// Matching is by normalised stem, so an id such as
/// <c>IMAGE_REANIM_PLANTSHADOW</c> resolves to <c>images/plantshadow.png</c> even
/// though the file lives outside <c>Resources/reanim</c>.
/// </summary>
internal sealed class ImageResourceCatalog
{
    private static readonly string[] ImageExtensions = [".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp"];

    /// <summary>Directories the engine searches by prefix (gDefLoadResPaths).</summary>
    private static readonly string[] ReanimDirectories = ["reanim", "images", "particles"];

    private readonly Dictionary<string, string> _idToPath = new(StringComparer.OrdinalIgnoreCase);
    private readonly Dictionary<string, string> _stemToPath = new(StringComparer.OrdinalIgnoreCase);

    private ImageResourceCatalog(string resourcesRoot)
    {
        ResourcesRoot = resourcesRoot;
    }

    public string ResourcesRoot { get; }

    /// <summary>
    /// Walks up from <paramref name="sourcePath"/> looking for a <c>Resources</c>
    /// directory, so the tool works on any project layout.
    /// </summary>
    public static ImageResourceCatalog? TryLoadFor(string sourcePath)
    {
        DirectoryInfo? directory = new FileInfo(Path.GetFullPath(sourcePath)).Directory;
        while (directory is not null)
        {
            string candidate = Path.Combine(directory.FullName, "Resources");
            if (Directory.Exists(candidate))
                return Load(candidate);

            directory = directory.Parent;
        }

        return null;
    }

    public static ImageResourceCatalog Load(string resourcesRoot)
    {
        var catalog = new ImageResourceCatalog(resourcesRoot);
        catalog.LoadResourcesXml();
        catalog.IndexImageFiles();
        return catalog;
    }

    /// <summary>
    /// Resolves a compiled image id. Returns <c>null</c> when no file exists on disk,
    /// which the caller reports as a fidelity warning rather than silently dropping.
    /// </summary>
    public ImageResource? Resolve(string imageId)
    {
        if (string.IsNullOrWhiteSpace(imageId))
            return null;

        if (_idToPath.TryGetValue(imageId, out string? registered) && File.Exists(Path.Combine(ResourcesRoot, registered)))
            return Create(imageId, registered);

        if (_stemToPath.TryGetValue(imageId, out string? discovered))
            return Create(imageId, discovered);

        return null;
    }

    private ImageResource Create(string imageId, string relativePath)
    {
        string full = Path.Combine(ResourcesRoot, relativePath);
        (int width, int height) = ImageDimensions.TryRead(full);
        return new ImageResource(imageId, relativePath.Replace('\\', '/'), width, height);
    }

    /// <summary>
    /// The name to place on a FLA library item.
    ///
    /// This is deliberately the <b>packed resource id</b> (for example
    /// <c>IMAGE_REANIM_BACKGROUND1</c>), not the file path. The forward converter
    /// derives the packed id from the library item name, and naming it after the path
    /// breaks every id that is registered in resources.xml under a different prefix
    /// (<c>IMAGE_REANIM_BACKGROUND1</c> lives at <c>images/background/background1.png</c>,
    /// so a path-based name would come back as <c>IMAGE_BACKGROUND1</c>). Naming it after
    /// the id round-trips 1468 of the 1471 ids in use; the remaining three contain
    /// characters the converter's id normaliser cannot preserve.
    ///
    /// The real file location is still carried on the media item's href, so Animate can
    /// resolve the artwork.
    /// </summary>
    public static string GetLibraryItemName(ImageResource resource) => resource.ImageId;

    /// <summary>
    /// Fallback library item name for an id with no file on disk. Identical to the
    /// normal case: the packed id itself is the round-trippable name.
    /// </summary>
    public static string GetFallbackLibraryItemName(string imageId) => imageId;

    /// <summary>
    /// Builds the id table from resources.xml, honouring <c>SetDefaults path</c> and
    /// <c>SetDefaults idprefix</c> exactly as the forward converter does.
    /// </summary>
    private void LoadResourcesXml()
    {
        string path = Path.Combine(ResourcesRoot, "resources.xml");
        if (!File.Exists(path))
            return;

        XDocument document;
        try
        {
            document = XDocument.Load(path, LoadOptions.None);
        }
        catch (Exception)
        {
            return;
        }

        string currentPath = "";
        string currentPrefix = "";
        foreach (XElement element in document.Descendants())
        {
            if (element.Name.LocalName == "SetDefaults")
            {
                string? p = element.Attribute("path")?.Value;
                if (p is not null)
                    currentPath = p.Trim().TrimEnd('/', '\\');

                string? prefix = element.Attribute("idprefix")?.Value;
                if (prefix is not null)
                    currentPrefix = prefix.Trim();

                continue;
            }

            if (element.Name.LocalName != "Image")
                continue;

            string pathValue = element.Attribute("path")?.Value ?? "";
            if (string.IsNullOrWhiteSpace(pathValue))
                continue;

            string? idValue = element.Attribute("id")?.Value;
            string imageId = currentPrefix + (string.IsNullOrWhiteSpace(idValue)
                ? Path.GetFileNameWithoutExtension(pathValue)
                : idValue.Trim());

            string relative = string.IsNullOrWhiteSpace(currentPath)
                ? pathValue
                : currentPath + "/" + pathValue;

            string? resolved = FindWithExtension(relative);
            if (resolved is not null)
            {
                _idToPath.TryAdd(imageId, resolved);
                continue;
            }

            // Keep the declared path even without a matching file, so the caller can
            // still report the intended location.
            _idToPath.TryAdd(imageId, relative);
        }
    }

    /// <summary>
    /// Indexes every image by normalised stem, so ids whose resources.xml entry is
    /// absent (the engine loads those through gDefLoadResPaths) still resolve.
    /// The engine's own search order is preserved when stems collide.
    /// </summary>
    private void IndexImageFiles()
    {
        foreach (string directoryName in ReanimDirectories)
        {
            string directory = Path.Combine(ResourcesRoot, directoryName);
            if (!Directory.Exists(directory))
                continue;

            foreach (string file in Directory.EnumerateFiles(directory, "*", SearchOption.AllDirectories))
            {
                if (!ImageExtensions.Contains(Path.GetExtension(file), StringComparer.OrdinalIgnoreCase))
                    continue;

                string relative = Path.GetRelativePath(ResourcesRoot, file).Replace('\\', '/');
                string stem = NormalizeResourceId(Path.GetFileNameWithoutExtension(file));

                _stemToPath.TryAdd("IMAGE_REANIM_" + stem, relative);
                _stemToPath.TryAdd("IMAGE_" + stem, relative);
            }
        }
    }

    private string? FindWithExtension(string relativePath)
    {
        string normalized = relativePath.Replace('\\', '/');
        if (ImageExtensions.Contains(Path.GetExtension(normalized), StringComparer.OrdinalIgnoreCase) &&
            File.Exists(Path.Combine(ResourcesRoot, normalized)))
        {
            return normalized;
        }

        string withoutExtension = Path.ChangeExtension(normalized, null);
        foreach (string extension in ImageExtensions)
        {
            string candidate = withoutExtension + extension;
            if (File.Exists(Path.Combine(ResourcesRoot, candidate)))
                return candidate;
        }

        return null;
    }

    /// <summary>Mirrors FlaToReanimConverter.NormalizeResourceId.</summary>
    internal static string NormalizeResourceId(string value) =>
        Regex.Replace(value.ToUpperInvariant(), "[^A-Z0-9]+", "_").Trim('_');
}

/// <summary>
/// Minimal header readers for the image formats the project ships. Avoids a
/// System.Drawing dependency so the tool keeps building as a plain WinUI app.
/// </summary>
internal static class ImageDimensions
{
    public static (int Width, int Height) TryRead(string path)
    {
        try
        {
            using FileStream stream = File.OpenRead(path);
            Span<byte> header = stackalloc byte[32];
            int read = stream.Read(header);
            if (read < 8)
                return (0, 0);

            // PNG: 8-byte signature, then IHDR with big-endian width/height at 16/20.
            if (header[0] == 0x89 && header[1] == 0x50 && header[2] == 0x4E && header[3] == 0x47)
                return ReadPng(stream, header, read);

            // JPEG: walk the marker chain to a Start-Of-Frame segment.
            if (header[0] == 0xFF && header[1] == 0xD8)
                return ReadJpeg(stream);

            // GIF: little-endian width/height at offset 6.
            if (header[0] == 'G' && header[1] == 'I' && header[2] == 'F')
                return (header[6] | (header[7] << 8), header[8] | (header[9] << 8));

            // BMP: little-endian width/height at offset 18/22.
            if (header[0] == 'B' && header[1] == 'M')
            {
                Span<byte> bmp = stackalloc byte[26];
                stream.Position = 0;
                if (stream.Read(bmp) < 26)
                    return (0, 0);
                return (BitConverter.ToInt32(bmp[18..22]), Math.Abs(BitConverter.ToInt32(bmp[22..26])));
            }
        }
        catch (IOException)
        {
        }
        catch (UnauthorizedAccessException)
        {
        }

        return (0, 0);
    }

    private static (int, int) ReadPng(FileStream stream, Span<byte> header, int read)
    {
        if (read < 24)
        {
            Span<byte> full = stackalloc byte[24];
            stream.Position = 0;
            if (stream.Read(full) < 24)
                return (0, 0);
            return (ReadBigEndian(full[16..20]), ReadBigEndian(full[20..24]));
        }

        return (ReadBigEndian(header[16..20]), ReadBigEndian(header[20..24]));
    }

    private static int ReadBigEndian(ReadOnlySpan<byte> bytes) =>
        (bytes[0] << 24) | (bytes[1] << 16) | (bytes[2] << 8) | bytes[3];

    private static (int, int) ReadJpeg(FileStream stream)
    {
        stream.Position = 2;
        while (stream.Position < stream.Length - 1)
        {
            int marker = stream.ReadByte();
            if (marker != 0xFF)
                continue;

            int type;
            do
            {
                type = stream.ReadByte();
            }
            while (type == 0xFF);

            if (type is 0xD8 or 0x01 || (type >= 0xD0 && type <= 0xD7))
                continue;

            int high = stream.ReadByte();
            int low = stream.ReadByte();
            if (high < 0 || low < 0)
                break;

            int length = (high << 8) | low;
            // SOF0..SOF15 except DHT(C4), JPG(C8), DAC(CC) carry the frame size.
            if (type >= 0xC0 && type <= 0xCF && type != 0xC4 && type != 0xC8 && type != 0xCC)
            {
                Span<byte> sof = stackalloc byte[5];
                if (stream.Read(sof) < 5)
                    return (0, 0);
                int height = (sof[1] << 8) | sof[2];
                int width = (sof[3] << 8) | sof[4];
                return (width, height);
            }

            stream.Position += Math.Max(0, length - 2);
        }

        return (0, 0);
    }
}
