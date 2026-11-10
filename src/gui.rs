#![cfg_attr(windows, windows_subsystem = "windows")]
mod application;
mod bootstrap;
mod pane_footer;

use fla_reanim::workflow::{self, Operation, Options};
use naui_winui3::{
    IWindowNative,
    Microsoft::UI::{
        Dispatching::DispatcherQueueTimer,
        Xaml::{
            self, Application, ApplicationInitializationCallback, Controls::*, DragEventArgs,
            DragEventHandler, DragOperationDeferral, FrameworkElement, Markup::XamlReader,
            Media::MicaBackdrop, RoutedEventHandler, Window,
        },
    },
};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};
use windows::{
    ApplicationModel::DataTransfer::{DataPackageOperation, StandardDataFormats},
    Foundation::{TimeSpan, TypedEventHandler},
    Win32::{
        Foundation::HWND,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
        UI::{
            Shell::{
                Common::COMDLG_FILTERSPEC, FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM,
                FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
            },
            WindowsAndMessaging::{
                GWL_EXSTYLE, GetWindowLongPtrW, MB_ICONERROR, MB_OK, MessageBoxW, SWP_FRAMECHANGED,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW,
                SetWindowPos, WS_EX_DLGMODALFRAME,
            },
        },
    },
    core::{HSTRING, Interface, Result, w},
};
use windows_future::{AsyncOperationCompletedHandler, IAsyncOperation};

#[link(name = "user32")]
unsafe extern "system" {
    fn GetDpiForWindow(window: HWND) -> u32;
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
    static APP: RefCell<Option<Application>> = const { RefCell::new(None) };
}
fn hs(s: impl AsRef<str>) -> HSTRING {
    HSTRING::from(s.as_ref())
}
fn with_ui(action: impl FnOnce(&Ui) -> Result<()>) -> Result<()> {
    let ui = UI.with(|s| s.borrow().clone());
    if let Some(ui) = ui {
        action(&ui)
    } else {
        Ok(())
    }
}
enum Event {
    DropReady,
    Log(String, bool),
    Finished,
}
type DroppedItems = windows_collections::IVectorView<windows::Storage::IStorageItem>;

// WinUI's deferral is apartment-bound. Keep it and the storage operation on
// the UI thread; the asynchronous callback sends only a completion signal.
struct PendingDrop {
    operation: IAsyncOperation<DroppedItems>,
    deferral: DragOperationDeferral,
}
impl Drop for PendingDrop {
    fn drop(&mut self) {
        let _ = self.deferral.Complete();
    }
}
#[derive(Clone, Copy)]
struct StatusTween {
    started: Instant,
    from_height: f64,
    to_height: f64,
    from_opacity: f64,
    to_opacity: f64,
}
struct Ui {
    window: Window,
    page: FrameworkElement,
    hwnd: HWND,
    pick: Button,
    pick_folder: Button,
    plus: CheckBox,
    strict: CheckBox,
    log: TextBox,
    status: InfoBar,
    status_popup: ScrollViewer,
    status_content: Grid,
    status_tween: Cell<Option<StatusTween>>,
    status_timer: DispatcherQueueTimer,
    progress: ProgressBar,
    sender: Sender<Event>,
    receiver: RefCell<Receiver<Event>>,
    pending_drop: RefCell<Option<PendingDrop>>,
    busy: RefCell<bool>,
    busy_visual_at: Cell<Option<Instant>>,
    busy_visual_active: Cell<bool>,
    operation: Cell<Operation>,
    timer: DispatcherQueueTimer,
    _pane_footer: pane_footer::PaneFooterAnimation,
}
fn find<T: Interface>(page: &FrameworkElement, name: &str) -> Result<T> {
    page.FindName(&hs(name))?.cast()
}
impl Ui {
    fn launch() -> Result<()> {
        let window = Window::new()?;
        window.SetTitle(&hs("FLA Reanim Compiler"))?;
        window.SetSystemBackdrop(&MicaBackdrop::new()?)?;
        let page: FrameworkElement =
            XamlReader::Load(&hs(include_str!("../ui/MainWindow.xaml")))?.cast()?;
        window.SetContent(&page)?;
        let hwnd = unsafe { window.cast::<IWindowNative>()?.WindowHandle()? };
        // Keep the native caption and controls, with no fallback application icon.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_DLGMODALFRAME.0 as isize);
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
            )?;
        }
        let timer = window.DispatcherQueue()?.CreateTimer()?;
        timer.SetInterval(TimeSpan { Duration: 500_000 })?;
        let status_timer = window.DispatcherQueue()?.CreateTimer()?;
        status_timer.SetInterval(TimeSpan { Duration: 160_000 })?;
        let (sender, receiver) = mpsc::channel();
        let pane_footer =
            pane_footer::PaneFooterAnimation::new(&page.cast()?, find(&page, "FormatPanel")?)?;
        let ui = Rc::new(Self {
            pick: find(&page, "PickButton")?,
            pick_folder: find(&page, "PickFolderButton")?,
            plus: find(&page, "PlusCheck")?,
            strict: find(&page, "StrictCheck")?,
            log: find(&page, "LogText")?,
            status: find(&page, "Status")?,
            status_popup: find(&page, "StatusPopup")?,
            status_content: find(&page, "StatusContent")?,
            status_tween: Cell::new(None),
            status_timer,
            progress: find(&page, "Progress")?,
            window,
            page,
            hwnd,
            sender,
            receiver: RefCell::new(receiver),
            pending_drop: RefCell::new(None),
            busy: RefCell::new(false),
            busy_visual_at: Cell::new(None),
            busy_visual_active: Cell::new(false),
            operation: Cell::new(Operation::Pack),
            timer,
            _pane_footer: pane_footer,
        });
        UI.with(|s| *s.borrow_mut() = Some(ui.clone()));
        ui.bind()?;
        ui.select_operation(Operation::Pack)?;
        ui.timer.Start()?;
        ui.window.Activate()?;
        // AppWindow sizes are physical pixels; XAML uses 96-DPI logical units.
        let dpi = unsafe { GetDpiForWindow(ui.hwnd) }.max(96) as i32;
        ui.window
            .AppWindow()?
            .Resize(windows::Graphics::SizeInt32 {
                Width: 960 * dpi / 96,
                Height: 680 * dpi / 96,
            })?;
        Ok(())
    }
    fn bind(&self) -> Result<()> {
        self.pick.Click(&RoutedEventHandler::new(|_, _| {
            with_ui(|ui| {
                if let Some(files) = ui.pick_files(false)? {
                    ui.start(files)?;
                }
                Ok(())
            })
        }))?;
        self.pick_folder.Click(&RoutedEventHandler::new(|_, _| {
            with_ui(|ui| {
                if let Some(files) = ui.pick_files(true)? {
                    ui.start(files)?;
                }
                Ok(())
            })
        }))?;
        let clear: Button = find(&self.page, "ClearButton")?;
        clear.Click(&RoutedEventHandler::new(|_, _| {
            with_ui(|ui| ui.log.SetText(&HSTRING::new()))
        }))?;
        find::<Button>(&self.page, "DismissStatus")?.Click(&RoutedEventHandler::new(|_, _| {
            with_ui(|ui| ui.hide_status())
        }))?;
        let navigation: NavigationView = self.page.cast()?;
        navigation.SelectionChanged(&TypedEventHandler::new(
            |_, args: windows_core::Ref<NavigationViewSelectionChangedEventArgs>| {
                with_ui(|ui| {
                    if let Some(args) = args.as_ref() {
                        let selected: NavigationViewItem = args.SelectedItem()?.cast()?;
                        let tag = selected
                            .Tag()?
                            .cast::<windows::Foundation::IPropertyValue>()?
                            .GetString()?
                            .to_string();
                        ui.select_operation(match tag.as_str() {
                            "unpack" => Operation::Unpack,
                            "tv" => Operation::TvToPc,
                            _ => Operation::Pack,
                        })?;
                    }
                    Ok(())
                })
            },
        ))?;
        let drop: Border = find(&self.page, "DropSurface")?;
        drop.DragOver(&DragEventHandler::new(|_, args| {
            with_ui(|ui| {
                if !*ui.busy.borrow()
                    && ui.pending_drop.borrow().is_none()
                    && let Some(args) = args.as_ref()
                {
                    args.SetAcceptedOperation(DataPackageOperation::Copy)?;
                }
                Ok(())
            })
        }))?;
        drop.Drop(&DragEventHandler::new(|_, args| {
            with_ui(|ui| {
                if let Some(args) = args.as_ref()
                    && let Err(error) = ui.begin_drop(args)
                {
                    let _ = ui
                        .sender
                        .send(Event::Log(format!("读取拖入文件失败：{error}"), true));
                }
                Ok(())
            })
        }))?;
        self.timer
            .Tick(&TypedEventHandler::new(|_, _| with_ui(|ui| ui.poll())))?;
        self.status_timer.Tick(&TypedEventHandler::new(|_, _| {
            with_ui(|ui| ui.animate_status())
        }))?;
        self.window.AppWindow()?.Closing(&TypedEventHandler::<
            _,
            naui_winui3::Microsoft::UI::Windowing::AppWindowClosingEventArgs,
        >::new(|_, args| {
            with_ui(|ui| {
                if *ui.busy.borrow() {
                    args.as_ref().unwrap().SetCancel(true)?;
                    ui.show_status(
                        "正在转换，请等待当前批次完成",
                        "",
                        InfoBarSeverity::Informational,
                    )?;
                }
                Ok(())
            })
        }))?;
        Ok(())
    }
    fn begin_drop(&self, args: &DragEventArgs) -> Result<()> {
        if *self.busy.borrow() || self.pending_drop.borrow().is_some() {
            return Ok(());
        }
        let data = args.DataView()?;
        if !data.Contains(&StandardDataFormats::StorageItems()?)? {
            return Ok(());
        }
        let pending = PendingDrop {
            operation: data.GetStorageItemsAsync()?,
            deferral: args.GetDeferral()?,
        };
        let sender = self.sender.clone();
        pending
            .operation
            .SetCompleted(&AsyncOperationCompletedHandler::<DroppedItems>::new(
                move |_, _| {
                    let _ = sender.send(Event::DropReady);
                    Ok(())
                },
            ))?;
        *self.pending_drop.borrow_mut() = Some(pending);
        Ok(())
    }
    fn finish_drop(&self) -> Result<()> {
        let Some(pending) = self.pending_drop.borrow_mut().take() else {
            return Ok(());
        };
        let paths = (|| -> Result<Vec<PathBuf>> {
            let items = pending.operation.GetResults()?;
            let mut paths = Vec::new();
            for index in 0..items.Size()? {
                paths.push(PathBuf::from(items.GetAt(index)?.Path()?.to_string()));
            }
            Ok(paths)
        })();
        // Complete and release WinUI objects here, before starting conversion.
        drop(pending);
        match paths {
            Ok(paths) => self.start(paths),
            Err(error) => {
                let _ = self
                    .sender
                    .send(Event::Log(format!("读取拖入文件失败：{error}"), true));
                Ok(())
            }
        }
    }
    fn select_operation(&self, operation: Operation) -> Result<()> {
        self.operation.set(operation);
        let visibility = |visible| {
            if visible {
                Xaml::Visibility::Visible
            } else {
                Xaml::Visibility::Collapsed
            }
        };
        self.plus
            .SetVisibility(visibility(operation == Operation::Pack))?;
        self.strict
            .SetVisibility(visibility(operation == Operation::Unpack))?;
        self.pick_folder
            .SetVisibility(visibility(operation == Operation::TvToPc))?;
        let (format, detail, subtitle, title, hint) = match operation {
            Operation::Unpack => (
                "reanim.compiled → FLA",
                "自动识别经典或增强格式",
                "把游戏动画还原为可编辑的 FLA",
                "拖放 .reanim.compiled 到这里",
                "自动识别经典或增强格式。已有输出会保留，请使用新文件名。",
            ),
            Operation::TvToPc => (
                "TV reanim.compiled → PC",
                "支持多文件和文件夹",
                "把 TV 版动画转换为 PC 版动画",
                "拖放 TV 动画文件或文件夹到这里",
                "输出保存在输入旁边，文件名添加 _pc。已有输出会保留。",
            ),
            Operation::Auto | Operation::Pack => (
                "FLA → reanim.compiled",
                "经典动画 · 分组 · 骨骼",
                "把 FLA / XFL 编译为游戏动画",
                "拖放 .fla / XFL 到这里",
                "支持多个文件。已有输出会保留，请使用新文件名。",
            ),
        };
        for (name, text) in [
            ("FormatText", format),
            ("FormatDetail", detail),
            ("Subtitle", subtitle),
            ("DropTitle", title),
            ("DropHint", hint),
        ] {
            find::<TextBlock>(&self.page, name)?.SetText(&hs(text))?;
        }
        Ok(())
    }
    fn set_busy(&self, busy: bool) -> Result<()> {
        *self.busy.borrow_mut() = busy;
        if busy {
            // Short conversions should not flash the whole set of controls
            // through disabled/enabled styles or briefly show a progress bar.
            self.busy_visual_at
                .set(Some(Instant::now() + Duration::from_millis(150)));
        } else {
            self.busy_visual_at.set(None);
            if self.busy_visual_active.replace(false) {
                self.apply_busy_visual(false)?;
            }
        }
        Ok(())
    }
    fn apply_busy_visual(&self, busy: bool) -> Result<()> {
        self.pick.SetIsEnabled(!busy)?;
        self.pick_folder.SetIsEnabled(!busy)?;
        self.plus.SetIsEnabled(!busy)?;
        self.strict.SetIsEnabled(!busy)?;
        for name in ["PackItem", "UnpackItem", "TvItem"] {
            find::<NavigationViewItem>(&self.page, name)?.SetIsEnabled(!busy)?;
        }
        // Reserve the bar's space so that starting/finishing never moves text.
        self.progress.SetIsIndeterminate(busy)?;
        self.progress.SetOpacity(if busy { 1.0 } else { 0.0 })?;
        Ok(())
    }
    fn show_status(&self, title: &str, message: &str, severity: InfoBarSeverity) -> Result<()> {
        self.status.SetSeverity(severity)?;
        self.status.SetTitle(&hs(title))?;
        self.status.SetMessage(&hs(message))?;
        self.status_popup.SetIsHitTestVisible(true)?;
        self.status_content.Measure(windows::Foundation::Size {
            Width: self.status_popup.ActualWidth()? as f32,
            Height: f32::INFINITY,
        })?;
        self.transition_status(self.status_content.DesiredSize()?.Height as f64, 1.0)
    }
    fn hide_status(&self) -> Result<()> {
        self.status_popup.SetIsHitTestVisible(false)?;
        self.transition_status(0.0, 0.0)
    }
    fn transition_status(&self, height: f64, opacity: f64) -> Result<()> {
        let from_height = self.status_popup.ActualHeight()?;
        let from_opacity = self.status_content.Opacity()?;
        if (height - from_height).abs() < 0.1 && (opacity - from_opacity).abs() < 0.001 {
            self.status_tween.set(None);
            self.status_timer.Stop()?;
            return Ok(());
        }
        // Animate the real layout height, including the notice's top spacing.
        // The ScrollViewer clips partially revealed content during insertion.
        self.status_popup.SetHeight(from_height)?;
        self.status_tween.set(Some(StatusTween {
            started: Instant::now(),
            from_height,
            to_height: height,
            from_opacity,
            to_opacity: opacity,
        }));
        self.status_timer.Start()
    }
    fn animate_status(&self) -> Result<()> {
        let Some(tween) = self.status_tween.get() else {
            return Ok(());
        };
        let progress = (tween.started.elapsed().as_secs_f64() / 0.22).min(1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        self.status_popup
            .SetHeight(tween.from_height + (tween.to_height - tween.from_height) * eased)?;
        self.status_content
            .SetOpacity(tween.from_opacity + (tween.to_opacity - tween.from_opacity) * eased)?;
        if progress >= 1.0 {
            self.status_tween.set(None);
            self.status_timer.Stop()?;
            if tween.to_opacity > 0.0 {
                // Once open, follow wrapping and window resizing naturally.
                self.status_popup.SetHeight(f64::NAN)?;
            }
        }
        Ok(())
    }
    fn pick_files(&self, folders: bool) -> Result<Option<Vec<PathBuf>>> {
        unsafe {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            dialog.SetOptions(dialog.GetOptions()? | FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM)?;
            if folders {
                dialog.SetOptions(dialog.GetOptions()? | FOS_PICKFOLDERS)?;
            } else {
                let (name, extensions) = match self.operation.get() {
                    Operation::Pack | Operation::Auto => (w!("FLA / XFL"), w!("*.fla;*.xfl")),
                    Operation::Unpack | Operation::TvToPc => {
                        (w!("Reanim compiled"), w!("*.compiled"))
                    }
                };
                dialog.SetFileTypes(&[
                    COMDLG_FILTERSPEC {
                        pszName: name,
                        pszSpec: extensions,
                    },
                    COMDLG_FILTERSPEC {
                        pszName: w!("All files"),
                        pszSpec: w!("*.*"),
                    },
                ])?;
            }
            if let Err(error) = dialog.Show(Some(self.hwnd)) {
                if error.code().0 as u32 == 0x800704c7 {
                    return Ok(None);
                }
                return Err(error);
            }
            let items = dialog.GetResults()?;
            let mut files = Vec::new();
            for i in 0..items.GetCount()? {
                let name = items.GetItemAt(i)?.GetDisplayName(SIGDN_FILESYSPATH)?;
                let text = name.to_string();
                CoTaskMemFree(Some(name.0.cast()));
                files.push(PathBuf::from(text?));
            }
            Ok(Some(files))
        }
    }
    fn start(&self, files: Vec<PathBuf>) -> Result<()> {
        if *self.busy.borrow() || files.is_empty() {
            return Ok(());
        }
        self.set_busy(true)?;
        self.hide_status()?;
        let operation = self.operation.get();
        let options = Options {
            operation,
            plus: operation == Operation::Pack && self.plus.IsChecked()?.Value()?,
            strict_tweens: operation == Operation::Unpack && self.strict.IsChecked()?.Value()?,
            ..Default::default()
        };
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let mut jobs = Vec::new();
            for input in files {
                match workflow::expand_inputs(&input, &options) {
                    Ok(inputs) => jobs.extend(inputs),
                    Err(error) => {
                        let _ = sender
                            .send(Event::Log(format!("{}：{error:#}", input.display()), true));
                    }
                }
            }
            for (input, options) in jobs {
                let _ = sender.send(Event::Log(format!("正在转换 {}", input.display()), false));
                match workflow::convert(&input, &options) {
                    Ok(report) => {
                        let _ = sender.send(Event::Log(
                            format!(
                                "完成 → {}\n{} 轨道 · {} 帧 · {:.3} 秒\n{}",
                                report.output.display(),
                                report.tracks,
                                report.frames,
                                report.seconds,
                                report.detail
                            ),
                            false,
                        ));
                    }
                    Err(error) => {
                        let _ = sender.send(Event::Log(format!("失败：{error:#}"), true));
                    }
                }
            }
            let _ = sender.send(Event::Finished);
        });
        Ok(())
    }
    fn poll(&self) -> Result<()> {
        let events: Vec<_> = self.receiver.borrow().try_iter().collect();
        for event in events {
            match event {
                Event::DropReady => self.finish_drop()?,
                Event::Log(text, error) => {
                    self.log
                        .SetText(&hs(format!("{}{}\n\n", self.log.Text()?, text)))?;
                    if error {
                        self.show_status("转换失败", &text, InfoBarSeverity::Error)?;
                    }
                }
                Event::Finished => {
                    self.set_busy(false)?;
                }
            }
        }
        let now = Instant::now();
        if self.busy_visual_at.get().is_some_and(|at| now >= at) {
            self.busy_visual_at.set(None);
            self.busy_visual_active.set(true);
            self.apply_busy_visual(true)?;
        }
        Ok(())
    }
}
fn run() -> Result<()> {
    naui_winui3::init_apartment(naui_winui3::ApartmentType::SingleThreaded)?;
    Application::Start(&ApplicationInitializationCallback::new(|_| {
        let app = application::create(|| {
            let initialize = || -> Result<()> {
                let fluent: Xaml::ResourceDictionary = XamlControlsResources::new()?.cast()?;
                Application::Current()?
                    .Resources()?
                    .MergedDictionaries()?
                    .Append(&fluent)?;
                Ui::launch()
            };
            if let Err(error) = initialize() {
                show_error(&format!("{error:?}"));
                Application::Current()?.Exit()?;
            }
            Ok(())
        })?;
        APP.with(|s| *s.borrow_mut() = Some(app));
        Ok(())
    }))?;
    UI.with(|s| *s.borrow_mut() = None);
    APP.with(|s| *s.borrow_mut() = None);
    Ok(())
}
fn show_error(message: &str) {
    unsafe {
        MessageBoxW(
            None,
            windows::core::PCWSTR(hs(message).as_ptr()),
            w!("FLA Reanim Compiler"),
            MB_OK | MB_ICONERROR,
        );
    }
}
fn main() {
    if std::env::args_os().len() > 1 {
        // CLI runs before WinUI initialization, so it requires no Windows App Runtime.
        if let Err(error) = workflow::cli(std::env::args().skip(1)) {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }
    let _bootstrap = match bootstrap::initialize() {
        Ok(value) => value,
        Err(error) => {
            error.show_dialog();
            return;
        }
    };
    if let Err(error) = run() {
        show_error(&format!("{error:?}"));
    }
}
