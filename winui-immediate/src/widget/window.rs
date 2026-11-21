use crate::{
    bindings::{
        FrameworkElement, HorizontalAlignment, IWindow2, MicaBackdrop, OverlappedPresenter, Size,
        SizeInt32, VerticalAlignment, Window,
    },
    events::{EventKind, EventSet},
    pass::{ChildKind, Payload, Ui, WindowNode},
    tree::NodeId,
};
use std::hash::Hash;
use windows_core::{BOOL, HRESULT, IInspectable, Interface, Result};

impl Ui {
    /// Declares an open window. It is created on its first pass and activated
    /// once; a window a pass does not declare is closed. `body` must declare
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
                activated: false,
                mica: false,
                compact: false,
                extends: false,
                sized: false,
                fitted: (0, 0),
                fitted_content: None,
                fit: false,
                fitted_once: false,
                cloaked: false,
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
        let Payload::Window(window) = &mut self.tree.node_mut(node).payload else {
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
        let window = self.window_mut(node);
        if !window.activated {
            // Cloaked, so layout runs while nothing is drawn on screen; the
            // end of the pass uncloaks it.
            cloak(&window.window, true)?;
            window.window.Activate()?;
            window.activated = true;
            window.cloaked = true;
        }
        Ok(WindowResponse { ui: self, node })
    }

    fn window_mut(&mut self, node: NodeId) -> &mut WindowNode {
        match &mut self.tree.node_mut(node).payload {
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
            window
                .window
                .cast::<IWindow2>()?
                .SetSystemBackdrop(&MicaBackdrop::new()?)?;
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
            let presenter = window
                .app_window
                .Presenter()?
                .cast::<OverlappedPresenter>()?;
            presenter.SetIsMaximizable(false)?;
            presenter.SetIsMinimizable(false)?;
            presenter.SetIsResizable(false)?;
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
            let native: IWindowNative = window.window.cast()?;
            let mut hwnd = std::ptr::null_mut();
            // SAFETY: `hwnd` is written by the call.
            let dpi = unsafe {
                native.window_handle(&mut hwnd).ok()?;
                GetDpiForWindow(hwnd)
            };
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
        match &self.ui.tree.node(self.node).payload {
            Payload::Window(window) => Ok(window.app_window.Id()?.value),
            _ => unreachable!("window keys hold windows"),
        }
    }

    /// The WinUI `Window`, for what this crate does not cover. Cast it with
    /// your own WinUI bindings.
    pub fn window(&self) -> IInspectable {
        match &self.ui.tree.node(self.node).payload {
            Payload::Window(window) => window.window.clone().into(),
            _ => unreachable!("window keys hold windows"),
        }
    }
}

/// Hides `window` from the screen while it stays shown, active and laid
/// out, or shows it again.
pub(crate) fn cloak(window: &Window, on: bool) -> Result<()> {
    let native: IWindowNative = window.cast()?;
    let mut hwnd = std::ptr::null_mut();
    let value = BOOL::from(on);
    // SAFETY: `hwnd` is written by the call; `value` is a live BOOL of the
    // size passed.
    unsafe {
        native.window_handle(&mut hwnd).ok()?;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_CLOAK,
            (&raw const value).cast(),
            size_of::<BOOL>() as u32,
        )
        .ok()
    }
}

/// The interface behind `Window` that gives its `HWND`, from
/// `microsoft.ui.xaml.window.h`, which no metadata describes.
#[windows_core::interface("EECDBF0E-BAE9-4CB6-A68E-9598E1CB57BB")]
unsafe trait IWindowNative: windows_core::IUnknown {
    fn window_handle(&self, hwnd: *mut *mut core::ffi::c_void) -> HRESULT;
}

const DWMWA_CLOAK: u32 = 13;

/// How many resizes a window's first fit may take before it shows anyway.
const FIT_TRIES: u32 = 3;

windows_core::link!("user32.dll" "system" fn GetDpiForWindow(hwnd: *mut core::ffi::c_void) -> u32);
windows_core::link!("dwmapi.dll" "system" fn DwmSetWindowAttribute(hwnd: *mut core::ffi::c_void, attribute: u32, value: *const core::ffi::c_void, size: u32) -> HRESULT);
