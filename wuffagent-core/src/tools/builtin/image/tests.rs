//! Unit tests for the `show_image` tool (see `super`).

use super::*;
use base64::Engine;

/// Encode a synthetic WxH PNG (solid color) to bytes.
fn png_bytes(w: u32, h: u32, color: (u8, u8, u8)) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |_, _| image::Rgb([color.0, color.1, color.2]));
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)
        .expect("encode test PNG");
    buf.into_inner()
}

fn write_temp_png(name: &str, w: u32, h: u32) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("wuffagent_show_image_{name}.png"));
    std::fs::write(&path, png_bytes(w, h, (120, 40, 200))).expect("write PNG");
    path
}

#[test]
fn test_show_image_file() {
    let path = write_temp_png("file", 100, 50);
    let tool = ShowImageTool::new();
    let result = tool
        .execute(ToolParams {
            values: {
                let mut m = HashMap::new();
                m.insert("path".to_string(), serde_json::json!(path.to_string_lossy()));
                m
            },
        })
        .expect("show_image should succeed");
    match result {
        ToolOutput::Success(v) => {
            assert_eq!(v["source"], "file");
            assert_eq!(v["format"], "png");
            assert_eq!(v["width"], 100);
            assert_eq!(v["height"], 50);
            let uri = v["data_uri"].as_str().expect("data_uri");
            assert!(uri.starts_with("data:image/jpeg;base64,"));
            let b64 = uri.strip_prefix("data:image/jpeg;base64,").unwrap();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .expect("valid base64");
            // Payload must actually be a decodable JPEG.
            assert!(image::load_from_memory(&bytes).is_ok());
            let _ = std::fs::remove_file(&path);
        }
        ToolOutput::Error(e) => panic!("show_image should succeed: {e}"),
    }
}

#[test]
fn test_show_image_data_uri() {
    let png = png_bytes(80, 40, (10, 200, 30));
    let uri = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    );
    let tool = ShowImageTool::new();
    let result = tool.execute(ToolParams {
        values: {
            let mut m = HashMap::new();
            m.insert("path".to_string(), serde_json::json!(uri));
            m
        },
    });
    match result.expect("data: URI should work") {
        ToolOutput::Success(v) => {
            assert_eq!(v["source"], "data-uri");
            assert_eq!(v["width"], 80);
            assert!(v["data_uri"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
        }
        ToolOutput::Error(e) => panic!("should succeed: {e}"),
    }
}

#[test]
fn test_show_image_downscales_large_image() {
    let path = write_temp_png("big", 2000, 1000);
    let tool = ShowImageTool::new();
    let result = tool.execute(ToolParams {
        values: {
            let mut m = HashMap::new();
            m.insert("path".to_string(), serde_json::json!(path.to_string_lossy()));
            m
        },
    });
    match result.expect("large image should work") {
        ToolOutput::Success(v) => {
            // Default max_width = 900.
            assert_eq!(v["width"], 900);
            assert_eq!(v["height"], 450);
        }
        ToolOutput::Error(e) => panic!("should succeed: {e}"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_show_image_missing_file() {
    let tool = ShowImageTool::new();
    let result = tool.execute(ToolParams {
        values: {
            let mut m = HashMap::new();
            m.insert(
                "path".to_string(),
                serde_json::json!("C:/definitely/not/here/wuff_test_missing.png"),
            );
            m
        },
    });
    assert!(result.is_err(), "missing file should error");
}

#[test]
fn test_show_image_not_an_image() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("not_an_image.txt");
    std::fs::write(&path, b"hello world, not an image").unwrap();
    let tool = ShowImageTool::new();
    let result = tool.execute(ToolParams {
        values: {
            let mut m = HashMap::new();
            m.insert("path".to_string(), serde_json::json!(path.to_string_lossy()));
            m
        },
    });
    assert!(result.is_err(), "non-image bytes should error");
}

#[test]
fn test_show_image_requires_path() {
    let tool = ShowImageTool::new();
    let result = tool.execute(ToolParams::new());
    match result {
        Err(ToolError::InvalidParams(msg)) => assert!(msg.contains("path")),
        other => panic!("expected InvalidParams, got {other:?}"),
    }
}

#[test]
fn test_expand_tilde() {
    let p = expand_tilde("~/some/file.png");
    assert!(
        p.is_absolute(),
        "tilde should expand to an absolute path, got {p:?}"
    );
    assert_eq!(expand_tilde("C:/x/y.png"), std::path::PathBuf::from("C:/x/y.png"));
}
