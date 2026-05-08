//! moat_watcher — Window Attention Registry Daemon
//!
//! Runs outside the Tauri process. Listens for macOS focus changes,
//! proactively scrapes the AX tree of the focused app, and serves the
//! cached tree over a Unix Domain Socket at `/tmp/moat_watcher.sock`.
//!
//! **Self-Exclusion Rule:** If the newly focused app is "MoatAgeex" or
//! "ageex_ui", the event is ignored and the previous target stays pinned.

use accessibility::{AXUIElement, AXAttribute};
use active_win_pos_rs::get_active_window;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};

// ─────────────────────────────────────────────────────────────────────────────
// § Data Types
// ─────────────────────────────────────────────────────────────────────────────

/// A scraped UI element — mirrors the Tauri layer's ScrapedElement for
/// deserialization compatibility across the UDS boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapedElement {
    pub id: usize,
    pub role: String,
    pub label: String,
    pub value: String,
    pub center_x: f64,
    pub center_y: f64,
    pub width: f64,
    pub height: f64,
}

/// The cached UI tree that the daemon serves to clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedUiTree {
    pub ui_tree_text: String,
    pub elements: Vec<ScrapedElement>,
    pub app_name: String,
    pub element_count: usize,
    pub timestamp_ms: u64,
}

impl Default for CachedUiTree {
    fn default() -> Self {
        Self {
            ui_tree_text: String::new(),
            elements: Vec::new(),
            app_name: String::new(),
            element_count: 0,
            timestamp_ms: 0,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// § Constants
// ─────────────────────────────────────────────────────────────────────────────

const SOCKET_PATH: &str = "/tmp/moat_watcher.sock";
const POLL_INTERVAL_MS: u64 = 500;
const MAX_ELEMENTS: usize = 200;
const MAX_DEPTH: usize = 30;
const UI_TREE_CHAR_CAP: usize = 6_000;

/// App names to ignore (self-exclusion rule).
const EXCLUDED_APPS: &[&str] = &[
    "MoatAgeex",
    "ageex_ui",
    "moatageex",
    "Ageex",
];

/// Roles considered actionable for OS automation.
const ACTIONABLE_ROLES: &[&str] = &[
    "AXButton",
    "AXTextField",
    "AXTextArea",
    "AXLink",
    "AXMenuItem",
    "AXCheckBox",
    "AXRadioButton",
    "AXPopUpButton",
    "AXComboBox",
    "AXSlider",
    "AXTab",
    "AXTabGroup",
    "AXToolbar",
    "AXMenuBar",
    "AXMenu",
    "AXStaticText",
    "AXImage",
    "AXGroup",
    "AXScrollArea",
    "AXTable",
    "AXRow",
    "AXCell",
    "AXWebArea",
    "AXHeading",
    "AXList",
    "AXOutline",
    "AXDisclosureTriangle",
    "AXSwitch",
];

// ─────────────────────────────────────────────────────────────────────────────
// § Self-Exclusion Filter
// ─────────────────────────────────────────────────────────────────────────────

fn should_ignore_app(app_name: &str) -> bool {
    let lower = app_name.to_lowercase();
    EXCLUDED_APPS
        .iter()
        .any(|excluded| lower == excluded.to_lowercase())
}

// ─────────────────────────────────────────────────────────────────────────────
// § AX Tree Scraper (self-contained — mirrors ageex_ui/src-tauri/src/ui_scraper.rs)
// ─────────────────────────────────────────────────────────────────────────────

fn get_ax_string_attr(element: &AXUIElement, attr: &str) -> String {
    element
        .attribute(&AXAttribute::new(&CFString::new(attr)))
        .ok()
        .and_then(|val| val.downcast_into::<CFString>())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn get_ax_position(element: &AXUIElement) -> (f64, f64) {
    #[allow(unused_assignments)]
    let (mut x, mut y) = (0.0, 0.0);
    match element.attribute(&AXAttribute::new(&CFString::new("AXPosition"))) {
        Ok(val) => {
            let ptr = val.as_CFTypeRef();
            let mut point = core_graphics_types::geometry::CGPoint::new(0.0, 0.0);
            let ok = unsafe {
                accessibility_sys::AXValueGetValue(
                    ptr as accessibility_sys::AXValueRef,
                    accessibility_sys::kAXValueTypeCGPoint,
                    &mut point as *mut _ as *mut std::ffi::c_void,
                )
            };
            if ok {
                x = point.x;
                y = point.y;
            } else {
                x = 0.0;
                y = 0.0;
            }
        }
        Err(_) => {
            x = 0.0;
            y = 0.0;
        }
    }
    (x, y)
}

fn get_ax_size(element: &AXUIElement) -> (f64, f64) {
    match element.attribute(&AXAttribute::new(&CFString::new("AXSize"))) {
        Ok(val) => {
            let ptr = val.as_CFTypeRef();
            let mut size = core_graphics_types::geometry::CGSize::new(0.0, 0.0);
            let ok = unsafe {
                accessibility_sys::AXValueGetValue(
                    ptr as accessibility_sys::AXValueRef,
                    accessibility_sys::kAXValueTypeCGSize,
                    &mut size as *mut _ as *mut std::ffi::c_void,
                )
            };
            if ok {
                (size.width, size.height)
            } else {
                (0.0, 0.0)
            }
        }
        Err(_) => (0.0, 0.0),
    }
}

fn scrape_element_tree(
    element: &AXUIElement,
    elements: &mut Vec<ScrapedElement>,
    tree_lines: &mut Vec<String>,
    depth: usize,
    counter: &mut usize,
    total_chars: &mut usize,
) {
    if depth > MAX_DEPTH || *counter >= MAX_ELEMENTS || *total_chars >= UI_TREE_CHAR_CAP {
        return;
    }

    let role = get_ax_string_attr(element, "AXRole");
    if role.is_empty() {
        return;
    }

    let is_actionable = ACTIONABLE_ROLES.iter().any(|r| *r == role);
    let label = get_ax_string_attr(element, "AXTitle");
    let value = get_ax_string_attr(element, "AXValue");
    let description = get_ax_string_attr(element, "AXDescription");

    let display_label = if !label.is_empty() {
        label.clone()
    } else if !description.is_empty() {
        description.clone()
    } else {
        String::new()
    };

    let display_value: String = value.chars().take(80).collect();

    if is_actionable && (!display_label.is_empty() || !display_value.is_empty()) {
        let (px, py) = get_ax_position(element);
        let (w, h) = get_ax_size(element);
        let cx = px + w / 2.0;
        let cy = py + h / 2.0;

        let id = *counter;
        *counter += 1;

        let indent = "  ".repeat(depth);
        let line = if display_value.is_empty() {
            format!("{indent}[{id}] {role} \"{display_label}\"")
        } else {
            format!("{indent}[{id}] {role} \"{display_label}\" value=\"{display_value}\"")
        };
        *total_chars += line.len();
        tree_lines.push(line);

        elements.push(ScrapedElement {
            id,
            role: role.clone(),
            label: display_label,
            value: display_value,
            center_x: cx,
            center_y: cy,
            width: w,
            height: h,
        });
    }

    // Recurse into children
    if let Ok(children_val) = element.attribute(&AXAttribute::new(&CFString::new("AXChildren"))) {
        if let Some(children_array) = children_val.downcast_into::<core_foundation::array::CFArray>() {
            for i in 0..children_array.len() {
                if *counter >= MAX_ELEMENTS || *total_chars >= UI_TREE_CHAR_CAP {
                    break;
                }
                if let Some(child_ref) = children_array.get(i) {
                    let raw_ref = *child_ref as accessibility_sys::AXUIElementRef;
                    // Safety: child_ref is a valid AXUIElementRef obtained from CFArray.
                    // wrap_under_get_rule increments the retain count so the element
                    // stays alive for the duration of our recursive call.
                    let child_element: AXUIElement = unsafe {
                        TCFType::wrap_under_get_rule(raw_ref)
                    };
                    scrape_element_tree(
                        &child_element,
                        elements,
                        tree_lines,
                        depth + 1,
                        counter,
                        total_chars,
                    );
                }
            }
        }
    }
}

/// Scrape the frontmost window's AX tree. Returns None if no window is focused
/// or the scrape fails.
fn scrape_active_window() -> Option<CachedUiTree> {
    let window = get_active_window().ok()?;
    let app_name = window.app_name.clone();

    if should_ignore_app(&app_name) {
        return None; // Self-exclusion: don't scrape our own UI
    }

    let pid = window.process_id as i32;
    let app_element = AXUIElement::application(pid);

    // Get the focused window element
    let focused_window = app_element
        .attribute(&AXAttribute::new(&CFString::new("AXFocusedWindow")))
        .ok()?;
    let window_element = focused_window.downcast_into::<AXUIElement>()?;

    let mut elements = Vec::new();
    let mut tree_lines = Vec::new();
    let mut counter = 0usize;
    let mut total_chars = 0usize;

    // Add header line
    let header = format!("App: {} (pid {})", app_name, pid);
    tree_lines.push(header);

    scrape_element_tree(
        &window_element,
        &mut elements,
        &mut tree_lines,
        0,
        &mut counter,
        &mut total_chars,
    );

    let element_count = elements.len();
    let ui_tree_text = tree_lines.join("\n");

    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    Some(CachedUiTree {
        ui_tree_text,
        elements,
        app_name,
        element_count,
        timestamp_ms,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// § Focus Poller (replaces AXObserver for simplicity + thread safety)
// ─────────────────────────────────────────────────────────────────────────────

/// Polls for focus changes and rescans the AX tree when the active app changes.
/// This is more robust than AXObserver because:
/// 1. No run loop thread management required
/// 2. Works across all apps without per-PID observer registration
/// 3. Self-exclusion is trivially enforced
async fn focus_poller(cache: Arc<RwLock<CachedUiTree>>) {
    let mut last_app = String::new();
    let mut ticker = interval(Duration::from_millis(POLL_INTERVAL_MS));

    eprintln!("[moat_watcher] Focus poller started (poll interval: {}ms)", POLL_INTERVAL_MS);

    loop {
        ticker.tick().await;

        let current_app = match get_active_window() {
            Ok(w) => w.app_name,
            Err(_) => continue,
        };

        // Self-exclusion: if the focused app is MoatAgeex, keep existing cache
        if should_ignore_app(&current_app) {
            continue;
        }

        // Only rescrape if the active app changed
        if current_app != last_app {
            last_app = current_app.clone();
            eprintln!("[moat_watcher] Focus changed to: {}", last_app);

            // Scrape in a blocking task to avoid starving the async runtime
            let cache_clone = Arc::clone(&cache);
            tokio::task::spawn_blocking(move || {
                if let Some(tree) = scrape_active_window() {
                    eprintln!(
                        "[moat_watcher] Scraped {} elements from '{}'",
                        tree.element_count, tree.app_name
                    );
                    // Update cache (blocking lock is fine here — no contention)
                    if let Ok(mut guard) = cache_clone.try_write() {
                        *guard = tree;
                    }
                }
            })
            .await
            .ok();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// § Unix Domain Socket Server
// ─────────────────────────────────────────────────────────────────────────────

async fn serve_socket(cache: Arc<RwLock<CachedUiTree>>) {
    // Remove stale socket file if it exists
    let _ = std::fs::remove_file(SOCKET_PATH);

    let listener = match UnixListener::bind(SOCKET_PATH) {
        Ok(l) => l,
        Err(err) => {
            eprintln!("[moat_watcher] FATAL: Failed to bind {}: {}", SOCKET_PATH, err);
            return;
        }
    };

    eprintln!("[moat_watcher] UDS server listening on {}", SOCKET_PATH);

    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(err) => {
                eprintln!("[moat_watcher] Accept error: {}", err);
                continue;
            }
        };

        let cache_ref = Arc::clone(&cache);
        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut buf_reader = BufReader::new(reader);
            let mut line = String::new();

            match buf_reader.read_line(&mut line).await {
                Ok(0) => return, // Client disconnected
                Ok(_) => {
                    let cmd = line.trim().to_uppercase();
                    if cmd == "SCRAPE" {
                        let cached = cache_ref.read().await;
                        let json = serde_json::to_string(&*cached).unwrap_or_else(|_| {
                            r#"{"error":"serialization failed"}"#.to_string()
                        });
                        let _ = writer.write_all(json.as_bytes()).await;
                        let _ = writer.write_all(b"\n").await;
                    } else if cmd == "PING" {
                        let _ = writer.write_all(b"PONG\n").await;
                    } else if cmd == "RESCRAPE" {
                        // Force an immediate rescrape regardless of focus change
                        let cache_for_scrape = Arc::clone(&cache_ref);
                        let result = tokio::task::spawn_blocking(move || {
                            scrape_active_window()
                        })
                        .await;
                        if let Ok(Some(tree)) = result {
                            let mut guard = cache_for_scrape.write().await;
                            *guard = tree;
                        }
                        let cached = cache_ref.read().await;
                        let json = serde_json::to_string(&*cached).unwrap_or_else(|_| {
                            r#"{"error":"serialization failed"}"#.to_string()
                        });
                        let _ = writer.write_all(json.as_bytes()).await;
                        let _ = writer.write_all(b"\n").await;
                    } else {
                        let _ = writer
                            .write_all(
                                b"{\"error\":\"Unknown command. Use SCRAPE, RESCRAPE, or PING.\"}\n",
                            )
                            .await;
                    }
                }
                Err(err) => {
                    eprintln!("[moat_watcher] Read error: {}", err);
                }
            }
        });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// § Main
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    eprintln!("============================================");
    eprintln!(" moat_watcher — Window Attention Registry");
    eprintln!(" Socket: {}", SOCKET_PATH);
    eprintln!("============================================");

    let cache: Arc<RwLock<CachedUiTree>> = Arc::new(RwLock::new(CachedUiTree::default()));

    // Do an initial scrape before starting the server
    {
        let c = Arc::clone(&cache);
        let _ = tokio::task::spawn_blocking(move || {
            if let Some(tree) = scrape_active_window() {
                eprintln!(
                    "[moat_watcher] Initial scrape: {} elements from '{}'",
                    tree.element_count, tree.app_name
                );
                let mut guard = c.blocking_write();
                *guard = tree;
            }
        })
        .await;
    }

    // Run poller and socket server concurrently
    tokio::select! {
        _ = focus_poller(Arc::clone(&cache)) => {
            eprintln!("[moat_watcher] Focus poller exited unexpectedly");
        }
        _ = serve_socket(Arc::clone(&cache)) => {
            eprintln!("[moat_watcher] Socket server exited unexpectedly");
        }
    }
}
