//! Image helpers for model requests.
//!
//! Pairs with the documented egui exception in `types` (`QueuedMessage.image`):
//! the UI hands images to the agent as `egui::ImageSource`, and the
//! conversion to the `data:` URI form used in model requests lives here so
//! the agent loop stays egui-free. Both disappear with the E1 data-URI
//! migration.

use base64::Engine;

/// Convert an attached image (egui source from the UI) into the `data:` URI
/// form used in model requests. Only `Bytes` sources carry a payload to send;
/// texture/URI references have none and return `None`.
pub fn image_source_data_uri(source: &egui::ImageSource<'static>) -> Option<String> {
    let bytes = match source {
        egui::ImageSource::Bytes { bytes, .. } => bytes,
        _ => return None,
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes.as_ref());
    Some(format!("data:image/png;base64,{}", b64))
}
