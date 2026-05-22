using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using System.Collections.ObjectModel;
using System.IO;
using System.Runtime.InteropServices;
using Windows.ApplicationModel.DataTransfer;
using Windows.Graphics;
using Windows.Storage;
using Windows.Storage.Pickers;
using WinRT.Interop;

namespace FlaReanimCompiler;

public sealed class ConversionLogItem
{
    public string Title { get; set; } = "";
    public string Detail { get; set; } = "";
    public Brush Accent { get; set; } = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 96, 96, 96));
    public Brush Background { get; set; } = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 255, 255, 255));
    public Brush BorderBrush { get; set; } = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 224, 224, 224));
}

public partial class MainWindow : Window
{
    private const int InitialWidthDip = 1120;
    private const int InitialHeightDip = 700;
    private const int MinimumWidthDip = 720;
    private const int MinimumHeightDip = 500;

    private readonly FlaToReanimConverter _converter = new();
    private readonly nint _windowHandle;
    private AppWindow? _appWindow;
    private Brush? _dropIdleBackground;
    private Brush? _dropIdleBorderBrush;
    private Brush? _dropIdleIconBackground;
    private readonly DispatcherTimer _statusHideTimer = new();
    private readonly DispatcherTimer _paneFooterRefreshTimer = new();
    private int _minimumWidthPixels;
    private int _minimumHeightPixels;
    private bool _busy;
    private bool _statusInitialized;

    public ObservableCollection<ConversionLogItem> LogItems { get; } = new();

    public MainWindow()
    {
        InitializeComponent();

        Title = "FLA Reanim Compiler";
        SystemBackdrop = new MicaBackdrop();

        _windowHandle = WindowNative.GetWindowHandle(this);
        _dropIdleBackground = DropSurface.Background;
        _dropIdleBorderBrush = DropSurface.BorderBrush;
        _dropIdleIconBackground = DropIconSurface.Background;
        ConfigureWindow();

        LogItems.CollectionChanged += (_, _) => UpdateEmptyState();
        _statusHideTimer.Interval = TimeSpan.FromSeconds(4.5);
        _statusHideTimer.Tick += (_, _) =>
        {
            _statusHideTimer.Stop();
            CloseStatusInfoBar();
        };
        _paneFooterRefreshTimer.Interval = TimeSpan.FromMilliseconds(260);
        _paneFooterRefreshTimer.Tick += (_, _) =>
        {
            _paneFooterRefreshTimer.Stop();
            UpdatePaneFooter();
        };

        UpdateEmptyState();
        UpdatePaneFooter();
    }

    private void ConfigureWindow()
    {
        WindowId windowId = Win32Interop.GetWindowIdFromWindow(_windowHandle);
        _appWindow = AppWindow.GetFromWindowId(windowId);

        int initialWidth = ScaleDipToPixels(InitialWidthDip);
        int initialHeight = ScaleDipToPixels(InitialHeightDip);
        _minimumWidthPixels = ScaleDipToPixels(MinimumWidthDip);
        _minimumHeightPixels = ScaleDipToPixels(MinimumHeightDip);

        if (_appWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.PreferredMinimumWidth = _minimumWidthPixels;
            presenter.PreferredMinimumHeight = _minimumHeightPixels;
        }

        DisplayArea displayArea = DisplayArea.GetFromWindowId(windowId, DisplayAreaFallback.Primary);
        RectInt32 workArea = displayArea.WorkArea;
        initialWidth = Math.Min(initialWidth, Math.Max(_minimumWidthPixels, workArea.Width - ScaleDipToPixels(48)));
        initialHeight = Math.Min(initialHeight, Math.Max(_minimumHeightPixels, workArea.Height - ScaleDipToPixels(48)));

        _appWindow.Resize(new SizeInt32(initialWidth, initialHeight));

        int x = workArea.X + Math.Max(0, (workArea.Width - initialWidth) / 2);
        int y = workArea.Y + Math.Max(0, (workArea.Height - initialHeight) / 2);
        _appWindow.Move(new PointInt32(x, y));
    }

    private int ScaleDipToPixels(int dips)
    {
        uint dpi = GetDpiForWindow(_windowHandle);
        return (int)Math.Ceiling(dips * dpi / 96.0);
    }

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(nint hwnd);

    private async void PickFiles_Click(object sender, RoutedEventArgs e)
    {
        if (_busy)
            return;

        var picker = new FileOpenPicker
        {
            SuggestedStartLocation = PickerLocationId.DocumentsLibrary
        };
        picker.FileTypeFilter.Add(".fla");
        InitializeWithWindow.Initialize(picker, _windowHandle);

        IReadOnlyList<StorageFile> files = await picker.PickMultipleFilesAsync();
        await ConvertFilesAsync(files.Select(file => file.Path));
    }

    private void Clear_Click(object sender, RoutedEventArgs e)
    {
        if (_busy)
            return;

        LogItems.Clear();
        SetStatus(InfoBarSeverity.Informational, "等待文件", "可以拖放多个 .fla 文件，也可以点击选择文件。", autoHide: false);
        DropTitleText.Text = "拖放 .fla 文件到这里";
        DropDetailText.Text = "支持多个文件，输出保存在原目录";
        SetDropHighlighted(false);
    }

    private void DropSurface_DragOver(object sender, DragEventArgs e)
    {
        e.AcceptedOperation = !_busy && e.DataView.Contains(StandardDataFormats.StorageItems)
            ? DataPackageOperation.Copy
            : DataPackageOperation.None;
        SetDropHighlighted(e.AcceptedOperation == DataPackageOperation.Copy);
        e.Handled = true;
    }

    private void DropSurface_DragLeave(object sender, DragEventArgs e)
    {
        if (!_busy)
            SetDropHighlighted(false);
    }

    private async void DropSurface_Drop(object sender, DragEventArgs e)
    {
        if (_busy || !e.DataView.Contains(StandardDataFormats.StorageItems))
            return;

        IReadOnlyList<IStorageItem> items = await e.DataView.GetStorageItemsAsync();
        string[] paths = items
            .OfType<StorageFile>()
            .Select(file => file.Path)
            .ToArray();

        await ConvertFilesAsync(paths);
        SetDropHighlighted(false);
    }

    private async Task ConvertFilesAsync(IEnumerable<string> paths)
    {
        string[] flaPaths = paths
            .Where(path => string.Equals(Path.GetExtension(path), ".fla", StringComparison.OrdinalIgnoreCase))
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .ToArray();

        if (flaPaths.Length == 0)
        {
            AddLog("没有可转换的文件", "请拖入 .fla 文件。", false);
            SetStatus(InfoBarSeverity.Warning, "没有可转换的文件", "请拖入 .fla 文件。");
            return;
        }

        SetBusy(true, flaPaths.Length);

        int ok = 0;
        int failed = 0;
        foreach (string path in flaPaths)
        {
            try
            {
                ConversionResult result = await Task.Run(() => _converter.Convert(path));
                ok++;

                string warningText = result.Warnings.Count == 0
                    ? ""
                    : $"，{result.Warnings.Count} 条提示";

                AddLog(
                    Path.GetFileName(result.OutputPath),
                    $"{result.TrackCount} tracks / {result.FrameCount} frames / {result.Fps:0.##} fps{warningText}\n{result.OutputPath}",
                    true);

                foreach (string warning in result.Warnings.Take(3))
                    AddLog("提示", warning, null);
            }
            catch (Exception ex)
            {
                failed++;
                AddLog(Path.GetFileName(path), ex.Message, false);
            }
        }

        if (failed == 0)
        {
            SetStatus(InfoBarSeverity.Success, $"完成 {ok} 个", "可以继续拖入新的 .fla 文件。");
            DropTitleText.Text = "转换完成";
        }
        else
        {
            SetStatus(InfoBarSeverity.Warning, $"完成 {ok} 个，失败 {failed} 个", "查看日志了解失败原因。");
            DropTitleText.Text = "部分文件失败";
        }

        DropDetailText.Text = "可以继续拖入新的 .fla 文件";
        SetBusy(false, 0);
    }

    private void SetBusy(bool busy, int count)
    {
        _busy = busy;
        PickFilesButton.IsEnabled = !busy;
        ClearButton.IsEnabled = !busy;
        BusyProgress.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
        DropSurface.Opacity = busy ? 0.82 : 1.0;
        UpdateEmptyState();

        if (busy)
        {
            SetStatus(InfoBarSeverity.Informational, $"正在处理 {count} 个文件", "解析 FLA 并写入 compiled cache。", autoHide: false);
            DropTitleText.Text = "正在转换";
            DropDetailText.Text = "请稍等，转换完成后会写入原目录";
            SetDropHighlighted(false);
        }
    }

    private void SetDropHighlighted(bool highlighted)
    {
        if (highlighted)
        {
            DropSurface.Background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 243, 249, 255));
            DropSurface.BorderBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 0, 95, 184));
            DropIconSurface.Background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 232, 244, 255));
            return;
        }

        if (_dropIdleBackground is not null)
            DropSurface.Background = _dropIdleBackground;

        if (_dropIdleBorderBrush is not null)
            DropSurface.BorderBrush = _dropIdleBorderBrush;

        if (_dropIdleIconBackground is not null)
            DropIconSurface.Background = _dropIdleIconBackground;
    }

    private void SetStatus(InfoBarSeverity severity, string title, string message, bool autoHide = true)
    {
        _statusHideTimer.Stop();

        StatusInfoBar.Severity = severity;
        StatusInfoBar.Title = title;
        StatusInfoBar.Message = message;

        if (StatusInfoBar.IsOpen)
        {
            StatusInfoBar.IsOpen = false;
            DispatcherQueue.TryEnqueue(() => StatusInfoBar.IsOpen = true);
        }
        else
        {
            StatusInfoBar.IsOpen = true;
        }

        if (autoHide)
            _statusHideTimer.Start();
    }

    private void AddLog(string title, string detail, bool? success)
    {
        Brush accent;
        Brush background;
        Brush border;

        if (success == true)
        {
            accent = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 15, 123, 95));
            background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 241, 250, 246));
            border = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 196, 232, 216));
        }
        else if (success == false)
        {
            accent = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 196, 43, 28));
            background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 255, 246, 245));
            border = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 242, 203, 199));
        }
        else
        {
            accent = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 0, 95, 184));
            background = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 248, 249, 251));
            border = new SolidColorBrush(Windows.UI.Color.FromArgb(255, 226, 230, 235));
        }

        LogItems.Insert(0, new ConversionLogItem
        {
            Title = title,
            Detail = detail,
            Accent = accent,
            Background = background,
            BorderBrush = border
        });
    }

    private void UpdateEmptyState()
    {
        bool showEmptyState = !_busy && LogItems.Count == 0;
        EmptyState.Visibility = showEmptyState ? Visibility.Visible : Visibility.Collapsed;
        LogList.Visibility = LogItems.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    private void UpdatePaneFooter()
    {
        bool showFooter = RootNavigation.IsPaneOpen && RootNavigation.DisplayMode == NavigationViewDisplayMode.Expanded;
        SetPaneFooterVisible(showFooter);
    }

    private void SetPaneFooterVisible(bool visible)
    {
        PaneFooterDetails.Opacity = visible ? 1 : 0;
        PaneFooterDetails.IsHitTestVisible = visible;
        PaneFooterDetails.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        PaneFooterDetails.InvalidateMeasure();
        RootNavigation.InvalidateMeasure();
    }

    private void QueuePaneFooterUpdate(bool? visible = null)
    {
        if (visible is bool immediateVisibility)
            SetPaneFooterVisible(immediateVisibility);
        else
            UpdatePaneFooter();

        DispatcherQueue.TryEnqueue(UpdatePaneFooter);
        _paneFooterRefreshTimer.Stop();
        _paneFooterRefreshTimer.Start();
    }

    private void RootNavigation_Loaded(object sender, RoutedEventArgs e)
    {
        if (_statusInitialized)
            return;

        _statusInitialized = true;
        QueuePaneFooterUpdate();
        SetStatus(InfoBarSeverity.Informational, "等待文件", "可以拖放多个 .fla 文件，也可以点击选择文件。", autoHide: false);
    }

    private void RootNavigation_PaneOpening(NavigationView sender, object args)
    {
        QueuePaneFooterUpdate(true);
    }

    private void RootNavigation_PaneClosing(NavigationView sender, object args)
    {
        QueuePaneFooterUpdate(false);
    }

    private void RootNavigation_PaneOpened(NavigationView sender, object args)
    {
        QueuePaneFooterUpdate();
    }

    private void RootNavigation_PaneClosed(NavigationView sender, object args)
    {
        QueuePaneFooterUpdate();
    }

    private void RootNavigation_DisplayModeChanged(NavigationView sender, NavigationViewDisplayModeChangedEventArgs args)
    {
        QueuePaneFooterUpdate();
    }

    private void CloseStatusInfoBar()
    {
        if (!StatusInfoBar.IsOpen || _busy)
            return;

        StatusInfoBar.IsOpen = false;
    }
}
