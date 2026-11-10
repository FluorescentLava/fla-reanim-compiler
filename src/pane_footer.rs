//! The WinUI projection omits dependency-property callbacks. Project just the
//! required ABI prefixes from Microsoft.UI.Xaml.winmd (Windows App SDK 2.3).
//! Watching IsPaneOpen directly starts the fade before the pane's first frame.

use std::ffi::c_void;

use naui_winui3::Microsoft::UI::Xaml::Controls::{NavigationView, StackPanel};
use windows_core::{
    HRESULT, IInspectable, IInspectable_Vtbl, IUnknown, IUnknown_Vtbl, Interface, Result, Type,
};

windows_core::imp::define_interface!(
    INavigationViewProperties,
    NavigationViewPropertiesVtable,
    0xdcd04caf_1904_564b_b0de_babaff9962f5
);
#[repr(C)]
pub struct NavigationViewPropertiesVtable {
    base: IInspectable_Vtbl,
    is_pane_open_property: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

windows_core::imp::define_interface!(
    IXamlObjectProperties,
    XamlObjectPropertiesVtable,
    0xe7beaee7_160e_50f7_8789_d63463f979fa
);
#[repr(C)]
pub struct XamlObjectPropertiesVtable {
    base: IInspectable_Vtbl,
    // GetValue, SetValue, ClearValue, ReadLocalValue, GetAnimationBaseValue.
    value_methods: [usize; 5],
    register_changed:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut i64) -> HRESULT,
    unregister_changed: unsafe extern "system" fn(*mut c_void, *mut c_void, i64) -> HRESULT,
}

#[windows_core::interface("f055bb21-219b-5b0c-805d-bcaedae15458")]
unsafe trait IDependencyPropertyChangedCallback: IUnknown {
    fn invoke(&self, sender: *mut c_void, property: *mut c_void) -> HRESULT;
}

#[windows_core::implement(IDependencyPropertyChangedCallback)]
struct FadeCallback(StackPanel);

impl IDependencyPropertyChangedCallback_Impl for FadeCallback_Impl {
    unsafe fn invoke(&self, sender: *mut c_void, _property: *mut c_void) -> HRESULT {
        // The callback's sender is a borrowed DependencyObject, not necessarily
        // its default NavigationView interface. Query the correct interface.
        let result = (|| -> Result<()> {
            let sender = unsafe { IInspectable::from_raw_borrowed(&sender) }
                .ok_or_else(|| windows_core::Error::from_hresult(HRESULT(0x80004003_u32 as i32)))?;
            let navigation: NavigationView = sender.cast()?;
            self.0
                .SetOpacity(if navigation.IsPaneOpen()? { 1.0 } else { 0.0 })
        })();
        result.into()
    }
}

pub struct PaneFooterAnimation {
    owner: IXamlObjectProperties,
    property: IInspectable,
    token: i64,
}

impl PaneFooterAnimation {
    pub fn new(navigation: &NavigationView, panel: StackPanel) -> Result<Self> {
        panel.SetOpacity(if navigation.IsPaneOpen()? { 1.0 } else { 0.0 })?;
        let statics = windows_core::factory::<NavigationView, INavigationViewProperties>()?;
        let owner: IXamlObjectProperties = navigation.cast()?;
        let mut property_raw = std::ptr::null_mut();
        // These interfaces use the published WinRT ABI and the runtime owns
        // the callback until unregister_changed. No Rust reference crosses it.
        unsafe {
            (statics.vtable().is_pane_open_property)(statics.as_raw(), &mut property_raw).ok()?;
        }
        let property: IInspectable = unsafe { Type::from_abi(property_raw)? };
        let callback: IDependencyPropertyChangedCallback = FadeCallback(panel).into();
        let mut token = 0;
        unsafe {
            (owner.vtable().register_changed)(
                owner.as_raw(),
                property.as_raw(),
                callback.as_raw(),
                &mut token,
            )
            .ok()?;
        }
        Ok(Self {
            owner,
            property,
            token,
        })
    }
}

impl Drop for PaneFooterAnimation {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.owner.vtable().unregister_changed)(
                self.owner.as_raw(),
                self.property.as_raw(),
                self.token,
            );
        }
    }
}
