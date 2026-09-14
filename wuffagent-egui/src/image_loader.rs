//! egui image decoder for the app.
//!
//! egui 0.36 ships no image decoders: an `ImageSource::Bytes` (used for the
//! pasted/attached-image preview, and for chat-bubble images via
//! `Image::from_bytes`) renders as a red "no image loaders are loaded" error
//! texture unless an [`ImageLoader`] is registered. This one decodes the bytes
//! with the `image` crate (already a direct dependency, same format set as the
//! file picker) and is registered once in `main.rs`.

use std::sync::Arc;

use eframe::egui::{self, ColorImage, Context, SizeHint};
use eframe::egui::load::{BytesPoll, ImageLoadResult, ImagePoll, LoadError};

/// Decodes image bytes (png/jpg/jpeg/gif/bmp/webp) with the `image` crate.
pub struct ImageBytesLoader;

impl egui::load::ImageLoader for ImageBytesLoader {
    fn id(&self) -> &str {
        egui::generate_loader_id!(ImageBytesLoader)
    }

    fn load(&self, ctx: &Context, uri: &str, _size_hint: SizeHint) -> ImageLoadResult {
        let bytes = match ctx.try_load_bytes(uri) {
            // `ImageSource::Bytes` calls `ctx.include_bytes` right before the
            // texture load, so app images are always Ready here.
            Ok(BytesPoll::Ready { bytes, .. }) => bytes,
            Ok(BytesPoll::Pending { size }) => {
                ctx.request_repaint();
                return Ok(ImagePoll::Pending { size });
            }
            // Unknown URI / no bytes loader supports it: let other loaders
            // (if any) have a turn.
            Err(e) => return Err(e),
        };

        match image::load_from_memory(bytes.as_ref()) {
            Ok(img) => {
                let img = img.into_rgba8();
                let image = ColorImage::from_rgba_unmultiplied(
                    [img.width() as usize, img.height() as usize],
                    &img.into_raw(),
                );
                Ok(ImagePoll::Ready {
                    image: Arc::new(image),
                })
            }
            Err(e) => {
                tracing::warn!("[IMAGE] failed to decode {uri}: {e}");
                Err(LoadError::FormatNotSupported {
                    detected_format: Some(e.to_string()),
                })
            }
        }
    }

    // Stateless: the default `TextureLoader` caches the decoded texture per
    // (uri, texture-options), so each byte payload is decoded at most once.
    fn forget(&self, _uri: &str) {}
    fn forget_all(&self) {}
    fn byte_size(&self) -> usize {
        0
    }
}
