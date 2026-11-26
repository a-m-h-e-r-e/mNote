//! Summoning mNote from anywhere on macOS: the global summon and quick
//! capture keys (`⌥N` and `⌥M` unless config.toml says otherwise), a window
//! that joins every Space (full-screen apps' Spaces included), and today's
//! local date for quick capture's daily note.
//!
//! This is the one module allowed `unsafe` (see CLAUDE.md): the hotkey is
//! Carbon's `RegisterEventHotKey`, called directly; the window's Space
//! behaviour and visibility are set on its `NSWindow` through the
//! Objective-C runtime; the date comes from libc's `localtime_r`. None of
//! it needs Accessibility permission.
#![allow(unsafe_code)]
// objc 0.2's `msg_send!` checks `feature = "cargo-clippy"`, which this crate
// doesn't declare.
#![allow(unexpected_cfgs)]

use std::ffi::c_void;
use std::fmt;
use std::sync::{Mutex, OnceLock};

use gpui::Window;
use mnote_core::config::KeySpec;
use objc::runtime::Object;
use objc::{msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

type OsStatus = i32;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerProc = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OsStatus;

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerProc,
        num_types: usize,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OsStatus;
    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OsStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OsStatus;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OsStatus;
}

/// `kEventClassKeyboard` ('keyb') and `kEventHotKeyPressed`.
const KEYBOARD_CLASS: u32 = u32::from_be_bytes(*b"keyb");
const HOT_KEY_PRESSED: u32 = 5;
/// `kEventParamDirectObject` and `typeEventHotKeyID`: which hotkey fired.
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const HOT_KEY_ID_TYPE: u32 = u32::from_be_bytes(*b"hkid");
/// Our hotkeys' signature.
const SIGNATURE: u32 = u32::from_be_bytes(*b"mNte");
/// Carbon's `cmdKey`, `shiftKey`, `optionKey` and `controlKey`.
const COMMAND: u32 = 1 << 8;
const SHIFT: u32 = 1 << 9;
const OPTION: u32 = 1 << 11;
const CONTROL: u32 = 1 << 12;
/// `eventHotKeyExistsErr`: another app has the key.
const HOT_KEY_TAKEN: OsStatus = -9878;

/// mNote's global keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    /// `summon` (`⌥N`): bring the mNote window forward.
    Summon,
    /// `capture` (`⌥M`): open the quick capture panel.
    Capture,
}

impl Hotkey {
    const ALL: [Hotkey; 2] = [Hotkey::Summon, Hotkey::Capture];

    fn id(self) -> u32 {
        match self {
            Hotkey::Summon => 1,
            Hotkey::Capture => 2,
        }
    }

    /// The setting's name in config.toml.
    fn setting(self) -> &'static str {
        match self {
            Hotkey::Summon => "summon",
            Hotkey::Capture => "capture",
        }
    }
}

/// The ANSI virtual key code for a config key (`n`, `;`, `space`).
fn key_code(key: &str) -> Option<u32> {
    let code = match key {
        "a" => 0x00,
        "s" => 0x01,
        "d" => 0x02,
        "f" => 0x03,
        "h" => 0x04,
        "g" => 0x05,
        "z" => 0x06,
        "x" => 0x07,
        "c" => 0x08,
        "v" => 0x09,
        "b" => 0x0B,
        "q" => 0x0C,
        "w" => 0x0D,
        "e" => 0x0E,
        "r" => 0x0F,
        "y" => 0x10,
        "t" => 0x11,
        "1" => 0x12,
        "2" => 0x13,
        "3" => 0x14,
        "4" => 0x15,
        "6" => 0x16,
        "5" => 0x17,
        "=" => 0x18,
        "9" => 0x19,
        "7" => 0x1A,
        "-" => 0x1B,
        "8" => 0x1C,
        "0" => 0x1D,
        "]" => 0x1E,
        "o" => 0x1F,
        "u" => 0x20,
        "[" => 0x21,
        "i" => 0x22,
        "p" => 0x23,
        "l" => 0x25,
        "j" => 0x26,
        "'" => 0x27,
        "k" => 0x28,
        ";" => 0x29,
        "\\" => 0x2A,
        "," => 0x2B,
        "/" => 0x2C,
        "n" => 0x2D,
        "m" => 0x2E,
        "." => 0x2F,
        "space" => 0x31,
        "`" => 0x32,
        _ => return None,
    };
    Some(code)
}

fn modifiers(spec: &KeySpec) -> u32 {
    [
        (spec.command, COMMAND),
        (spec.shift, SHIFT),
        (spec.option, OPTION),
        (spec.control, CONTROL),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .fold(0, |all, (_, bit)| all | bit)
}

/// Where hotkey presses go. Bounded: presses while the app is busy collapse.
static PRESSES: OnceLock<async_channel::Sender<Hotkey>> = OnceLock::new();
/// The registered keys (as addresses), so a config change can unregister
/// them.
static REGISTERED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Why a hotkey couldn't be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyError {
    /// Another app already has the key.
    Taken(String),
    /// A key mNote can't register (not on the ANSI layout's map).
    Unknown(String),
    /// The keys were registered once already.
    AlreadyRegistered,
    /// Carbon returned this status.
    Os(String, i32),
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HotkeyError::Taken(key) => write!(f, "another app is using {key}"),
            HotkeyError::Unknown(key) => write!(f, "{key} can't be a global key"),
            HotkeyError::AlreadyRegistered => write!(f, "the global keys are already registered"),
            HotkeyError::Os(key, status) => write!(f, "macOS refused {key} (status {status})"),
        }
    }
}

impl std::error::Error for HotkeyError {}

extern "C" fn on_hot_key(_: EventHandlerCallRef, event: EventRef, _: *mut c_void) -> OsStatus {
    let mut id = EventHotKeyId {
        signature: 0,
        id: 0,
    };
    // SAFETY: reads the pressed hotkey's ID from the live event Carbon
    // handed us into `id`, a local of exactly the requested size.
    let status = unsafe {
        GetEventParameter(
            event,
            DIRECT_OBJECT,
            HOT_KEY_ID_TYPE,
            std::ptr::null_mut(),
            std::mem::size_of::<EventHotKeyId>(),
            std::ptr::null_mut(),
            (&raw mut id).cast(),
        )
    };
    let key = Hotkey::ALL
        .into_iter()
        .find(|k| status == 0 && id.signature == SIGNATURE && id.id == k.id());
    if let (Some(key), Some(presses)) = (key, PRESSES.get()) {
        // Full means presses are already on their way.
        let _ = presses.try_send(key);
    }
    0
}

/// Starts listening for global keys and registers `summon` and `capture`.
/// Presses arrive on the returned channel, on the main thread's run loop; a
/// key another app holds is reported and skipped. Call once, after the app
/// starts.
pub fn register_hotkeys(
    summon: &KeySpec,
    capture: &KeySpec,
) -> Result<(async_channel::Receiver<Hotkey>, Vec<HotkeyError>), HotkeyError> {
    let (send, receive) = async_channel::bounded(2);
    PRESSES
        .set(send)
        .map_err(|_| HotkeyError::AlreadyRegistered)?;
    let spec = EventTypeSpec {
        event_class: KEYBOARD_CLASS,
        event_kind: HOT_KEY_PRESSED,
    };
    let mut handler: EventHandlerRef = std::ptr::null_mut();
    // SAFETY: plain Carbon calls with valid pointers to locals; `spec` is
    // read during the call only. The handler is a `'static` function that
    // touches only the `PRESSES` static, and stays installed for the life of
    // the app, so nothing dangles.
    let status = unsafe {
        InstallEventHandler(
            GetApplicationEventTarget(),
            on_hot_key,
            1,
            &spec,
            std::ptr::null_mut(),
            &mut handler,
        )
    };
    if status != 0 {
        return Err(HotkeyError::Os("the global keys".into(), status));
    }
    Ok((receive, set_hotkeys(summon, capture)))
}

/// Replaces the global keys (after config.toml changed). Returns the keys
/// that couldn't be registered.
pub fn set_hotkeys(summon: &KeySpec, capture: &KeySpec) -> Vec<HotkeyError> {
    let Ok(mut registered) = REGISTERED.lock() else {
        return vec![HotkeyError::AlreadyRegistered];
    };
    for hot_key in registered.drain(..) {
        // SAFETY: each address is a ref RegisterEventHotKey gave us and that
        // hasn't been unregistered yet (we drain it from the list).
        unsafe { UnregisterEventHotKey(hot_key as EventHotKeyRef) };
    }
    let mut failed = Vec::new();
    for (key, spec) in [(Hotkey::Summon, summon), (Hotkey::Capture, capture)] {
        let label = format!("{} ({})", spec.label(), key.setting());
        let Some(code) = key_code(&spec.key) else {
            failed.push(HotkeyError::Unknown(label));
            continue;
        };
        let id = EventHotKeyId {
            signature: SIGNATURE,
            id: key.id(),
        };
        let mut hot_key: EventHotKeyRef = std::ptr::null_mut();
        // SAFETY: Carbon fills `hot_key`, a local; the target is the app's
        // own, alive for the life of the app.
        let status = unsafe {
            RegisterEventHotKey(
                code,
                modifiers(spec),
                id,
                GetApplicationEventTarget(),
                0,
                &mut hot_key,
            )
        };
        match status {
            0 => registered.push(hot_key as usize),
            HOT_KEY_TAKEN => failed.push(HotkeyError::Taken(label)),
            status => failed.push(HotkeyError::Os(label, status)),
        }
    }
    failed
}

/// libc's `struct tm` on macOS.
#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: std::ffi::c_long,
    tm_zone: *const std::ffi::c_char,
}

unsafe extern "C" {
    fn localtime_r(time: *const i64, out: *mut Tm) -> *mut Tm;
}

/// Today's date in the local time zone, as (year, month, day).
pub fn today() -> (i32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let now = i64::try_from(secs).unwrap_or(0);
    let mut tm = Tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    };
    // SAFETY: `localtime_r` is the thread-safe form: it reads `now` and
    // fills `tm`, both locals that outlive the call, and returns null on
    // failure (checked). `tm_zone` is never read.
    let filled = unsafe { localtime_r(&now, &mut tm) };
    if filled.is_null() {
        return mnote_core::capture::utc_date(secs);
    }
    (
        tm.tm_year + 1900,
        u32::try_from(tm.tm_mon + 1).unwrap_or(1),
        u32::try_from(tm.tm_mday).unwrap_or(1),
    )
}

/// The NSWindow behind a gpui window, or `None` if it has none (yet).
fn ns_window(window: &Window) -> Option<*mut Object> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    let view = appkit.ns_view.as_ptr().cast::<Object>();
    // SAFETY: `ns_view` is gpui's live NSView for this window, valid while
    // we hold `window`; `-window` returns its NSWindow or nil. Main thread.
    let ns_window: *mut Object = unsafe { msg_send![view, window] };
    (!ns_window.is_null()).then_some(ns_window)
}

/// `NSWindowCollectionBehaviorCanJoinAllSpaces | …FullScreenAuxiliary`.
const ALL_SPACES: usize = 1 << 0;
const FULL_SCREEN_AUXILIARY: usize = 1 << 8;

/// Puts `window` on every Space, full-screen apps' Spaces included, so a
/// summon shows it where you are instead of switching Spaces.
pub fn join_all_spaces(window: &Window) {
    let Some(ns_window) = ns_window(window) else {
        return;
    };
    // SAFETY: a live NSWindow (see `ns_window`); `-setCollectionBehavior:`
    // takes an NSUInteger. Main thread.
    unsafe {
        let _: () = msg_send![ns_window, setCollectionBehavior: ALL_SPACES | FULL_SCREEN_AUXILIARY];
    }
}

/// Shows or hides `window`'s close, minimize and zoom buttons (ADR-0015:
/// hidden, the tab rows reach the window's top edge).
pub fn set_window_buttons(window: &Window, visible: bool) {
    let Some(ns_window) = ns_window(window) else {
        return;
    };
    let hidden = if visible {
        objc::runtime::NO
    } else {
        objc::runtime::YES
    };
    // NSWindowCloseButton, NSWindowMiniaturizeButton, NSWindowZoomButton.
    for kind in 0usize..3 {
        // SAFETY: a live NSWindow (see `ns_window`);
        // `-standardWindowButton:` takes an NSWindowButton (NSUInteger) and
        // returns an NSButton or nil. Main thread.
        let button: *mut Object = unsafe { msg_send![ns_window, standardWindowButton: kind] };
        if !button.is_null() {
            // SAFETY: a live NSButton; `-setHidden:` takes a BOOL.
            unsafe {
                let _: () = msg_send![button, setHidden: hidden];
            }
        }
    }
}

/// Takes `window` off the screen until it's next activated (a summon), so
/// quick capture shows alone over the app you were in.
pub fn order_out(window: &Window) {
    let Some(ns_window) = ns_window(window) else {
        return;
    };
    // SAFETY: a live NSWindow (see `ns_window`); `-orderOut:` takes a
    // sender, nil here. Main thread.
    unsafe {
        let _: () = msg_send![ns_window, orderOut: std::ptr::null_mut::<Object>()];
    }
}
