#![cfg_attr(windows, windows_subsystem = "windows")]
mod application;
mod bootstrap;

use fla_reanim::workflow::{self, Operation, Options};
use naui_winui3::{
    IWindowNative,
    Microsoft::UI::{
        Dispatching::DispatcherQueueTimer,
        Xaml::{
            self, Application, ApplicationInitializationCallback, Controls::*, DragEventHandler,
            FrameworkElement, Markup::XamlReader, Media::MicaBackdrop, RoutedEventHandler, Window,
        },
    },
};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
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
            WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
        },
    },
    core::{HSTRING, Interface, Result, w},
};
use windows_future::AsyncOperationCompletedHandler;

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
    Dropped(Vec<PathBuf>),
    Log(String, bool),
    Finished,
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
    progress: ProgressBar,
    sender: Sender<Event>,
    receiver: RefCell<Receiver<Event>>,
    busy: RefCell<bool>,
    operation: Cell<Operation>,
    timer: DispatcherQueueTimer,
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
        let timer = window.DispatcherQueue()?.CreateTimer()?;
        timer.SetInterval(TimeSpan { Duration: 500_000 })?;
        let (sender, receiver) = mpsc::channel();
        let ui = Rc::new(Self {
            pick: find(&page, "PickButton")?,
            pick_folder: find(&page, "PickFolderButton")?,
            plus: find(&page, "PlusCheck")?,
            strict: find(&page, "StrictCheck")?,
            log: find(&page, "LogText")?,
            status: find(&page, "Status")?,
            progress: find(&page, "Progress")?,
            window,
            page,
            hwnd,
            sender,
            receiver: RefCell::new(receiver),
            busy: RefCell::new(false),
            operation: Cell::new(Operation::Pack),
            timer,
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
        let navigation: NavigationView = self.page.cast()?;
        navigation.PaneClosed(&TypedEventHandler::new(|_, _| {
            with_ui(|ui| {
                find::<StackPanel>(&ui.page, "FormatPanel")?
                    .SetVisibility(Xaml::Visibility::Collapsed)
            })
        }))?;
        navigation.PaneOpening(&TypedEventHandler::new(|_, _| {
            with_ui(|ui| {
                find::<StackPanel>(&ui.page, "FormatPanel")?
                    .SetVisibility(Xaml::Visibility::Visible)
            })
        }))?;
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
                if !*ui.busy.borrow() {
                    args.as_ref()
                        .unwrap()
                        .SetAcceptedOperation(DataPackageOperation::Copy)?;
                }
                Ok(())
            })
        }))?;
        drop.Drop(&DragEventHandler::new(|_, args| {
            with_ui(|ui| {
                if *ui.busy.borrow() {
                    return Ok(());
                }
                let args = args.as_ref().unwrap();
                let data = args.DataView()?;
                if !data.Contains(&StandardDataFormats::StorageItems()?)? {
                    return Ok(());
                }
                let sender = ui.sender.clone();
                let deferral = args.GetDeferral()?;
                data.GetStorageItemsAsync()?
                    .SetCompleted(&AsyncOperationCompletedHandler::<
                        windows_collections::IVectorView<windows::Storage::IStorageItem>,
                    >::new(move |op, _| {
                        if let Some(op) = op.as_ref()
                            && let Ok(items) = op.GetResults()
                        {
                            let paths = items
                                .into_iter()
                                .filter_map(|i| i.Path().ok().map(|p| PathBuf::from(p.to_string())))
                                .collect();
                            let _ = sender.send(Event::Dropped(paths));
                        }
                        deferral.Complete()?;
                        Ok(())
                    }))?;
                Ok(())
            })
        }))?;
        self.timer
            .Tick(&TypedEventHandler::new(|_, _| with_ui(|ui| ui.poll())))?;
        self.window.AppWindow()?.Closing(&TypedEventHandler::<
            _,
            naui_winui3::Microsoft::UI::Windowing::AppWindowClosingEventArgs,
        >::new(|_, args| {
            with_ui(|ui| {
                if *ui.busy.borrow() {
                    args.as_ref().unwrap().SetCancel(true)?;
                    ui.status.SetTitle(&hs("正在转换，请等待当前批次完成"))?;
                    ui.status.SetIsOpen(true)?;
                }
                Ok(())
            })
        }))?;
        Ok(())
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
        self.pick.SetIsEnabled(!busy)?;
        self.pick_folder.SetIsEnabled(!busy)?;
        self.plus.SetIsEnabled(!busy)?;
        self.strict.SetIsEnabled(!busy)?;
        for name in ["PackItem", "UnpackItem", "TvItem"] {
            find::<NavigationViewItem>(&self.page, name)?.SetIsEnabled(!busy)?;
        }
        self.progress.SetVisibility(if busy {
            Xaml::Visibility::Visible
        } else {
            Xaml::Visibility::Collapsed
        })?;
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
        self.status.SetIsOpen(false)?;
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
                Event::Dropped(paths) => self.start(paths)?,
                Event::Log(text, error) => {
                    self.log
                        .SetText(&hs(format!("{}{}\n\n", self.log.Text()?, text)))?;
                    if error {
                        self.status.SetSeverity(InfoBarSeverity::Error)?;
                        self.status.SetTitle(&hs("转换失败"))?;
                        self.status.SetMessage(&hs(text))?;
                        self.status.SetIsOpen(true)?;
                    }
                }
                Event::Finished => {
                    self.set_busy(false)?;
                }
            }
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
