//! Spike: how many plaintext copies of a typed password reach this process's
//! memory through a windows-reactor `PasswordBox`.
//!
//! Type `PROBE_SECRET` into the box (the probe script does this with
//! SendKeys). Every two seconds the app scans its own committed memory for the
//! secret as UTF-8 (Rust-side copies) and UTF-16 (WinUI and HSTRING copies)
//! and prints the counts. Freed memory is scanned too, because freeing does
//! not wipe it.
//!
//! `SPIKE_MODE`:
//! - `none`: the box has no `on_password_changed`.
//! - `length`: the callback maps the password to its length at once.
//! - `state`: the callback stores the password in component state.
//!
//! Set `SPIKE_WIPE` to install an allocator that zeroes every block on free.
//! The Submit button takes the password out of state and resets the box.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use std::{
    alloc::{GlobalAlloc, Layout, System},
    time::Duration,
};

/// Zeroes every block before it returns to the system allocator, when
/// `SPIKE_WIPE` is set. Read once at the first free.
struct WipingAllocator;

static WIPE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn wipe_enabled() -> bool {
    // `var_os` allocates, so the first call may recurse into `dealloc`
    // before the lock is set. `get` returns `None` then, and that free is
    // not wiped, which only affects blocks freed before typing starts.
    match WIPE.get() {
        Some(enabled) => *enabled,
        None => {
            if let Ok(guard) = WIPE_INIT.try_lock() {
                let enabled = std::env::var_os("SPIKE_WIPE").is_some();
                let _ = WIPE.set(enabled);
                drop(guard);
                enabled
            } else {
                false
            }
        }
    }
}

static WIPE_INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Zeroes `size` bytes at `pointer` in a way the optimizer keeps.
///
/// # Safety
/// `pointer` must be valid for writes of `size` bytes.
unsafe fn wipe(pointer: *mut u8, size: usize) {
    // SAFETY: the caller guarantees `size` writable bytes.
    unsafe { std::ptr::write_bytes(pointer, 0, size) };
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

unsafe impl GlobalAlloc for WipingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if wipe_enabled() {
            // SAFETY: the block is live and `layout.size()` bytes long.
            unsafe { wipe(pointer, layout.size()) };
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // An in-place system realloc could free the old block unwiped, so
        // move by hand: allocate, copy, then wipe and free the old block.
        let new_layout = Layout::from_size_align(new_size, layout.align())
            .expect("realloc keeps a valid alignment");
        // SAFETY: `new_layout` has a non-zero size because the caller's does.
        let new_pointer = unsafe { self.alloc(new_layout) };
        if !new_pointer.is_null() {
            // SAFETY: both blocks are valid for the smaller of the sizes and
            // do not overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(pointer, new_pointer, layout.size().min(new_size));
                self.dealloc(pointer, layout);
            }
        }
        new_pointer
    }
}

#[global_allocator]
static GLOBAL: WipingAllocator = WipingAllocator;
use windows_reactor::*;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, PAGE_READWRITE, VirtualQuery,
};

/// Stored reversed, so the binary's own data holds no forward copy.
const PROBE_SECRET_REVERSED: &str = "9rekraMx7qZ";

#[derive(Clone, Copy, PartialEq)]
enum ProbeMode {
    None,
    Length,
    State,
    /// A `TextBox` in place of the `PasswordBox`, to check the input path.
    Text,
}

#[derive(Clone)]
enum ProbeMessage {
    Length(usize),
    Password(String),
    /// Takes the password out of state, as an unlock would.
    Submit,
    Scan,
}

struct Probe {
    mode: ProbeMode,
    length: usize,
    /// Only used in `ProbeMode::State`.
    password: String,
    password_box: ElementRef<PasswordBox>,
    text_box: ElementRef<TextBox>,
    typed: bool,
    scans: u32,
}

/// Counts non-overlapping matches of `needle` in committed read-write memory,
/// skipping the bytes of `needle` itself.
fn count_in_memory(needle: &[u8]) -> usize {
    let mut count = 0;
    let mut address = 0usize;
    loop {
        // SAFETY: zeroed is a valid MEMORY_BASIC_INFORMATION.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid out pointer of the stated size.
        let written = unsafe {
            VirtualQuery(
                address as *const _,
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if written == 0 {
            return count;
        }
        let base = info.BaseAddress as usize;
        let size = info.RegionSize;
        let readable = info.State == MEM_COMMIT
            && info.Protect & PAGE_READWRITE != 0
            && info.Protect & (PAGE_GUARD | PAGE_NOACCESS) == 0;
        if readable {
            // SAFETY: the region is committed and read-write, and nothing
            // frees it while this single-threaded scan runs on the UI thread.
            let bytes = unsafe { std::slice::from_raw_parts(base as *const u8, size) };
            let needle_start = needle.as_ptr() as usize;
            count += bytes
                .windows(needle.len())
                .enumerate()
                .filter(|(offset, window)| *window == needle && base + offset != needle_start)
                .count();
        }
        match base.checked_add(size) {
            Some(next) => address = next,
            None => return count,
        }
    }
}

/// Types the secret into the focused control as Unicode key events.
fn type_secret() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput,
    };
    // The secret is ASCII, so each char is one UTF-16 unit.
    for unit in PROBE_SECRET_REVERSED
        .chars()
        .rev()
        .map(|character| character as u16)
    {
        let inputs = [0, KEYEVENTF_KEYUP].map(|flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0,
                    wScan: unit,
                    dwFlags: KEYEVENTF_UNICODE | flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
        // SAFETY: `inputs` is a valid array of INPUT of the stated size.
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<INPUT>() as i32,
            )
        };
    }
}

/// Scans at 15 s and 30 s. A scan blocks the UI thread for seconds, so the
/// window must stay free for typing in between.
fn scan_later(context: &ComponentContext<Probe>) {
    context.spawn_background(|_cancel| {
        std::thread::sleep(Duration::from_secs(15));
        ProbeMessage::Scan
    });
}

impl Component for Probe {
    type Input = ();
    type Message = ProbeMessage;

    fn create(_input: &(), context: &ComponentContext<Self>) -> Self {
        scan_later(context);
        Self {
            mode: match std::env::var("SPIKE_MODE").as_deref() {
                Ok("length") => ProbeMode::Length,
                Ok("state") => ProbeMode::State,
                Ok("text") => ProbeMode::Text,
                _ => ProbeMode::None,
            },
            length: 0,
            password: String::new(),
            password_box: ElementRef::new(),
            text_box: ElementRef::new(),
            typed: false,
            scans: 0,
        }
    }

    fn update(&mut self, message: ProbeMessage, context: &ComponentContext<Self>) {
        match message {
            ProbeMessage::Length(length) => self.length = length,
            ProbeMessage::Password(password) => {
                self.length = password.len();
                self.password = password;
            }
            ProbeMessage::Submit => {
                // A real unlock would pass this to the SDK. Dropping it here
                // frees it, and the wiping allocator zeroes it.
                let password = std::mem::take(&mut self.password);
                eprintln!("submitted {} bytes", password.len());
                self.length = 0;
            }
            ProbeMessage::Scan => {
                if !self.typed && std::env::var_os("SPIKE_AUTOTYPE").is_some() {
                    // False only means focus could not be queued, and the
                    // length printed below shows whether typing landed.
                    let _ = self.password_box.request_focus();
                    let _ = self.text_box.request_focus();
                    self.typed = true;
                    // Type off the UI thread after focus has moved, so the
                    // key events go through the normal input queue.
                    std::thread::spawn(|| {
                        std::thread::sleep(Duration::from_millis(500));
                        type_secret();
                    });
                }
                let mut utf8: Vec<u8> = PROBE_SECRET_REVERSED.bytes().rev().collect();
                let mut utf16: Vec<u8> = PROBE_SECRET_REVERSED
                    .chars()
                    .rev()
                    .flat_map(|character| (character as u16).to_le_bytes())
                    .collect();
                let counts = (count_in_memory(&utf8), count_in_memory(&utf16));
                // Wipe the needles, or their freed buffers count as copies
                // in the next scan.
                for byte in utf8.iter_mut().chain(utf16.iter_mut()) {
                    // SAFETY: `byte` is a valid, aligned, exclusive reference.
                    unsafe { std::ptr::write_volatile(byte, 0) };
                }
                eprintln!(
                    "length {}: UTF-8 copies {}, UTF-16 copies {}",
                    self.length, counts.0, counts.1,
                );
                self.scans += 1;
                if self.scans < 2 {
                    scan_later(context);
                }
            }
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        context.window_title("Password probe");
        let password_box = PasswordBox::new()
            .placeholder_text("Password")
            .element_ref(&self.password_box);
        let input: View = match self.mode {
            ProbeMode::None => password_box.into(),
            ProbeMode::Length => password_box
                .on_password_changed(
                    context.callback(|password: String| ProbeMessage::Length(password.len())),
                )
                .into(),
            // An unset value means "reset to default" in Reactor, so the box
            // must be controlled or it clears what the user typed.
            ProbeMode::State => password_box
                .password(self.password.clone())
                .on_password_changed(context.callback(ProbeMessage::Password))
                .into(),
            ProbeMode::Text => TextBox::new()
                .element_ref(&self.text_box)
                .on_text_changed(context.callback(ProbeMessage::Password))
                .into(),
        };
        StackPanel::new().spacing(8.0).children((
            TextBlock::new().text(format!("Length: {}", self.length)),
            input,
            Button::new()
                .on_click(context.message(ProbeMessage::Submit))
                .content("Submit"),
        ))
    }
}

fn main() {
    App::run_component::<Probe>(()).expect("WinUI app failed to start");
}
