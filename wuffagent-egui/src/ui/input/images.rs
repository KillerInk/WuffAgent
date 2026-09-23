//! Image attach/clipboard plumbing for the chat input: reading an image
//! off the system clipboard or from a file, storing it as the session's
//! pending image (re-encoded as PNG), and base64-encoding it for display.

use base64::Engine;
use eframe::egui;

use super::super::state::ChatApp;

impl ChatApp {
    /// Paste an image from the system clipboard into the selected session's
    /// pending-image slot. No-op when the clipboard holds no image (text is
    /// pasted by egui itself; an empty clipboard simply does nothing).
    pub(super) fn paste_image_from_clipboard(&mut self) {
        if self.selected_session_id.is_none() {
            return;
        }
        if let Some(rgba) = clipboard_image_pixels() {
            self.attach_rgba(rgba, "clipboard");
        }
    }

    /// "Attach image" button: try the system clipboard first (the screenshot
    /// flow), then fall back to a file picker for common image formats.
    pub(super) fn attach_image_from_clipboard_or_file(&mut self) {
        if self.selected_session_id.is_none() {
            return;
        }
        if let Some(rgba) = clipboard_image_pixels() {
            if self.attach_rgba(rgba, "clipboard") {
                return;
            }
        }
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif"])
            .pick_file()
        {
            match image_pixels_from_file(&path) {
                Some(rgba) => {
                    self.attach_rgba(rgba, "file");
                }
                None => self.notify_chat(
                    &format!(
                        "Could not read image file: {}",
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    ),
                    false,
                ),
            }
        }
    }

    /// Store RGBA8 pixels as the session's pending image (re-encoded as PNG).
    /// Replaces any previously attached image (one image per message).
    /// Returns true on success.
    fn attach_rgba(&mut self, rgba: (u32, u32, Vec<u8>), source: &str) -> bool {
        let sid = match self.selected_session_id.clone() {
            Some(sid) => sid,
            None => return false,
        };
        let (w, h, bytes) = rgba;
        let expected = w.saturating_mul(h).saturating_mul(4) as usize;
        if w == 0 || h == 0 || bytes.len() != expected {
            tracing::warn!(
                "[IMAGE] bad pixel data from {}: {}x{} ({} bytes, expected {})",
                source,
                w,
                h,
                bytes.len(),
                expected
            );
            return false;
        }
        let png = match png_bytes_from_rgba(w, h, &bytes) {
            Some(p) => p,
            None => {
                tracing::warn!("[IMAGE] PNG encoding failed for {} image", source);
                return false;
            }
        };
        let png_len = png.len();
        if let Some(runtime) = self.session_store.get_mut(&sid) {
            // URI unique per content: egui's bytes loader keeps the FIRST
            // payload stored for a URI, so a fixed URI would show a stale
            // image whenever a different one is attached (per-session
            // pending images would also collide).
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hasher::write(&mut hasher, png.as_slice());
            let hash = std::hash::Hasher::finish(&hasher);
            runtime.chat_state.pending_image = Some(egui::ImageSource::Bytes {
                uri: format!("bytes://attached_image_{hash:016x}.png").into(),
                bytes: png.into(),
            });
        }
        tracing::info!(
            "[IMAGE] attached {}x{} image from {} ({} KB PNG)",
            w,
            h,
            source,
            png_len / 1024
        );
        true
    }
}

/// RGBA8 pixels (width, height, bytes) from the system clipboard, or `None`.
///
/// egui-winit can only paste TEXT from the clipboard: with an image-only
/// clipboard it logs "arboard paste error" and emits no event at all for the
/// key press, so image pastes are handled here instead. When the clipboard
/// also holds text, egui's own text paste already ran, so we return `None`
/// to avoid duplicating it.
fn clipboard_image_pixels() -> Option<(u32, u32, Vec<u8>)> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    if clipboard.get_text().is_ok() {
        return None;
    }
    let img = match clipboard.get_image() {
        Ok(img) => img,
        Err(e) => {
            tracing::debug!("[IMAGE] clipboard has no image: {}", e);
            return None;
        }
    };
    if img.width == 0 || img.height == 0 || img.bytes.is_empty() {
        return None;
    }
    Some((img.width as u32, img.height as u32, img.bytes.into_owned()))
}

/// Decode an image file (png/jpg/jpeg/webp/bmp/gif) into RGBA8 pixels.
fn image_pixels_from_file(path: &std::path::Path) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::open(path).ok()?.into_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

/// Encode RGBA8 pixels as PNG bytes.
fn png_bytes_from_rgba(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())?;
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).ok()?;
    Some(buf.into_inner())
}

/// Raw base64 (STANDARD) of a pending image's PNG bytes, for the chat display
/// (`ChatMessage.image`). Only `Bytes`-based sources carry the payload.
pub(super) fn pending_image_b64(source: Option<&egui::ImageSource<'static>>) -> Option<String> {
    match source? {
        egui::ImageSource::Bytes { bytes, .. } => {
            Some(base64::engine::general_purpose::STANDARD.encode(bytes.as_ref()))
        }
        _ => None,
    }
}
