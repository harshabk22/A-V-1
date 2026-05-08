//! Visual-Semantic Driver: macOS Accessibility Tree Scraper
//!
//! Traverses the frontmost application's accessibility tree and produces
//! a flat, indexed list of actionable UI elements for the LLM context,
//! plus a coordinate map for `os:click` and `os:type_text` execution.

use accessibility::{AXUIElement, AXUIElementAttributes, TreeVisitor, TreeWalker, TreeWalkerFlow};
use active_win_pos_rs::get_active_window;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};

/// Extract the screen position and size of an AX element using raw FFI.
/// Returns (center_x, center_y, width, height).
fn extract_element_geometry(element: &AXUIElement) -> (f64, f64, f64, f64) {
    use accessibility_sys::{
        kAXPositionAttribute, kAXSizeAttribute,
        AXUIElementCopyAttributeValue, AXValueGetValue,
    };
    use core_foundation::base::CFTypeRef;
    use core_graphics_types::geometry::{CGPoint, CGSize};
    use std::mem::MaybeUninit;
    use std::os::raw::c_void;

    unsafe {
        // Get position
        let pos_key = CFString::from_static_string(kAXPositionAttribute);
        let mut pos_ref: CFTypeRef = std::ptr::null();
        let pos_err = AXUIElementCopyAttributeValue(
            element.as_concrete_TypeRef(),
            pos_key.as_concrete_TypeRef(),
            &mut pos_ref,
        );

        // Get size  
        let size_key = CFString::from_static_string(kAXSizeAttribute);
        let mut size_ref: CFTypeRef = std::ptr::null();
        let size_err = AXUIElementCopyAttributeValue(
            element.as_concrete_TypeRef(),
            size_key.as_concrete_TypeRef(),
            &mut size_ref,
        );

        if pos_err != 0 || size_err != 0 || pos_ref.is_null() || size_ref.is_null() {
            return (0.0, 0.0, 0.0, 0.0);
        }

        // Extract CGPoint from AXValue
        let mut point = MaybeUninit::<CGPoint>::uninit();
        let got_point = AXValueGetValue(
            pos_ref as _,
            1, // kAXValueCGPointType
            point.as_mut_ptr() as *mut c_void,
        );

        // Extract CGSize from AXValue
        let mut size = MaybeUninit::<CGSize>::uninit();
        let got_size = AXValueGetValue(
            size_ref as _,
            2, // kAXValueCGSizeType
            size.as_mut_ptr() as *mut c_void,
        );

        // Release CF refs
        core_foundation::base::CFRelease(pos_ref);
        core_foundation::base::CFRelease(size_ref);

        if !got_point || !got_size {
            return (0.0, 0.0, 0.0, 0.0);
        }

        let p = point.assume_init();
        let s = size.assume_init();

        (
            p.x + s.width / 2.0,
            p.y + s.height / 2.0,
            s.width,
            s.height,
        )
    }
}

/// Maximum number of elements to include in a single scrape.
const MAX_ELEMENTS: usize = 200;
/// Maximum depth to traverse in the accessibility tree.
const MAX_DEPTH: usize = 30;
/// Maximum character length for the flat UI tree string.
const UI_TREE_CHAR_CAP: usize = 6_000;

/// A scraped UI element with enough data for both LLM context and physical interaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapedElement {
    pub id: usize,
    pub role: String,
    pub label: String,
    pub value: String,
    /// Center X coordinate (screen-space).
    pub center_x: f64,
    /// Center Y coordinate (screen-space).
    pub center_y: f64,
    pub width: f64,
    pub height: f64,
}

/// Result of a UI scrape operation.
pub struct UiScrapeResult {
    /// Flat text representation for LLM context injection.
    pub ui_tree_text: String,
    /// Indexed element map for `os:click` / `os:type_text` execution.
    pub elements: Vec<ScrapedElement>,
    /// The app name that was scraped (for safety checks).
    #[allow(dead_code)]
    pub app_name: String,
}

/// Roles we consider "actionable" and worth showing to the LLM.
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
    "AXMenuButton",
    "AXIncrementor",
    "AXSearchField",
];

/// Roles for static text — included for context but not actionable.
const STATIC_ROLES: &[&str] = &["AXStaticText", "AXHeading"];

/// Convert an AX role string to a human-readable short name.
fn role_display_name(role: &str) -> &str {
    match role {
        "AXButton" => "Button",
        "AXTextField" | "AXSearchField" => "TextField",
        "AXTextArea" => "TextArea",
        "AXLink" => "Link",
        "AXMenuItem" => "MenuItem",
        "AXCheckBox" => "CheckBox",
        "AXRadioButton" => "RadioButton",
        "AXPopUpButton" => "PopUpButton",
        "AXComboBox" => "ComboBox",
        "AXSlider" => "Slider",
        "AXTab" => "Tab",
        "AXTabGroup" => "TabGroup",
        "AXToolbar" => "Toolbar",
        "AXMenuButton" => "MenuButton",
        "AXIncrementor" => "Stepper",
        "AXStaticText" => "Text",
        "AXHeading" => "Heading",
        other => other,
    }
}

/// Tree visitor that collects actionable UI elements.
struct UiCollector {
    elements: RefCell<Vec<ScrapedElement>>,
    next_id: Cell<usize>,
    depth: Cell<usize>,
}

impl UiCollector {
    fn new() -> Self {
        Self {
            elements: RefCell::new(Vec::new()),
            next_id: Cell::new(1),
            depth: Cell::new(0),
        }
    }
}

impl TreeVisitor for UiCollector {
    fn enter_element(&self, element: &AXUIElement) -> TreeWalkerFlow {
        self.depth.set(self.depth.get() + 1);

        // Bail early if we've collected enough or gone too deep
        if self.elements.borrow().len() >= MAX_ELEMENTS || self.depth.get() > MAX_DEPTH {
            return TreeWalkerFlow::SkipSubtree;
        }

        // Get the role
        let role = match element.role() {
            Ok(r) => r.to_string(),
            Err(_) => return TreeWalkerFlow::Continue,
        };

        let is_actionable = ACTIONABLE_ROLES.iter().any(|r| *r == role);
        let is_static = STATIC_ROLES.iter().any(|r| *r == role);

        if !is_actionable && !is_static {
            return TreeWalkerFlow::Continue;
        }

        // Get label (title, then description, then placeholder)
        let label = element
            .title()
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                element
                    .description()
                    .ok()
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| {
                element
                    .placeholder_value()
                    .ok()
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_default();

        // Get current value (for text fields)
        let value = element
            .value()
            .ok()
            .and_then(|v| {
                // Try to extract as string
                use core_foundation::base::TCFType;
                if v.instance_of::<CFString>() {
                    unsafe {
                        let cf_str = CFString::wrap_under_get_rule(
                            v.as_concrete_TypeRef() as core_foundation::string::CFStringRef,
                        );
                        Some(cf_str.to_string())
                    }
                } else {
                    None
                }
            })
            .unwrap_or_default();

        // Skip elements with no label and no value (invisible/decorative)
        if label.is_empty() && value.is_empty() && is_static {
            return TreeWalkerFlow::Continue;
        }

        // Get position and size for coordinate-based interaction
        let (center_x, center_y, width, height) = extract_element_geometry(element);

        let id = self.next_id.get();
        self.next_id.set(id + 1);

        self.elements.borrow_mut().push(ScrapedElement {
            id,
            role: role.clone(),
            label: label.chars().take(120).collect(),
            value: value.chars().take(200).collect(),
            center_x,
            center_y,
            width,
            height,
        });

        TreeWalkerFlow::Continue
    }

    fn exit_element(&self, _element: &AXUIElement) {
        self.depth.set(self.depth.get().saturating_sub(1));
    }
}

/// Scrape the active macOS window's accessibility tree.
///
/// Returns a flat text representation for the LLM and a coordinate map
/// for physical interaction. Returns `None` if accessibility is unavailable.
pub fn scrape_active_window_ui() -> Option<UiScrapeResult> {
    // Get the frontmost application
    let active_window = get_active_window().ok()?;
    let app_name = active_window.app_name.trim().to_string();
    let pid = active_window.process_id as i32;

    // Create the AX application element
    let app_element = AXUIElement::application(pid);

    // Get the focused window
    let window = app_element.focused_window().ok().unwrap_or(app_element);

    // Walk the tree
    let collector = UiCollector::new();
    let walker = TreeWalker::new();
    walker.walk(&window, &collector);

    let elements = collector.elements.into_inner();

    if elements.is_empty() {
        return None;
    }

    // Build flat text representation
    let mut lines = Vec::with_capacity(elements.len());
    for elem in &elements {
        let display_role = role_display_name(&elem.role);
        let is_actionable = ACTIONABLE_ROLES.iter().any(|r| *r == elem.role);

        let line = if !elem.value.is_empty() && !elem.label.is_empty() {
            format!(
                "[ID: {}] [{}] \"{}\" = \"{}\"",
                elem.id, display_role, elem.label, elem.value
            )
        } else if !elem.label.is_empty() {
            format!("[ID: {}] [{}] \"{}\"", elem.id, display_role, elem.label)
        } else if !elem.value.is_empty() {
            format!("[ID: {}] [{}] = \"{}\"", elem.id, display_role, elem.value)
        } else {
            format!("[ID: {}] [{}]", elem.id, display_role)
        };

        // Mark non-actionable elements
        let line = if !is_actionable {
            format!("{line} (read-only)")
        } else {
            line
        };

        lines.push(line);
    }

    let mut ui_tree_text = lines.join("\n");

    // Cap the output
    if ui_tree_text.len() > UI_TREE_CHAR_CAP {
        ui_tree_text.truncate(UI_TREE_CHAR_CAP);
        if let Some(last_newline) = ui_tree_text.rfind('\n') {
            ui_tree_text.truncate(last_newline);
        }
        ui_tree_text.push_str("\n...[TRUNCATED BY SYSTEM LIMIT]");
    }

    Some(UiScrapeResult {
        ui_tree_text,
        elements,
        app_name,
    })
}
