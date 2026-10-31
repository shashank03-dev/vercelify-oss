//! A virtual display for an extended desktop (macOS 11+). CoreGraphics'
//! `CGVirtualDisplay` (the API DeskPad and BetterDisplay use) adds a monitor
//! that exists while this process holds it: apps can be dragged onto it like
//! any screen, and ffmpeg captures it for the server's panel. The classes
//! aren't in Apple's public headers, so they're looked up at run time and a
//! Mac without them says so instead of crashing.

use objc2::encode::{Encode, Encoding};
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::{NSArray, NSSize, NSString};
use std::ffi::{c_void, CStr};

/// `dispatch_queue_t`: an Objective-C object, passed as one.
#[repr(transparent)]
struct Queue(*mut c_void);

// SAFETY: a dispatch queue is an Objective-C object pointer.
unsafe impl Encode for Queue {
    const ENCODING: Encoding = Encoding::Object;
}

extern "C" {
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
}

/// A display that is on screen until this is dropped.
pub struct VirtualDisplay {
    // Held, not used: releasing it takes the display away.
    _display: Retained<AnyObject>,
}

fn class(name: &CStr) -> Result<&'static AnyClass, String> {
    AnyClass::get(name).ok_or_else(|| "this Mac can't make a virtual display (it needs macOS 11 or later)".into())
}

/// Make a `width`×`height` display at `hz`. Call on the main thread.
pub fn create(name: &str, width: u32, height: u32, hz: f64) -> Result<VirtualDisplay, String> {
    let (descriptor_class, display_class) = (class(c"CGVirtualDisplayDescriptor")?, class(c"CGVirtualDisplay")?);
    let (settings_class, mode_class) = (class(c"CGVirtualDisplaySettings")?, class(c"CGVirtualDisplayMode")?);
    // SAFETY: the selectors and their types are CGVirtualDisplay's since
    // macOS 11 (unsigned int sizes and ids, CGSize millimetres, a dispatch
    // queue); the classes were checked above.
    unsafe {
        let d: Retained<AnyObject> = msg_send![descriptor_class, new];
        let _: () = msg_send![&d, setQueue: Queue(dispatch_get_global_queue(0, 0))];
        let _: () = msg_send![&d, setName: &*NSString::from_str(name)];
        let _: () = msg_send![&d, setMaxPixelsWide: width];
        let _: () = msg_send![&d, setMaxPixelsHigh: height];
        // A 15.6" laptop panel: macOS derives the display's DPI from this.
        let _: () = msg_send![&d, setSizeInMillimeters: NSSize::new(344.0, 194.0)];
        let _: () = msg_send![&d, setVendorID: 0x5646u32];
        let _: () = msg_send![&d, setProductID: 0x0001u32];
        let _: () = msg_send![&d, setSerialNum: 0x0001u32];
        let a: Allocated<AnyObject> = msg_send![display_class, alloc];
        let display: Option<Retained<AnyObject>> = msg_send![a, initWithDescriptor: &*d];
        let display = display.ok_or("macOS refused to make a virtual display")?;
        let a: Allocated<AnyObject> = msg_send![mode_class, alloc];
        let mode: Option<Retained<AnyObject>> = msg_send![a, initWithWidth: width, height: height, refreshRate: hz];
        let mode = mode.ok_or("macOS refused the virtual display's mode")?;
        let settings: Retained<AnyObject> = msg_send![settings_class, new];
        let _: () = msg_send![&settings, setHiDPI: 0u32];
        let _: () = msg_send![&settings, setModes: &*NSArray::from_retained_slice(&[mode])];
        let applied: bool = msg_send![&display, applySettings: &*settings];
        if !applied {
            return Err(format!("macOS refused a {width}×{height} virtual display"));
        }
        Ok(VirtualDisplay { _display: display })
    }
}

// SAFETY: CGVirtualDisplay is made and released from any thread (the
// display's own callbacks run on the dispatch queue it was given); the app
// keeps it behind a mutex and only creates and drops it.
unsafe impl Send for VirtualDisplay {}

#[cfg(test)]
mod tests {
    use super::*;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGGetActiveDisplayList(max: u32, ids: *mut u32, count: *mut u32) -> i32;
    }

    fn active() -> Vec<u32> {
        let mut ids = [0u32; 16];
        let mut n = 0;
        // SAFETY: a buffer of 16 ids and its length.
        unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut n) };
        ids[..n as usize].to_vec()
    }

    // Adds a real display to this Mac for a few seconds:
    // cargo test -p vercelify-cockpit vdisplay -- --ignored
    #[test]
    #[ignore]
    fn a_virtual_display_appears_and_goes() {
        let screens = || crate::local::capture_screens().unwrap();
        let (before, displays) = (screens(), active());
        let d = create("Vercelify test", 1280, 720, 60.0).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert_eq!(active().len(), displays.len() + 1, "not among the active displays");
        let added = vercelify_core::avfoundation::added_screen(&before, &screens());
        assert!(added.is_some(), "ffmpeg sees no new screen");
        drop(d);
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert_eq!(active(), displays, "still there after drop");
    }
}
