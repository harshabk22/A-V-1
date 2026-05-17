//! UiActionBridge implementation for the Tauri layer.
//!
//! Implements `moat_core::orchestrator::UiActionBridge` using the stored
//! `ScrapedElement` coordinates and `enigo` for physical mouse/keyboard input.

use crate::ui_scraper::ScrapedElement;
use enigo::{Enigo, Key, KeyboardControllable, MouseButton, MouseControllable};
use moat_core::orchestrator::UiActionBridge;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Bridge between the moat_core orchestrator and the Tauri UI layer.
/// Holds a reference to the scraped UI elements for coordinate-based interaction.
pub struct TauriUiActionBridge {
    elements: Arc<Mutex<Vec<ScrapedElement>>>,
}

impl TauriUiActionBridge {
    pub fn new(elements: Arc<Mutex<Vec<ScrapedElement>>>) -> Self {
        Self { elements }
    }

    fn find_element(&self, element_id: usize) -> Result<ScrapedElement, String> {
        let elements = self
            .elements
            .lock()
            .map_err(|_| "Failed to lock UI element map".to_string())?;

        if elements.is_empty() {
            return Err(
                "No UI elements available. The accessibility tree has not been scraped yet. \
                 The Visual-Semantic Driver only captures UI on hotkey press."
                    .to_string(),
            );
        }

        elements
            .iter()
            .find(|e| e.id == element_id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "Element ID {} not found in the current UI tree. \
                     Valid IDs: 1-{}. The UI may have changed since the last scrape.",
                    element_id,
                    elements.len()
                )
            })
    }
}

impl UiActionBridge for TauriUiActionBridge {
    fn click(&self, element_id: usize) -> Result<String, String> {
        let element = self.find_element(element_id)?;

        if element.center_x <= 0.0 && element.center_y <= 0.0 {
            return Err(format!(
                "Element ID {} ({} \"{}\") has no valid screen coordinates.",
                element.id, element.role, element.label
            ));
        }

        // Use enigo to physically click at the element's center coordinates
        let mut enigo = Enigo::new();
        enigo.mouse_move_to(element.center_x as i32, element.center_y as i32);
        thread::sleep(Duration::from_millis(30));
        enigo.mouse_click(MouseButton::Left);
        thread::sleep(Duration::from_millis(50));

        let display_label = if element.label.is_empty() {
            element.role.clone()
        } else {
            format!("{} \"{}\"", element.role, element.label)
        };

        Ok(format!(
            "Clicked element ID {} ({}) at ({:.0}, {:.0})",
            element.id, display_label, element.center_x, element.center_y
        ))
    }

    fn type_text(&self, element_id: usize, text: &str) -> Result<String, String> {
        let element = self.find_element(element_id)?;

        if element.center_x <= 0.0 && element.center_y <= 0.0 {
            return Err(format!(
                "Element ID {} ({} \"{}\") has no valid screen coordinates.",
                element.id, element.role, element.label
            ));
        }

        // First click the element to focus it
        let mut enigo = Enigo::new();
        enigo.mouse_move_to(element.center_x as i32, element.center_y as i32);
        thread::sleep(Duration::from_millis(30));
        enigo.mouse_click(MouseButton::Left);
        thread::sleep(Duration::from_millis(80));

        // Select all existing text (Cmd+A on macOS) then type the new text
        #[cfg(target_os = "macos")]
        {
            enigo.key_down(Key::Meta);
            enigo.key_click(Key::Layout('a'));
            enigo.key_up(Key::Meta);
            thread::sleep(Duration::from_millis(30));
        }
        #[cfg(not(target_os = "macos"))]
        {
            enigo.key_down(Key::Control);
            enigo.key_click(Key::Layout('a'));
            enigo.key_up(Key::Control);
            thread::sleep(Duration::from_millis(30));
        }

        // Type the text character by character
        enigo.key_sequence(text);
        thread::sleep(Duration::from_millis(50));

        let display_label = if element.label.is_empty() {
            element.role.clone()
        } else {
            format!("{} \"{}\"", element.role, element.label)
        };

        Ok(format!(
            "Typed {} chars into element ID {} ({}) at ({:.0}, {:.0})",
            text.len(),
            element.id,
            display_label,
            element.center_x,
            element.center_y
        ))
    }

    fn refresh_ui_tree(&self) -> Result<String, String> {
        thread::sleep(Duration::from_millis(300));

        let result = crate::ui_scraper::scrape_active_window_ui()
            .ok_or_else(|| "Accessibility scrape returned no elements after UI action.".to_string())?;

        let mut elements = self
            .elements
            .lock()
            .map_err(|_| "Failed to lock UI element map for refresh".to_string())?;
        *elements = result.elements;

        Ok(result.ui_tree_text)
    }
}
