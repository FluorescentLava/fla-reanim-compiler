using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using System.Runtime.InteropServices;
using WinRT;

namespace FlaReanimCompiler;

internal static class Program
{
    [STAThread]
    private static void Main(string[] args)
    {
        string runtimeBaseDirectory = Path.GetDirectoryName(typeof(Program).Assembly.Location)
            ?? AppContext.BaseDirectory;
        if (!runtimeBaseDirectory.EndsWith(Path.DirectorySeparatorChar))
            runtimeBaseDirectory += Path.DirectorySeparatorChar;

        Environment.SetEnvironmentVariable(
            "MICROSOFT_WINDOWSAPPRUNTIME_BASE_DIRECTORY",
            runtimeBaseDirectory);
        SetDllDirectory(runtimeBaseDirectory);

        ComWrappersSupport.InitializeComWrappers();
        Application.Start(_ =>
        {
            var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            new App();
        });
    }

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern bool SetDllDirectory(string lpPathName);
}
