using Microsoft.UI.Xaml;
using System.Runtime.InteropServices;

namespace FlaReanimCompiler;

public partial class App : Application
{
    private Window? _window;

    public App()
    {
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        string[] fileArgs = GetStartupFileArgs(args);

        if (fileArgs.Length > 0)
        {
            int exitCode = ConvertFromCommandLine(fileArgs);
            Exit();
            Environment.Exit(exitCode);
            return;
        }

        _window = new MainWindow();
        _window.Activate();
    }

    private static string[] GetStartupFileArgs(LaunchActivatedEventArgs args)
    {
        string[] environmentArgs = Environment.GetCommandLineArgs()
            .Skip(1)
            .Where(arg => !string.IsNullOrWhiteSpace(arg))
            .ToArray();

        if (environmentArgs.Length > 0)
            return environmentArgs;

        return ParseActivationArguments(args.Arguments);
    }

    private static string[] ParseActivationArguments(string? arguments)
    {
        if (string.IsNullOrWhiteSpace(arguments))
            return [];

        string trimmed = arguments.Trim();
        if (File.Exists(trimmed))
            return [trimmed];

        nint argv = CommandLineToArgvW("app " + trimmed, out int argc);
        if (argv == 0)
            return [trimmed];

        try
        {
            string[] result = new string[Math.Max(0, argc - 1)];
            for (int i = 1; i < argc; i++)
            {
                nint argPointer = Marshal.ReadIntPtr(argv, i * nint.Size);
                result[i - 1] = Marshal.PtrToStringUni(argPointer) ?? "";
            }

            return result.Where(arg => !string.IsNullOrWhiteSpace(arg)).ToArray();
        }
        finally
        {
            LocalFree(argv);
        }
    }

    /// <summary>
    /// Command line entry point. Direction is chosen per file by extension, so the
    /// same executable handles both .fla -> .reanim.compiled and the reverse.
    ///
    ///   FlaReanimCompiler.exe Anim.fla                 编译
    ///   FlaReanimCompiler.exe Anim.reanim.compiled      逆向为 FLA
    ///   FlaReanimCompiler.exe --verify Anim.reanim.compiled  逆向并回环校验
    ///   FlaReanimCompiler.exe --inspect Anim.reanim.compiled  只打印内容，不写文件
    /// </summary>
    private static int ConvertFromCommandLine(IEnumerable<string> paths)
    {
        // The CLI prints Chinese warnings; without this the console codepage mangles
        // them. GUI launches have no console, so the call is guarded.
        try
        {
            Console.OutputEncoding = System.Text.Encoding.UTF8;
        }
        catch (IOException)
        {
            // No console attached (byte-for-byte GUI launch); nothing to configure.
        }

        var arguments = paths.ToList();
        bool verify = arguments.RemoveAll(a => IsSwitch(a, "--verify")) > 0;
        bool inspect = arguments.RemoveAll(a => IsSwitch(a, "--inspect")) > 0;
        bool linkOnly = arguments.RemoveAll(a => IsSwitch(a, "--link")) > 0;

        int exitCode = 0;

        foreach (string path in arguments)
        {
            try
            {
                if (path.EndsWith(".compiled", StringComparison.OrdinalIgnoreCase))
                {
                    if (inspect)
                    {
                        exitCode |= InspectCompiled(path);
                        continue;
                    }

                    if (verify)
                    {
                        exitCode |= VerifyReverse(path);
                        continue;
                    }

                    FlaConversionReport report = ReanimToFlaConverter.Convert(path, new FlaConversionOptions
                    {
                        EmbedBitmaps = !linkOnly
                    });

                    Console.WriteLine($"{Path.GetFileName(path)} -> {report.OutputPath}");
                    Console.WriteLine(
                        $"    {report.TrackCount} tracks ({report.RenderTrackCount} bitmap / " +
                        $"{report.LocatorTrackCount} locator / {report.LabelTrackCount} label / " +
                        $"{report.UnrepresentableTrackCount} xml-only), " +
                        $"{report.FrameCount} frames, {report.Fps:0.##} fps, {report.BitmapCount} bitmaps");

                    foreach (string warning in report.Warnings.Take(5))
                        Console.WriteLine($"    提示: {warning}");
                }
                else
                {
                    var converter = new FlaToReanimConverter();
                    ConversionResult result = converter.Convert(path);
                    Console.WriteLine($"{Path.GetFileName(path)} -> {result.OutputPath}");
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"{Path.GetFileName(path)}: {ex.Message}");
                exitCode = 1;
            }
        }

        return exitCode;
    }

    private static bool IsSwitch(string argument, string name) =>
        string.Equals(argument, name, StringComparison.OrdinalIgnoreCase);

    /// <summary>Prints the parsed definition without writing anything to disk.</summary>
    private static int InspectCompiled(string path)
    {
        ReanimCompiledDocument document = ReanimCompiledReader.Read(path);

        Console.WriteLine($"{Path.GetFileName(path)}");
        Console.WriteLine($"    schema 0x{document.SchemaHash:X8}, fps {document.Definition.Fps:0.##}, " +
                          $"{document.Definition.Tracks.Count} tracks x {document.Definition.FrameCount} frames");
        Console.WriteLine($"    消费 {document.BytesConsumed} / 声明 {document.DeclaredUncompressedSize} 字节");

        foreach (ReanimTrack track in document.Definition.Tracks)
        {
            ReanimTransform[] filled = ReanimTransformSemantics.FillForward(track.Transforms);
            string[] images = filled
                .Where(t => !string.IsNullOrEmpty(t.Image))
                .Select(t => t.Image)
                .Distinct(StringComparer.Ordinal)
                .ToArray();
            int visible = filled.Count(t => t.Frame >= 0.0f && !string.IsNullOrEmpty(t.Image));
            int firstVisible = Array.FindIndex(filled, t => t.Frame >= 0.0f && !string.IsNullOrEmpty(t.Image));
            int firstFrame = Array.FindIndex(filled, t => t.Frame >= 0.0f);
            string kind = images.Length > 0 ? "render" : "label/data";
            string detail = images.Length > 0
                ? string.Join(", ", images.Take(2))
                : (filled.Any(t => !string.IsNullOrEmpty(t.Text)) ? "attacher/text" : "");

            Console.WriteLine(
                $"    [{kind,-10}] {track.Name,-34} visible={visible,-5} " +
                $"firstVisible={firstVisible,-5} firstFrameGE0={firstFrame,-5} {detail}");
        }

        return 0;
    }

    /// <summary>Reverse conversion followed by a strict byte-level round-trip gate.</summary>
    private static int VerifyReverse(string path)
    {
        // The scratch directory must sit beside the source resource, NOT under %TEMP%.
        // The round trip re-runs the forward converter over the generated FLA, and that
        // converter locates Resources/ by walking up from the FLA path. From %TEMP% it
        // finds nothing, so every bitmap resolves as missing and the comparison reports
        // spurious differences.
        string sourceDirectory = Path.GetDirectoryName(Path.GetFullPath(path)) ?? ".";
        string workDirectory = Path.Combine(
            sourceDirectory,
            ".fla_reanim_verify",
            BuildVerifyFolderName(path));

        RoundTripResult result = ReanimRoundTripVerifier.Verify(path, workDirectory);

        Console.WriteLine($"{Path.GetFileName(path)}: {result.Status}");

        foreach (string difference in result.Differences.Take(20))
            Console.WriteLine($"    {difference}");

        if (result.Differences.Count > 20)
            Console.WriteLine($"    ... 还有 {result.Differences.Count - 20} 条差异");

        Console.WriteLine($"    校验产物: {result.VerificationPath}");

        return result.ByteExact || result.RenderingEquivalent ? 0 : 1;
    }

    /// <summary>
    /// Per-file scratch folder name for <c>--verify</c>. Inputs may be named either
    /// <c>X.reanim.compiled</c> or <c>X.compiled</c>, so the ".reanim" suffix is removed
    /// only when it is actually present.
    /// </summary>
    private static string BuildVerifyFolderName(string path)
    {
        string name = Path.GetFileName(path);
        if (name.EndsWith(".compiled", StringComparison.OrdinalIgnoreCase))
            name = name[..^".compiled".Length];

        if (name.EndsWith(".reanim", StringComparison.OrdinalIgnoreCase))
            name = name[..^".reanim".Length];

        foreach (char invalid in Path.GetInvalidFileNameChars())
            name = name.Replace(invalid, '_');

        return string.IsNullOrWhiteSpace(name) ? "reanim" : name;
    }

    [DllImport("shell32.dll", SetLastError = true)]
    private static extern nint CommandLineToArgvW(
        [MarshalAs(UnmanagedType.LPWStr)] string lpCmdLine,
        out int pNumArgs);

    [DllImport("kernel32.dll")]
    private static extern nint LocalFree(nint hMem);
}
