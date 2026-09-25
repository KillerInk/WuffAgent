//! `show_image`: the agent's own image-loading tool.
//!
//! Loads an image from a file path, `data:` URI, or http(s) URL, decodes it,
//! downscales it to a display-friendly size, re-encodes it (JPEG) and returns
//! it as a base64 `data:` URI in the result JSON. The egui chat UI recognises
//! the `data_uri` field and renders the image inside the tool card, so the
//! agent can show the user any image it finds on disk or on the web.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine;

use crate::tools::types::{Tool, ToolError, ToolOutput, ToolParams, ToolSchema};

/// Default downscale width in pixels.
const DEFAULT_MAX_WIDTH: u32 = 900;
const MIN_MAX_WIDTH: u32 = 160;
const MAX_MAX_WIDTH: u32 = 4096;

/// Never download / read more than this from the source.
const MAX_SOURCE_BYTES: u64 = 25 * 1024 * 1024;

/// Target size for the encoded payload (keeps the LLM-facing tool result
/// from ballooning): if the JPEG comes out bigger, halve the width and retry.
/// The result is replayed into the model's context in every later request,
/// so 256 KB (≈ 340 KB of base64 text) is the practical ceiling for an
/// inline image.
const TARGET_ENCODED_BYTES: usize = 256 * 1024;
const MIN_ENCODE_WIDTH: u32 = 320;
const JPEG_QUALITY: u8 = 82;

/// Cached tokio current-thread runtime for use inside spawn_blocking tool
/// calls (same pattern as `fetch_url`: never build a runtime per call).
static BLOCKING_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build blocking runtime")
});

macro_rules! block_on {
    ($expr:expr) => {{
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on($expr),
            Err(_) => BLOCKING_RUNTIME.block_on($expr),
        }
    }};
}

/// Load an image (file path, `data:` URI, or http(s) URL), decode it,
/// downscale it, and return it as a renderable `data:` URI so the chat UI
/// can display it in the tool card.
pub struct ShowImageTool {
    http_client: reqwest::Client,
}

impl ShowImageTool {
    pub fn new() -> Self {
        Self {
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client builder"),
        }
    }
}

impl Default for ShowImageTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Fetch the source bytes for `source`, returning them plus a label for the
/// result JSON (`"file"`, `"url"`, or `"data-uri"`).
fn load_source_bytes(
    source: &str,
    http_client: &reqwest::Client,
) -> Result<(Vec<u8>, &'static str), ToolError> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return Err(ToolError::InvalidParams("path is required".to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("data:") {
        // data:[<mime>][;base64],<payload>
        let (meta, payload) = rest
            .split_once(',')
            .ok_or_else(|| ToolError::InvalidParams("malformed data: URI (missing payload)".to_string()))?;
        if !meta.to_ascii_lowercase().ends_with(";base64") {
            return Err(ToolError::InvalidParams(
                "only base64 data: URIs are supported".to_string(),
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|e| ToolError::InvalidParams(format!("invalid base64 in data: URI: {e}")))?;
        return Ok((bytes, "data-uri"));
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let body = block_on!(async {
            let resp = http_client
                .get(trimmed)
                .header(
                    "User-Agent",
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) WuffAgent/1.0",
                )
                .header("Accept", "image/*,*/*;q=0.8")
                .send()
                .await
                .map_err(|e| ToolError::Execution(format!("HTTP request failed: {e}")))?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(ToolError::Execution(format!("HTTP {status} for {trimmed}")));
            }
            resp.bytes().await.map_err(|e| {
                ToolError::Execution(format!("Failed to read response: {e}"))
            })
        })?;
        if body.len() as u64 > MAX_SOURCE_BYTES {
            return Err(ToolError::Execution(format!(
                "image too large to load: {} bytes (limit {})",
                body.len(),
                MAX_SOURCE_BYTES
            )));
        }
        return Ok((body.to_vec(), "url"));
    }
    // Local file path (with `~` expansion).
    let path = expand_tilde(trimmed);
    let bytes = std::fs::read(&path).map_err(|e| {
        ToolError::Execution(format!("could not read image file {}: {e}", path.display()))
    })?;
    if bytes.is_empty() {
        return Err(ToolError::Execution(format!(
            "image file is empty: {}",
            path.display()
        )));
    }
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(ToolError::Execution(format!(
            "image file too large: {} bytes (limit {})",
            bytes.len(),
            MAX_SOURCE_BYTES
        )));
    }
    Ok((bytes, "file"))
}

/// Expand a leading `~` / `~/` to the user's home directory.
fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    std::path::PathBuf::from(path)
}

/// Flatten alpha onto white and encode as JPEG (quality 82).
fn encode_jpeg(img: &image::DynamicImage) -> Vec<u8> {
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let rgb = image::ImageBuffer::from_fn(w, h, |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let inv = 255 - a as u16;
        image::Rgb([
            (((r as u16) * a as u16 + 255 * inv) / 255) as u8,
            (((g as u16) * a as u16 + 255 * inv) / 255) as u8,
            (((b as u16) * a as u16 + 255 * inv) / 255) as u8,
        ])
    });
    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY);
    if rgb.write_with_encoder(encoder).is_ok() {
        buf.into_inner()
    } else {
        Vec::new()
    }
}

impl Tool for ShowImageTool {
    fn name(&self) -> &str {
        "show_image"
    }

    fn description(&self) -> &str {
        "Load an image (local file path, data: URI, or http(s) URL), downscale it, and display it in the chat. Returns dimensions plus a renderable data URI the UI shows in the tool card. Params: path (required), max_width (optional, default 900), caption (optional)"
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "show_image".to_string(),
            description: "Load and display an image in the chat".to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "path".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Image source: a file path (e.g. C:\\imgs\\shot.png or ~/pic.jpg), a data: URI, or an http(s) URL".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "max_width".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "integer".to_string(),
                            description: format!(
                                "Maximum display width in pixels (default {DEFAULT_MAX_WIDTH}, clamped to {MIN_MAX_WIDTH}..={MAX_MAX_WIDTH})"
                            ),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "caption".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Optional caption shown under the image".to_string(),
                            nullable: true,
                        },
                    );
                    map
                }),
                required: vec!["path".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let source: String = params.get("path").ok_or_else(|| {
            ToolError::InvalidParams("path is required".to_string())
        })?;
        let max_width: u32 = params
            .get("max_width")
            .unwrap_or(DEFAULT_MAX_WIDTH)
            .clamp(MIN_MAX_WIDTH, MAX_MAX_WIDTH);
        let caption: Option<String> = params.get("caption");

        let (bytes, source_kind) = load_source_bytes(&source, &self.http_client)?;
        let original_format = match image::guess_format(&bytes) {
            Ok(image::ImageFormat::Png) => "png".to_string(),
            Ok(image::ImageFormat::Jpeg) => "jpeg".to_string(),
            Ok(image::ImageFormat::Gif) => "gif".to_string(),
            Ok(image::ImageFormat::Bmp) => "bmp".to_string(),
            Ok(image::ImageFormat::WebP) => "webp".to_string(),
            Ok(_) => "other".to_string(),
            Err(_) => "unknown".to_string(),
        };
        let img = image::load_from_memory(&bytes).map_err(|e| {
            ToolError::Execution(format!("failed to decode image from {source}: {e}"))
        })?;
        let (orig_w, orig_h) = (img.width(), img.height());
        if orig_w == 0 || orig_h == 0 {
            return Err(ToolError::Execution("decoded image has zero size".to_string()));
        }

        // Downscale to the requested display width.
        let mut work = if orig_w > max_width {
            let h = ((orig_h as u64 * max_width as u64 / orig_w as u64).max(1)) as u32;
            image::DynamicImage::ImageRgba8(image::imageops::resize(
                &img,
                max_width,
                h,
                image::imageops::FilterType::Lanczos3,
            ))
        } else {
            img
        };

        // Keep the payload (and the LLM-facing tool result) bounded: halve the
        // width until the JPEG fits under TARGET_ENCODED_BYTES.
        let mut encoded = encode_jpeg(&work);
        let mut attempts = 0;
        while encoded.len() > TARGET_ENCODED_BYTES
            && work.width() > MIN_ENCODE_WIDTH
            && attempts < 6
        {
            let w = (work.width() / 2).max(MIN_ENCODE_WIDTH);
            let h = ((work.height() as u64 * w as u64 / work.width() as u64).max(1)) as u32;
            work = image::DynamicImage::ImageRgba8(image::imageops::resize(
                &work,
                w,
                h,
                image::imageops::FilterType::Lanczos3,
            ));
            encoded = encode_jpeg(&work);
            attempts += 1;
        }
        if encoded.is_empty() {
            return Err(ToolError::Execution("failed to encode image as JPEG".to_string()));
        }

        let data_uri = format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&encoded)
        );

        let mut out = serde_json::json!({
            "path": source,
            "source": source_kind,
            "format": original_format,
            "width": work.width(),
            "height": work.height(),
            "bytes": encoded.len(),
            "data_uri": data_uri,
        });
        if let Some(c) = &caption {
            if !c.trim().is_empty() {
                out["caption"] = serde_json::json!(c);
            }
        }
        tracing::info!(
            "[IMAGE] show_image: {} {}x{} -> {}x{} ({} KB JPEG, source={})",
            original_format,
            orig_w,
            orig_h,
            work.width(),
            work.height(),
            encoded.len() / 1024,
            source_kind
        );
        Ok(ToolOutput::Success(out))
    }
}

#[cfg(test)]
mod tests;
