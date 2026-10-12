//! Native macOS overlay enhancements for the Command Bar window.
//!
//! Converts the standard Tauri WebviewWindow into a true Spotlight-style
//! NSPanel with non-activating behavior, floating level, cross-Space
//! visibility, drag-from-anywhere, and frontmost app restoration.
//!
//! Pattern: OpenJarvis NSPanel+WKWebView overlay (J1-J3).

#![allow(unexpected_cfgs)]

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod macos {
    #![allow(unexpected_cfgs)]
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::Mutex;
    use tauri::WebviewWindow;

    static FRONTMOST_APP: Mutex<Option<usize>> = Mutex::new(None);

    /// Apply full NSPanel properties to the command bar window.
    pub fn configure_overlay_panel(window: &WebviewWindow) {
        let ns_window = match window.ns_window() {
            Ok(w) => w as *mut Object,
            Err(_) => return,
        };

        unsafe {
            // Non-activating panel: doesn't steal focus from the frontmost app
            let style_mask: i32 = msg_send![ns_window, styleMask];
            let new_mask = style_mask | (1 << 7); // NSWindowStyleMaskNonActivatingPanel
            let _: () = msg_send![ns_window, setStyleMask: new_mask];

            // Floating window level: always on top
            let _: () = msg_send![ns_window, setLevel: 3i32]; // NSFloatingWindowLevel

            // Cross-Space: appears on all Spaces / fullscreen
            let behavior: u32 = (1 << 0) | (1 << 17); // canJoinAllSpaces | fullScreenAuxiliary
            let _: () = msg_send![ns_window, setCollectionBehavior: behavior];

            // Movable by window background: drag from anywhere
            let _: () = msg_send![ns_window, setMovableByWindowBackground: true];

            // Don't hide on deactivate
            let _: () = msg_send![ns_window, setHidesOnDeactivate: false];

            // Make key window for keyboard input
            let _: () = msg_send![ns_window, makeKeyAndOrderFront: std::ptr::null::<Object>()];
        }
    }

    /// Save the frontmost application PID before showing.
    pub fn save_frontmost_app() {
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            let front_app: *mut Object = msg_send![workspace, frontmostApplication];
            if !front_app.is_null() {
                let pid: i32 = msg_send![front_app, processIdentifier];
                let _ = FRONTMOST_APP.lock().map(|mut slot| *slot = Some(pid as usize));
            }
        }
    }

    /// Restore the previously frontmost app via its PID.
    pub fn restore_frontmost_app() {
        if let Ok(mut slot) = FRONTMOST_APP.lock() {
            if let Some(pid) = slot.take() {
                unsafe {
                    let running_app: *mut Object = msg_send![
                        class!(NSRunningApplication),
                        runningApplicationWithProcessIdentifier: pid as i32
                    ];
                    if !running_app.is_null() {
                        let _: bool = msg_send![running_app, activateWithOptions: 2u32];
                        // 2 = NSApplicationActivateAllWindows
                    }
                }
            }
        }
    }

    /// Check if overlay is the key window.
    #[allow(dead_code)]
    pub fn overlay_is_key(window: &WebviewWindow) -> bool {
        let ns_window = match window.ns_window() {
            Ok(w) => w as *mut Object,
            Err(_) => return false,
        };
        unsafe {
            let is_key: bool = msg_send![ns_window, isKeyWindow];
            is_key
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod macos {
    use tauri::WebviewWindow;

    pub fn configure_overlay_panel(_window: &WebviewWindow) {}
    pub fn save_frontmost_app() {}
    pub fn restore_frontmost_app() {}
    #[allow(dead_code)]
    pub fn overlay_is_key(_window: &WebviewWindow) -> bool { true }
}

pub use macos::*;
