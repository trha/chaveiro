//! WinUI startup: load the Windows App Runtime, enter STA, start the XAML
//! application, and run a closure once it has launched.
//!
//! The steps follow windows-reactor 0.100 (`native/winui/{bootstrap,app_shim}.rs`,
//! MIT or Apache-2.0).

#![allow(non_snake_case)]

use crate::bindings::*;
use std::{cell::RefCell, rc::Rc};
use windows_core::{
    Array, BOOL, HRESULT, HSTRING, Interface, PCWSTR, PWSTR, Ref, Result, implement_decl, w,
};

/// Starts WinUI on the calling thread and calls `on_launched` from
/// [`OnLaunched`](IApplicationOverrides_Impl::OnLaunched). Blocks until the XAML message loop ends, which by default
/// happens when the last window closes.
///
/// The process must not be packaged, and the Windows App Runtime 2.x (x64)
/// must be installed.
pub(crate) fn start(on_launched: impl 'static + FnOnce() -> Result<()>) -> Result<()> {
    bootstrap()?;
    // SAFETY: plain Win32 calls with valid arguments.
    unsafe {
        // Failure leaves the process DPI-unaware, which only blurs the window.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 as _);
        CoInitializeEx(core::ptr::null(), COINIT_APARTMENTTHREADED).ok()?;
    }
    let on_launched: Launch = Rc::new(RefCell::new(Some(Box::new(on_launched))));
    Application::Start(&ApplicationInitializationCallback::new(move |_| {
        // The callback cannot return an error, and without an application
        // the message loop has nothing to run, so a failure here must panic.
        let application = Application::compose(Overrides {
            controls: RefCell::new(None),
            on_launched: Rc::clone(&on_launched),
        })
        .expect("XAML application failed to compose");
        APPLICATION.with(|slot| *slot.borrow_mut() = Some(application));
    }))
}

fn bootstrap() -> Result<()> {
    // SAFETY: the out-pointers are valid, and both returned strings are freed
    // with the heap that allocated them.
    unsafe {
        let mut dependency_id = PWSTR::null();
        TryCreatePackageDependency(
            core::ptr::null_mut(),
            RUNTIME_FAMILY,
            RUNTIME_MINIMUM_VERSION,
            ARCHITECTURE_X64 | ARCHITECTURE_NEUTRAL,
            0,
            PCWSTR::null(),
            0,
            &mut dependency_id,
        )
        .ok()?;
        let mut context = core::ptr::null_mut();
        let mut package_full_name = PWSTR::null();
        let added = AddPackageDependency(
            PCWSTR(dependency_id.0),
            0,
            0,
            &mut context,
            &mut package_full_name,
        );
        // A failed free leaks two short strings once per process.
        let _ = HeapFree(GetProcessHeap(), 0, dependency_id.0.cast());
        let _ = HeapFree(GetProcessHeap(), 0, package_full_name.0.cast());
        added.ok()
    }
}

implement_decl! {
    impl Overrides as Overrides_Impl: [IApplicationOverrides, IXamlMetadataProvider]
}

struct Overrides {
    controls: RefCell<Option<XamlControlsXamlMetaDataProvider>>,
    on_launched: Launch,
}

impl Overrides {
    fn controls(&self) -> Result<IXamlMetadataProvider> {
        if let Some(provider) = self.controls.borrow().as_ref() {
            return provider.cast();
        }
        let provider = XamlControlsXamlMetaDataProvider::new()?;
        *self.controls.borrow_mut() = Some(provider.clone());
        provider.cast()
    }
}

impl IApplicationOverrides_Impl for Overrides_Impl {
    fn OnLaunched(&self, _args: Ref<'_, LaunchActivatedEventArgs>) -> Result<()> {
        let Some(on_launched) = self.on_launched.borrow_mut().take() else {
            return Ok(());
        };
        install_control_resources()?;
        on_launched()
    }
}

impl IXamlMetadataProvider_Impl for Overrides_Impl {
    fn GetXamlType(&self, r#type: &TypeName) -> Result<IXamlType> {
        self.controls()?.GetXamlType(r#type)
    }

    fn GetXamlTypeByFullName(&self, full_name: &HSTRING) -> Result<IXamlType> {
        self.controls()?
            .GetXamlTypeByFullName(&full_name.to_string_lossy())
    }

    fn GetXmlnsDefinitions(&self) -> Result<Array<XmlnsDefinition>> {
        self.controls()?.GetXmlnsDefinitions()
    }
}

/// Ends the XAML message loop, so [`start`] returns. Does nothing before
/// launch or after the loop has ended.
pub(crate) fn exit() {
    if let Some(application) = APPLICATION.with(|slot| slot.borrow().clone()) {
        // Exit fails only when the application is already shutting down,
        // which is what this asks for.
        let _ = application.Exit();
    }
}

fn install_control_resources() -> Result<()> {
    let application = APPLICATION
        .with(|slot| slot.borrow().clone())
        .expect("OnLaunched runs after the Start callback stored the application");
    let resources: ResourceDictionary = XamlControlsResources::new()?.cast()?;
    application
        .Resources()?
        .MergedDictionaries()?
        .Append(&resources)
}

windows_core::link!("api-ms-win-appmodel-runtime-l1-1-5.dll" "system" fn TryCreatePackageDependency(user: *mut core::ffi::c_void, package_family_name: PCWSTR, minimum_version: u64, architectures: u32, lifetime_kind: i32, lifetime_artifact: PCWSTR, options: i32, dependency_id: *mut PWSTR) -> HRESULT);
windows_core::link!("api-ms-win-appmodel-runtime-l1-1-5.dll" "system" fn AddPackageDependency(dependency_id: PCWSTR, rank: i32, options: i32, context: *mut *mut core::ffi::c_void, package_full_name: *mut PWSTR) -> HRESULT);
windows_core::link!("ole32.dll" "system" fn CoInitializeEx(reserved: *const core::ffi::c_void, apartment: u32) -> HRESULT);
windows_core::link!("user32.dll" "system" fn SetProcessDpiAwarenessContext(value: *mut core::ffi::c_void) -> BOOL);
windows_core::link!("kernel32.dll" "system" fn GetProcessHeap() -> *mut core::ffi::c_void);
windows_core::link!("kernel32.dll" "system" fn HeapFree(heap: *mut core::ffi::c_void, flags: u32, memory: *mut core::ffi::c_void) -> BOOL);

/// Version 2.7.0.0 of the Windows App Runtime, as packed by `PACKAGE_VERSION`.
const RUNTIME_MINIMUM_VERSION: u64 = 0x0002_0007_0000_0000;
const RUNTIME_FAMILY: PCWSTR = w!("Microsoft.WindowsAppRuntime.2_8wekyb3d8bbwe");
const ARCHITECTURE_NEUTRAL: u32 = 1;
const ARCHITECTURE_X64: u32 = 4;
const COINIT_APARTMENTTHREADED: u32 = 2;
const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: isize = -4;

type Launch = Rc<RefCell<Option<Box<dyn FnOnce() -> Result<()>>>>>;

thread_local! {
    /// Keeps the composed application alive for the life of the UI thread.
    static APPLICATION: RefCell<Option<Application>> = const { RefCell::new(None) };
}
