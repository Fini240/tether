//! Raw CoreGraphics / CoreFoundation bindings.
//!
//! Hand-written rather than pulled from a wrapper crate: this is a small, very
//! stable slice of the C ABI, and owning it means the input path cannot break
//! under someone else's semver bump. Every symbol used anywhere in the macOS
//! backend is declared here and nowhere else.

#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::c_void;

pub type CGEventRef = *mut c_void;
pub type CGEventSourceRef = *mut c_void;
pub type CFMachPortRef = *mut c_void;
pub type CFRunLoopSourceRef = *mut c_void;
pub type CFRunLoopRef = *mut c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CGDirectDisplayID = u32;
pub type CGDisplayModeRef = *mut c_void;
pub type CGError = i32;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

pub type CGEventTapCallBack = extern "C" fn(
    proxy: *mut c_void,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

// ---- CGEventType ----
pub const kCGEventNull: u32 = 0;
pub const kCGEventLeftMouseDown: u32 = 1;
pub const kCGEventLeftMouseUp: u32 = 2;
pub const kCGEventRightMouseDown: u32 = 3;
pub const kCGEventRightMouseUp: u32 = 4;
pub const kCGEventMouseMoved: u32 = 5;
pub const kCGEventLeftMouseDragged: u32 = 6;
pub const kCGEventRightMouseDragged: u32 = 7;
pub const kCGEventKeyDown: u32 = 10;
pub const kCGEventKeyUp: u32 = 11;
pub const kCGEventFlagsChanged: u32 = 12;
pub const kCGEventScrollWheel: u32 = 22;
pub const kCGEventOtherMouseDown: u32 = 25;
pub const kCGEventOtherMouseUp: u32 = 26;
pub const kCGEventOtherMouseDragged: u32 = 27;
/// The tap was switched off because our callback was too slow. Recoverable:
/// call `CGEventTapEnable` again. Not handling this is the classic "input
/// sharing stops working after a while" bug.
pub const kCGEventTapDisabledByTimeout: u32 = 0xFFFF_FFFE;
pub const kCGEventTapDisabledByUserInput: u32 = 0xFFFF_FFFF;

// ---- CGMouseButton ----
pub const kCGMouseButtonLeft: u32 = 0;
pub const kCGMouseButtonRight: u32 = 1;
pub const kCGMouseButtonCenter: u32 = 2;

// ---- CGEventFlags ----
pub const kCGEventFlagMaskAlphaShift: u64 = 0x0001_0000;
pub const kCGEventFlagMaskShift: u64 = 0x0002_0000;
pub const kCGEventFlagMaskControl: u64 = 0x0004_0000;
pub const kCGEventFlagMaskAlternate: u64 = 0x0008_0000;
pub const kCGEventFlagMaskCommand: u64 = 0x0010_0000;
pub const kCGEventFlagMaskNumericPad: u64 = 0x0020_0000;
pub const kCGEventFlagMaskSecondaryFn: u64 = 0x0080_0000;

// ---- CGEventField ----
pub const kCGMouseEventButtonNumber: u32 = 3;
pub const kCGMouseEventDeltaX: u32 = 4;
pub const kCGMouseEventDeltaY: u32 = 5;
pub const kCGKeyboardEventAutorepeat: u32 = 8;
pub const kCGKeyboardEventKeycode: u32 = 9;
pub const kCGScrollWheelEventDeltaAxis1: u32 = 11;
pub const kCGScrollWheelEventDeltaAxis2: u32 = 12;
/// Free-form 64 bits that ride along with an event and survive a round trip
/// through an event tap. Used to label our own injections.
pub const kCGEventSourceUserData: u32 = 42;

// ---- CGEventTapLocation / Placement / Options ----
pub const kCGHIDEventTap: u32 = 0;
pub const kCGSessionEventTap: u32 = 1;
pub const kCGHeadInsertEventTap: u32 = 0;
pub const kCGEventTapOptionDefault: u32 = 0;
pub const kCGEventTapOptionListenOnly: u32 = 1;

// ---- CGScrollEventUnit ----
pub const kCGScrollEventUnitPixel: u32 = 0;
pub const kCGScrollEventUnitLine: u32 = 1;

// ---- CGEventSourceStateID ----
pub const kCGEventSourceStateHIDSystemState: i32 = 1;

pub const fn event_mask(event_type: u32) -> u64 {
    1u64 << event_type
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        mouseType: u32,
        mouseCursorPosition: CGPoint,
        mouseButton: u32,
    ) -> CGEventRef;

    pub fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtualKey: u16,
        keyDown: bool,
    ) -> CGEventRef;

    /// Variadic in C: the wheel deltas follow `wheelCount`. Axis 1 is vertical,
    /// axis 2 horizontal.
    pub fn CGEventCreateScrollWheelEvent(
        source: CGEventSourceRef,
        units: u32,
        wheelCount: u32,
        ...
    ) -> CGEventRef;

    pub fn CGEventPost(tap: u32, event: CGEventRef);
    pub fn CGEventSetFlags(event: CGEventRef, flags: u64);
    pub fn CGEventGetFlags(event: CGEventRef) -> u64;
    pub fn CGEventGetType(event: CGEventRef) -> u32;
    pub fn CGEventSetType(event: CGEventRef, eventType: u32);
    pub fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    pub fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    pub fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    pub fn CGEventGetDoubleValueField(event: CGEventRef, field: u32) -> f64;

    pub fn CGEventSourceCreate(stateID: i32) -> CGEventSourceRef;

    pub fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        eventsOfInterest: u64,
        callback: CGEventTapCallBack,
        userInfo: *mut c_void,
    ) -> CFMachPortRef;
    pub fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);

    pub fn CGWarpMouseCursorPosition(newCursorPosition: CGPoint) -> CGError;
    /// Re-couples the hardware mouse to the cursor after a warp. A warp
    /// silently decouples them for ~250 ms, which eats the first deltas after
    /// every screen switch if you do not call this.
    pub fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> CGError;

    pub fn CGDisplayHideCursor(display: CGDirectDisplayID) -> CGError;
    pub fn CGDisplayShowCursor(display: CGDirectDisplayID) -> CGError;
    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGGetActiveDisplayList(
        maxDisplays: u32,
        activeDisplays: *mut CGDirectDisplayID,
        displayCount: *mut u32,
    ) -> CGError;
    pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    pub fn CGDisplayPixelsWide(display: CGDirectDisplayID) -> usize;

    /// The current mode. `CGDisplayPixelsWide` is not a substitute: on a
    /// scaled Retina mode it reports logical points, so a 2x display looks
    /// like 1x. Only the mode knows the real backing-pixel count.
    pub fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> CGDisplayModeRef;
    pub fn CGDisplayModeGetWidth(mode: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeRelease(mode: CGDisplayModeRef);
    pub fn CGDisplayIsMain(display: CGDirectDisplayID) -> u32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFRunLoopCommonModes: CFStringRef;

    pub fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFRunLoopRun();
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRelease(cf: *const c_void);
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    /// Whether this process is in System Settings → Privacy & Security →
    /// Accessibility. Without it `CGEventTapCreate` returns NULL.
    pub fn AXIsProcessTrusted() -> bool;
}
