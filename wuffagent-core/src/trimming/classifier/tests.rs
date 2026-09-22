//! Unit tests for the `classifier` module (see `super`).

use super::*;

#[test]
fn test_empty_content() {
    assert_eq!(classify_content(""), ContentType::FreeText);
}

#[test]
fn test_build_log_detection() {
    let log = r#"
   Compiling my-project v0.1.0
    Checking deps v1.0.0
   Downloaded serde v1.0
   Downloaded tokio v1.0
    Finished dev [unoptimized + debuginfo] target(s)
warning: unused variable
error[E0382]: use of moved value
"#;
    assert_eq!(classify_content(log), ContentType::BuildLog);
}

#[test]
fn test_source_code_detection() {
    let code = r#"fn main() {
    let x = 5;
    if x > 3 {
        println!("big");
    }
}

struct Foo {
    bar: i32,
}

impl Foo {
    pub fn new() -> Self {
        Foo { bar: 0 }
    }
}"#;
    assert_eq!(classify_content(code), ContentType::SourceCode);
}

#[test]
fn test_file_list_detection() {
    let list = r#"
src/lib.rs
src/main.rs
src/trimming/classifier.rs
src/trimming/summarizer.rs
Cargo.toml
Cargo.lock
README.md
docs/architecture.md
tests/integration.rs"#;
    assert_eq!(classify_content(list), ContentType::FileList);
}

#[test]
fn test_json_wrapper_detection() {
    let json = r#"{"result": "file written", "path": "test.txt", "bytes_written": 100}"#;
    assert_eq!(classify_content(json), ContentType::JsonWrapper);
}

#[test]
fn test_tool_error_detection() {
    let err = "Error: failed to read file\n  at std::fs::read\n  at wuffagent::tools::file_io\n  at main::run";
    assert_eq!(classify_content(err), ContentType::ToolError);
}

#[test]
fn test_free_text_detection() {
    let text = "This is just a normal response from the LLM.";
    assert_eq!(classify_content(text), ContentType::FreeText);
}

#[test]
fn test_code_not_detected_for_short_content() {
    // Short content shouldn't be classified as source code.
    assert_eq!(classify_content("fn main() {}"), ContentType::FreeText);
}

#[test]
fn test_source_code_detection_with_line_numbers() {
    // Prefixed (cat -n style) file-read output must still be detected
    // as source code.
    let code = "      1 | fn main() {\n      2 |     let x = 5;\n      3 |     if x > 3 {\n      4 |         println!(\"big\");\n      5 |     }\n      6 | }";
    assert_eq!(classify_content(code), ContentType::SourceCode);
}

#[test]
fn test_plain_text_with_line_numbers_not_source_code() {
    // The line-number prefix must not masquerade as indentation and
    // drag plain prose into the SourceCode bucket.
    let text = "      1 | This is just a normal response.\n      2 | Another sentence here.\n      3 | And a final one.";
    assert_eq!(classify_content(text), ContentType::FreeText);
}

#[test]
fn test_strip_line_number_prefix() {
    assert_eq!(
        strip_line_number_prefix("      12 | let x = 5;"),
        "let x = 5;"
    );
    assert_eq!(
        strip_line_number_prefix("      3 |     indented"),
        "    indented"
    );
    assert_eq!(strip_line_number_prefix("no prefix"), "no prefix");
    assert_eq!(
        strip_line_number_prefix("12 | digits but no leading pad"),
        "digits but no leading pad"
    );
    assert_eq!(
        strip_line_number_prefix("abc | not a number"),
        "abc | not a number"
    );
}
