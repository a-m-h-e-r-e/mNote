//! Summoning mNote from anywhere on macOS: the global `⌥M` hotkey and a
//! window that joins every Space, full-screen apps' Spaces included.
//!
//! This is the one module allowed `unsafe` (see CLAUDE.md): the hotkey is
//! Carbon's `RegisterEventHotKey`, called directly, and the window's Space
//! behaviour is set on its `NSWindow` through the Objective-C runtime. Both
//! need no Accessibility permission.
#![allow(unsafe_code)]
// objc 0.2's `msg_send!` checks `feature = "cargo-clippy"`, which this crate
// doesn't declare.
#![allow(unexpected_cfgs)]

use std::ffi::c_void;
use std::fmt;
use std::sync::OnceLock;

use gpui::Window;
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
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OsStatus;
}

/// `kEventClassKeyboard` ('keyb') and `kEventHotKeyPressed`.
const KEYBOARD_CLASS: u32 = u32::from_be_bytes(*b"keyb");
const HOT_KEY_PRESSED: u32 = 5;
/// `kVK_ANSI_M` and Carbon's `optionKey`.
const KEY_M: u32 = 0x2E;
const OPTION: u32 = 1 << 11;
/// `eventHotKeyExistsErr`: another app has the key.
const HOT_KEY_TAKEN: OsStatus = -9878;

/// Where hotkey presses go. Bounded to one: presses while the app is busy
/// collapse into one summon.
static PRESSES: OnceLock<async_channel::Sender<()>> = OnceLock::new();

/// Why the hotkey couldn't be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyError {
    /// Another app already has `⌥M`.
    Taken,
    /// It was registered once already.
    AlreadyRegistered,
    /// Carbon returned this status.
    Os(i32),
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HotkeyError::Taken => write!(f, "another app is using ⌥M"),
            HotkeyError::AlreadyRegistered => write!(f, "the summon key is already registered"),
            HotkeyError::Os(status) => write!(f, "macOS refused the summon key (status {status})"),
        }
    }
}

impl std::error::Error for HotkeyError {}

extern "C" fn on_hot_key(_: EventHandlerCallRef, _: EventRef, _: *mut c_void) -> OsStatus {
    if let Some(presses) = PRESSES.get() {
        // Full means a summon is already on its way.
        let _ = presses.try_send(());
    }
    0
}

/// Registers `⌥M` for the whole system. Each press arrives on the returned
/// channel, on the main thread's run loop. Call once, after the app starts.
pub fn register_hotkey() -> Result<async_channel::Receiver<()>, HotkeyError> {
    let (send, receive) = async_channel::bounded(1);
    PRESSES
        .set(send)
        .map_err(|_| HotkeyError::AlreadyRegistered)?;
    let spec = EventTypeSpec {
        event_class: KEYBOARD_CLASS,
        event_kind: HOT_KEY_PRESSED,
    };
    let id = EventHotKeyId {
        signature: u32::from_be_bytes(*b"mNte"),
        id: 1,
    };
    let mut handler: EventHandlerRef = std::ptr::null_mut();
    let mut hot_key: EventHotKeyRef = std::ptr::null_mut();
    // SAFETY: plain Carbon calls with valid pointers to locals; `spec` is
    // read during the call only. The handler is a `'static` function that
    // touches only the `PRESSES` static, and the refs are kept for the life
    // of the app (never unregistered), so nothing dangles.
    let status = unsafe {
        let target = GetApplicationEventTarget();
        let status = InstallEventHandler(
            target,
            on_hot_key,
            1,
            &spec,
            std::ptr::null_mut(),
            &mut handler,
        );
        if status != 0 {
            status
        } else {
            RegisterEventHotKey(KEY_M, OPTION, id, target, 0, &mut hot_key)
        }
    };
    match status {
        0 => Ok(receive),
        HOT_KEY_TAKEN => Err(HotkeyError::Taken),
        status => Err(HotkeyError::Os(status)),
    }
}

/// `NSWindowCollectionBehaviorCanJoinAllSpaces | …FullScreenAuxiliary`.
const ALL_SPACES: usize = 1 << 0;
const FULL_SCREEN_AUXILIARY: usize = 1 << 8;

/// Puts `window` on every Space, full-screen apps' Spaces included, so a
/// summon shows it where you are instead of switching Spaces.
pub fn join_all_spaces(window: &Window) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let view = appkit.ns_view.as_ptr().cast::<Object>();
    // SAFETY: `ns_view` is gpui's live NSView for this window, valid while
    // we hold `window`. `-window` returns its NSWindow (or nil, which we
    // check), and `-setCollectionBehavior:` takes an NSUInteger. Both run on
    // the main thread, where gpui calls us.
    unsafe {
        let ns_window: *mut Object = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, setCollectionBehavior: ALL_SPACES | FULL_SCREEN_AUXILIARY];
    }
}
