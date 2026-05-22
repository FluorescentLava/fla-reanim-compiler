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

    private static int ConvertFromCommandLine(IEnumerable<string> paths)
    {
        int exitCode = 0;
        var converter = new FlaToReanimConverter();

        foreach (string path in paths)
        {
            try
            {
                ConversionResult result = converter.Convert(path);
                Console.WriteLine($"{Path.GetFileName(path)} -> {result.OutputPath}");
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"{Path.GetFileName(path)}: {ex.Message}");
                exitCode = 1;
            }
        }

        return exitCode;
    }

    [DllImport("shell32.dll", SetLastError = true)]
    private static extern nint CommandLineToArgvW(
        [MarshalAs(UnmanagedType.LPWStr)] string lpCmdLine,
        out int pNumArgs);

    [DllImport("kernel32.dll")]
    private static extern nint LocalFree(nint hMem);
}
