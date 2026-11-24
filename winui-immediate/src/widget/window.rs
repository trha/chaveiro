use crate::{
    bindings::{
        Color, CompositionBrush, CompositionTarget, Compositor, ElementTheme, FocusManager,
        FocusState, FrameworkElement, HorizontalAlignment, IAppWindowTitleBar3,
        ICompositionSupportsSystemBackdrop, IWindow2, Size, SizeInt32, TitleBarTheme, UIElement,
        VerticalAlignment, Window,
    },
    events::{EventKind, EventSet},
    pass::{ChildKind, NativeTree, Payload, Ui, WindowNode},
    tree::NodeId,
};
use std::{cell::Cell, hash::Hash, rc::Rc};
use windows_core::{HRESULT, IInspectable, Interface, Ref, Result};

impl Ui {
    /// Declares an open window. It is created hidden on its first pass and
    /// shows a few frames after that pass ends, once its content is drawn; a
    /// window a pass does not declare is closed. `body` must declare
    /// at most one control, which becomes the window's content.
    pub fn window(
        &mut self,
        id: impl Hash,
        title: &str,
        body: impl FnOnce(&mut Ui) -> Result<()>,
    ) -> Result<WindowResponse<'_>> {
        let (node, _) = self.visit(id, ChildKind::Logical, || {
            let window = Window::new()?;
            let app_window = window.cast::<IWindow2>()?.AppWindow()?;
            Ok(Payload::Window(WindowNode {
                window,
                app_window,
                content: None,
                title: String::new(),
                mica: false,
                dark: None,
                compact: false,
                extends: false,
                sized: false,
                fitted: (0, 0),
                fitted_content: None,
                fit: false,
                fitted_once: false,
                revealed: false,
                fit_tries: 0,
                title_bar: None,
                subscribed: EventSet::default(),
            }))
        })?;
        let window = self.window_mut(node);
        if window.title != title {
            window.window.SetTitle(title)?;
            window.title.clear();
            window.title.push_str(title);
        }
        let Payload::Window(window) = self.core.tree.native_mut(node) else {
            unreachable!("window keys hold windows")
        };
        self.events.subscribe(
            node,
            &mut window.subscribed,
            &window.app_window,
            EventKind::CloseRequested,
        )?;
        self.open(node);
        body(self)?;
        self.close()?;
        self.follow_theme(node)?;
        Ok(WindowResponse { ui: self, node })
    }

    /// Gives the window frame the content's theme: the DWM dark mode flag,
    /// which colors the frame and DWM's Mica, and the title bar's theme,
    /// which colors its caption buttons. XAML sets neither, and the caption
    /// buttons keep the theme the window started with when the system theme
    /// changes. A theme change raises an event, whose pass lands here.
    fn follow_theme(&mut self, node: NodeId) -> Result<()> {
        let Some(content) = self.window_mut(node).content else {
            return Ok(());
        };
        self.subscribe(content, &[EventKind::ThemeChanged], |control| {
            &control.element
        })?;
        // The theme is read only on the first pass with content and after a
        // change, so a steady pass makes no COM call here.
        let known = self.window_mut(node).dark.is_some();
        if known && !self.events.happened(content, EventKind::ThemeChanged) {
            return Ok(());
        }
        let dark = self
            .control_mut(content)
            .element
            .cast::<FrameworkElement>()?
            .ActualTheme()?
            == ElementTheme::Dark;
        let window = self.window_mut(node);
        if window.dark != Some(dark) {
            set_dwm_attribute(
                hwnd(&window.window)?,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                u32::from(dark),
            )?;
            window
                .app_window
                .TitleBar()?
                .cast::<IAppWindowTitleBar3>()?
                .SetPreferredTheme(match dark {
                    true => TitleBarTheme::Dark,
                    false => TitleBarTheme::Light,
                })?;
            window.dark = Some(dark);
        }
        Ok(())
    }

    fn window_mut(&mut self, node: NodeId) -> &mut WindowNode {
        match self.core.tree.native_mut(node) {
            Payload::Window(window) => window,
            _ => unreachable!("window keys hold windows"),
        }
    }
}

/// What [`Ui::window`] returns.
pub struct WindowResponse<'u> {
    ui: &'u mut Ui,
    node: NodeId,
}

impl WindowResponse<'_> {
    /// True on the pass after the user asked to close the window. The window
    /// stays open until a pass stops declaring it.
    pub fn close_requested(&self) -> bool {
        self.ui
            .events
            .happened(self.node, EventKind::CloseRequested)
    }

    /// Gives the window the Mica backdrop, which tints it with the desktop
    /// wallpaper. Content must leave its background unset to show it.
    pub fn mica(&mut self) -> Result<&mut Self> {
        let window = self.ui.window_mut(self.node);
        if !window.mica {
            // DWM draws its own Mica in step with the frame, so a resize shows
            // no black edges. `MicaBackdrop` draws on the XAML island, which
            // resizes a few frames after the frame does. DWM picks light or
            // dark Mica from the dark mode flag that `follow_theme` sets.
            let hwnd = hwnd(&window.window)?;
            let margins = [-1i32; 4];
            // SAFETY: `hwnd` is a live window; `margins` outlives the call.
            unsafe { DwmExtendFrameIntoClientArea(hwnd, margins.as_ptr()).ok()? };
            set_dwm_attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW)?;
            // Any backdrop brush makes XAML stop painting its opaque
            // background, so a clear one lets DWM's Mica show through.
            window
                .window
                .cast::<ICompositionSupportsSystemBackdrop>()?
                .SetSystemBackdrop(&clear_brush()?)?;
            window.mica = true;
        }
        Ok(self)
    }

    /// Turns the window into a small fixed dialog: the content extends into
    /// the title bar, which keeps only the close button, and the user cannot
    /// resize it. The content must draw its own title, and leave the top
    /// 32px free of controls, which stay the drag area.
    pub fn compact(&mut self) -> Result<&mut Self> {
        let window = self.ui.window_mut(self.node);
        if !window.compact {
            window.window.SetExtendsContentIntoTitleBar(true)?;
            // One style write and one frame change, because the three
            // `OverlappedPresenter` setters (`IsMaximizable`, `IsMinimizable`,
            // `IsResizable`) each change the frame, at about 10 ms each on
            // startup. The presenter keeps reporting its old values.
            let hwnd = hwnd(&window.window)?;
            // SAFETY: `hwnd` is a live window.
            unsafe {
                let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
                SetWindowLongPtrW(
                    hwnd,
                    GWL_STYLE,
                    style & !(WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX),
                );
                SetWindowPos(
                    hwnd,
                    core::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
                .ok()?;
            }
            window.compact = true;
        }
        Ok(self)
    }

    /// Lets the content extend into the title bar, so a [`Ui::title_bar`]
    /// draws it, while the window stays resizable with all its caption
    /// buttons. Leave the top 48px of the content to the title bar.
    pub fn extends_into_title_bar(&mut self) -> Result<&mut Self> {
        let window = self.ui.window_mut(self.node);
        if !window.extends {
            window.window.SetExtendsContentIntoTitleBar(true)?;
            window.extends = true;
        }
        Ok(self)
    }

    /// Sizes the window to `width` by `height` view pixels, scaled for its
    /// monitor, on the first pass that calls it. Later calls do nothing, so
    /// the user's resizes stay.
    pub fn initial_size(&mut self, width: f64, height: f64) -> Result<&mut Self> {
        let window = self.ui.window_mut(self.node);
        if !window.sized {
            let hwnd = hwnd(&window.window)?;
            // SAFETY: `hwnd` is a window handle.
            let dpi = unsafe { GetDpiForWindow(hwnd) };
            // 0 means the handle was not a window; 96 is 100% scale.
            let scale = match dpi {
                0 => 1.0,
                dpi => f64::from(dpi) / 96.0,
            };
            window.app_window.Resize(SizeInt32 {
                width: (width * scale).round() as i32,
                height: (height * scale).round() as i32,
            })?;
            window.sized = true;
        }
        Ok(self)
    }

    /// Sizes the window's client area to its content's size, whenever that
    /// size changes. The content is placed at the top left, so it takes its
    /// own size; give it a width. Does nothing until the first layout.
    pub fn fit_to_content(&mut self) -> Result<&mut Self> {
        let Some(content) = self.ui.window_mut(self.node).content else {
            return Ok(self);
        };
        let control = self.ui.control_mut(content);
        let element = control.element.clone();
        let mut subscribed = control.subscribed;
        self.ui
            .events
            .subscribe(content, &mut subscribed, &element, EventKind::SizeChanged)?;
        self.ui.control_mut(content).subscribed = subscribed;
        let frame = element.cast::<FrameworkElement>()?;
        // Once per content node, because the content changes between screens.
        if self.ui.window_mut(self.node).fitted_content != Some(content) {
            frame.SetHorizontalAlignment(HorizontalAlignment::Left)?;
            frame.SetVerticalAlignment(VerticalAlignment::Top)?;
            self.ui.window_mut(self.node).fitted_content = Some(content);
        }
        self.ui.window_mut(self.node).fit = true;
        if frame.ActualWidth()? <= 0.0 || frame.ActualHeight()? <= 0.0 {
            return Ok(self);
        }
        // Layout clamps the content to the window, so its actual size never
        // shows that it outgrew it. Measuring with no bound gives the size it
        // wants, including controls this pass added; the next layout measures
        // it again with the window's bound.
        element.Measure(Size {
            width: f32::INFINITY,
            height: f32::INFINITY,
        })?;
        let wanted = element.DesiredSize()?;
        let (width, height) = (f64::from(wanted.width), f64::from(wanted.height));
        let root = element.XamlRoot()?;
        let scale = root.RasterizationScale()?;
        let area = root.Size()?;
        // The XAML area and the client area differ by the title bar when the
        // content extends into it, so the outer size moves by the gap
        // between the XAML area and the content.
        let gap = |area: f32, content: f64| ((f64::from(area) - content) * scale).round() as i32;
        let (gap_width, gap_height) = (gap(area.width, width), gap(area.height, height));
        if gap_width == 0 && gap_height == 0 {
            self.ui.window_mut(self.node).fitted_once = true;
            return Ok(self);
        }
        let window = self.ui.window_mut(self.node);
        let outer = window.app_window.Size()?;
        let size = (outer.width - gap_width, outer.height - gap_height);
        // The same target twice means the last resize has not landed yet.
        if window.fitted != size {
            window.app_window.Resize(SizeInt32 {
                width: size.0,
                height: size.1,
            })?;
            window.fitted = size;
            // A resize can change the frame, so the first fit waits for a
            // pass that finds no gap; a few tries bound the wait.
            window.fit_tries += 1;
            if window.fit_tries >= FIT_TRIES {
                window.fitted_once = true;
            }
        }
        Ok(self)
    }

    /// The `AppWindow.Id` value, which window-aware system calls (such as
    /// Windows Hello's `...ForWindowAsync`) take as their owner window.
    pub fn id(&self) -> Result<u64> {
        match self.ui.core.tree.native(self.node) {
            Payload::Window(window) => Ok(window.app_window.Id()?.value),
            _ => unreachable!("window keys hold windows"),
        }
    }

    /// The WinUI `Window`, for what this crate does not cover. Cast it with
    /// your own WinUI bindings.
    pub fn window(&self) -> IInspectable {
        match self.ui.core.tree.native(self.node) {
            Payload::Window(window) => window.window.clone().into(),
            _ => unreachable!("window keys hold windows"),
        }
    }
}

/// Activates the never-shown `window` once XAML has committed its first
/// frame, so the DWM open animation draws the content instead of the bare
/// window background. Returns at once; the show happens on a later render
/// tick.
pub(crate) fn reveal(window: &Window) -> Result<()> {
    // XAML renders a hidden window too, and `Rendering` fires before each
    // frame, so by the `REVEAL_TICK`th tick the first frame is committed.
    // DWM animates the first show only, so showing earlier under a cloak and
    // uncloaking later would lose the animation.
    let ticks = Cell::new(0);
    // The handler drops its own revoker on the reveal tick, which ends the
    // subscription and frees the cycle through this cell.
    let revoker = Rc::new(Cell::new(None));
    let handler = {
        let window = window.clone();
        let revoker = Rc::clone(&revoker);
        move |_: Ref<'_, IInspectable>, _: Ref<'_, IInspectable>| {
            ticks.set(ticks.get() + 1);
            if ticks.get() != REVEAL_TICK {
                return;
            }
            drop(revoker.take());
            let shown = focus_first(&window).and_then(|()| window.Activate());
            if let Err(error) = shown {
                // A window closed before its reveal fails here, and needs no
                // showing.
                tracing::debug!(%error, "a window did not show");
            }
        }
    };
    revoker.set(Some(CompositionTarget::Rendering(handler)?));
    Ok(())
}

/// Focuses the first control in `window`'s content, unless a control there
/// has focus already. Without it, a window activated after its content
/// loaded shows a keyboard focus ring on that control.
fn focus_first(window: &Window) -> Result<()> {
    let Ok(content) = window.Content() else {
        return Ok(());
    };
    // A window with no focus yet has no focused element, which WinUI returns
    // as null, an error here.
    if FocusManager::GetFocusedElementWithRoot(&content.XamlRoot()?).is_ok() {
        return Ok(());
    }
    if let Ok(first) = FocusManager::FindFirstFocusableElement(&content) {
        // `Programmatic` draws the ring too, because WinUI then draws it
        // when the last input came from the keyboard. `Pointer` never does,
        // and the next Tab still moves focus with a ring.
        first.cast::<UIElement>()?.Focus(FocusState::Pointer)?;
    }
    Ok(())
}

/// The interface behind `Window` that gives its `HWND`, from
/// `microsoft.ui.xaml.window.h`, which no metadata describes.
#[windows_core::interface("EECDBF0E-BAE9-4CB6-A68E-9598E1CB57BB")]
unsafe trait IWindowNative: windows_core::IUnknown {
    fn window_handle(&self, hwnd: *mut *mut core::ffi::c_void) -> HRESULT;
}

/// The render tick on which [`reveal`] shows a window.
const REVEAL_TICK: u32 = 2;

/// How many resizes a window's first fit may take before it shows anyway.
const FIT_TRIES: u32 = 3;

/// A fully transparent system composition brush.
fn clear_brush() -> Result<CompositionBrush> {
    // A system `Compositor` needs a system dispatcher queue on this thread.
    thread_local!(static QUEUED: Cell<bool> = const { Cell::new(false) });
    if !QUEUED.get() {
        let options = DispatcherQueueOptions {
            size: size_of::<DispatcherQueueOptions>() as u32,
            thread_type: DQTYPE_THREAD_CURRENT,
            apartment_type: DQTAT_COM_NONE,
        };
        let mut controller = std::ptr::null_mut();
        // SAFETY: `options` and `controller` outlive the call.
        unsafe { CreateDispatcherQueueController(options, &mut controller).ok()? };
        // The raw controller is never released, so the queue lives as long
        // as the thread.
        QUEUED.set(true);
    }
    let brush = Compositor::new()?.CreateColorBrushWithColor(Color {
        a: 0,
        r: 0,
        g: 0,
        b: 0,
    })?;
    brush.cast()
}

#[repr(C)]
struct DispatcherQueueOptions {
    size: u32,
    thread_type: i32,
    apartment_type: i32,
}
const DQTYPE_THREAD_CURRENT: i32 = 2;
const DQTAT_COM_NONE: i32 = 0;

windows_core::link!("coremessaging.dll" "system" fn CreateDispatcherQueueController(options: DispatcherQueueOptions, controller: *mut *mut core::ffi::c_void) -> HRESULT);

/// The `HWND` behind `window`.
fn hwnd(window: &Window) -> Result<*mut core::ffi::c_void> {
    let native: IWindowNative = window.cast()?;
    let mut hwnd = std::ptr::null_mut();
    // SAFETY: `hwnd` is written by the call.
    unsafe { native.window_handle(&mut hwnd).ok()? };
    Ok(hwnd)
}

/// Sets a 32-bit DWM window attribute on `hwnd`.
fn set_dwm_attribute(hwnd: *mut core::ffi::c_void, attribute: u32, value: u32) -> Result<()> {
    // SAFETY: `value` outlives the call, and its size is passed.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            (&raw const value).cast(),
            size_of::<u32>() as u32,
        )
        .ok()
    }
}

const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
const DWMSBT_MAINWINDOW: u32 = 2;

windows_core::link!("dwmapi.dll" "system" fn DwmSetWindowAttribute(hwnd: *mut core::ffi::c_void, attribute: u32, value: *const core::ffi::c_void, size: u32) -> HRESULT);
windows_core::link!("dwmapi.dll" "system" fn DwmExtendFrameIntoClientArea(hwnd: *mut core::ffi::c_void, margins: *const i32) -> HRESULT);
windows_core::link!("user32.dll" "system" fn GetWindowLongPtrW(hwnd: *mut core::ffi::c_void, index: i32) -> isize);
windows_core::link!("user32.dll" "system" fn SetWindowLongPtrW(hwnd: *mut core::ffi::c_void, index: i32, value: isize) -> isize);
windows_core::link!("user32.dll" "system" fn SetWindowPos(hwnd: *mut core::ffi::c_void, after: *mut core::ffi::c_void, x: i32, y: i32, width: i32, height: i32, flags: u32) -> windows_core::BOOL);
const GWL_STYLE: i32 = -16;
const WS_THICKFRAME: isize = 0x0004_0000;
const WS_MINIMIZEBOX: isize = 0x0002_0000;
const WS_MAXIMIZEBOX: isize = 0x0001_0000;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_FRAMECHANGED: u32 = 0x0020;
windows_core::link!("user32.dll" "system" fn GetDpiForWindow(hwnd: *mut core::ffi::c_void) -> u32);
