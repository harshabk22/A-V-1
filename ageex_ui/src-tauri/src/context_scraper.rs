use active_win_pos_rs::get_active_window;
use arboard::Clipboard;
use enigo::{Enigo, Key, KeyboardControllable};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

const SCRAPED_TEXT_CHAR_CAP: usize = 4_000;
const SCRAPED_TEXT_TRUNCATION_SUFFIX: &str = "...[TRUNCATED BY SYSTEM LIMIT]";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPayload {
    pub active_app_name: String,
    pub active_window_title: String,
    pub selected_text: Option<String>,
    /// Flat accessibility tree of the active window for the Visual-Semantic Driver.
    pub ui_tree: Option<String>,
}

enum ClipboardBackup {
    Text(String),
    Html(String),
    Image(arboard::ImageData<'static>),
    FileList(Vec<PathBuf>),
    Empty,
}

fn read_clipboard_backup(clipboard: &mut Clipboard) -> Result<ClipboardBackup, String> {
    if let Ok(files) = clipboard.get().file_list() {
        return Ok(ClipboardBackup::FileList(files));
    }
    if let Ok(image) = clipboard.get_image() {
        return Ok(ClipboardBackup::Image(image));
    }
    if let Ok(html) = clipboard.get().html() {
        return Ok(ClipboardBackup::Html(html));
    }
    if let Ok(text) = clipboard.get_text() {
        return Ok(ClipboardBackup::Text(text));
    }
    Ok(ClipboardBackup::Empty)
}

fn restore_clipboard_backup(
    clipboard: &mut Clipboard,
    backup: ClipboardBackup,
) -> Result<(), String> {
    match backup {
        ClipboardBackup::Text(text) => clipboard
            .set_text(text)
            .map_err(|err| format!("Clipboard restore failed: {err}")),
        ClipboardBackup::Html(html) => clipboard
            .set_html(html, None)
            .map_err(|err| format!("Clipboard restore failed: {err}")),
        ClipboardBackup::Image(image) => clipboard
            .set_image(image)
            .map_err(|err| format!("Clipboard restore failed: {err}")),
        ClipboardBackup::FileList(files) => clipboard
            .set()
            .file_list(&files)
            .map_err(|err| format!("Clipboard restore failed: {err}")),
        ClipboardBackup::Empty => clipboard
            .clear()
            .map_err(|err| format!("Clipboard clear failed: {err}")),
    }
}

fn cap_scraped_text(text: String) -> String {
    let char_count = text.chars().count();
    if char_count <= SCRAPED_TEXT_CHAR_CAP {
        return text;
    }

    let capped: String = text.chars().take(SCRAPED_TEXT_CHAR_CAP).collect();
    format!("{capped}{SCRAPED_TEXT_TRUNCATION_SUFFIX}")
}

fn scrape_selected_text() -> Option<String> {
    let mut clipboard = Clipboard::new().ok()?;
    let backup = read_clipboard_backup(&mut clipboard).ok()?;

    let _ = clipboard.clear();

    let mut enigo = Enigo::new();
    #[cfg(target_os = "macos")]
    {
        enigo.key_down(Key::Meta);
        enigo.key_click(Key::Layout('c'));
        enigo.key_up(Key::Meta);
    }
    #[cfg(not(target_os = "macos"))]
    {
        enigo.key_down(Key::Control);
        enigo.key_click(Key::Layout('c'));
        enigo.key_up(Key::Control);
    }

    thread::sleep(Duration::from_millis(50));

    let scraped = clipboard
        .get_text()
        .ok()
        .map(|text| cap_scraped_text(text.trim().to_string()))
        .filter(|text| !text.is_empty());

    let _ = restore_clipboard_backup(&mut clipboard, backup);

    scraped
}

pub fn scrape_os_context() -> (ContextPayload, Option<Vec<crate::ui_scraper::ScrapedElement>>) {
    let (active_app_name, active_window_title) = match get_active_window() {
        Ok(window) => (
            window.app_name.trim().to_string(),
            window.title.trim().to_string(),
        ),
        Err(_) => (String::new(), String::new()),
    };

    let selected_text = scrape_selected_text();

    // Scrape the accessibility tree for the Visual-Semantic Driver
    let (ui_tree, scraped_elements) = match crate::ui_scraper::scrape_active_window_ui() {
        Some(result) => (Some(result.ui_tree_text), Some(result.elements)),
        None => (None, None),
    };

    (
        ContextPayload {
            active_app_name,
            active_window_title,
            selected_text,
            ui_tree,
        },
        scraped_elements,
    )
}
