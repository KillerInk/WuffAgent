//! Shallow tool-parameter validation (T5/M2).
//!
//! `ToolManager::validate` used to be a no-op placeholder, so malformed
//! model-generated tool calls (wrong types, missing required fields) went
//! straight to the tool itself. This module checks the parts of the JSON
//! schema the tools actually declare — `required` fields and the top-level
//! property types — and reports human-readable problems, which the agent
//! loop feeds back to the model so it can self-correct.
//!
//! Full JSON-Schema semantics (nested objects, patterns, enums, …) are
//! deliberately out of scope: tool schemas are shallow, and the tool's own
//! argument parsing remains the final authority (unknown type keywords are
//! skipped rather than rejected).

use crate::tools::types::{FieldSchema, JsonSchema, ToolParams};

/// Validate `params` against `schema`.
///
/// Returns the list of human-readable problems; an empty list means valid.
pub fn validate_params(schema: &JsonSchema, params: &ToolParams) -> Vec<String> {
    let mut problems: Vec<String> = Vec::new();

    // Required fields: present and (unless the field is nullable) non-null.
    for req in &schema.required {
        match params.values.get(req) {
            None => problems.push(format!("missing required parameter '{req}'")),
            Some(serde_json::Value::Null) if !is_nullable(schema, req) => {
                problems.push(format!("required parameter '{req}' must not be null"))
            }
            Some(_) => {}
        }
    }

    // Declared property types (only for keys actually present in the call).
    if let Some(props) = &schema.properties {
        for (name, field) in props {
            if let Some(value) = params.values.get(name) {
                if let Some(problem) = type_problem(schema, name, field, value) {
                    problems.push(format!("parameter '{name}': {problem}"));
                }
            }
        }
    }

    problems
}

fn is_nullable(schema: &JsonSchema, name: &str) -> bool {
    schema
        .properties
        .as_ref()
        .and_then(|p| p.get(name))
        .map(|f| f.nullable)
        .unwrap_or(false)
}

fn is_required(schema: &JsonSchema, name: &str) -> bool {
    schema.required.iter().any(|r| r == name)
}

/// A type-mismatch message for one value, or `None` when it matches the
/// field's declared type.
fn type_problem(
    schema: &JsonSchema,
    name: &str,
    field: &FieldSchema,
    value: &serde_json::Value,
) -> Option<String> {
    match value {
        serde_json::Value::Null => {
            if field.nullable {
                None
            } else if is_required(schema, name) {
                // The required loop already reported this ("required parameter
                // '{name}' must not be null") — don't double-report.
                None
            } else {
                Some("must not be null".to_string())
            }
        }
        _ => match field.type_name.as_str() {
            "string" => check(value.is_string(), "string", value),
            "boolean" => check(value.is_boolean(), "boolean", value),
            "integer" => check(is_integer(value), "integer", value),
            "number" => check(value.is_number(), "number", value),
            "object" => check(value.is_object(), "object", value),
            "array" => check(value.is_array(), "array", value),
            // Unknown type vocabulary (extended/custom schema): don't reject —
            // the tool's own argument parsing is the final authority.
            _ => None,
        },
    }
}

/// `true` for JSON integers and for whole-number floats: LLMs frequently
/// emit `300.0` where the schema says `integer`, and rejecting that is
/// noisier than it is useful.
fn is_integer(value: &serde_json::Value) -> bool {
    matches!(
        value,
        serde_json::Value::Number(n)
            if n.is_i64()
                || n.is_u64()
                || n.as_f64().is_some_and(|f| f.fract() == 0.0)
    )
}

fn check(ok: bool, expected: &str, value: &serde_json::Value) -> Option<String> {
    if ok {
        None
    } else {
        Some(format!("expected {expected}, got {}", json_kind(value)))
    }
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn params(pairs: &[(&str, serde_json::Value)]) -> ToolParams {
        let mut values = HashMap::new();
        for (k, v) in pairs {
            values.insert(k.to_string(), v.clone());
        }
        ToolParams { values }
    }

    fn field(type_name: &str, nullable: bool) -> FieldSchema {
        FieldSchema {
            description: String::new(),
            type_name: type_name.to_string(),
            nullable,
        }
    }

    fn schema_with(
        required: &[&str],
        properties: &[(&str, &FieldSchema)],
    ) -> JsonSchema {
        let mut props = HashMap::new();
        for (k, v) in properties {
            props.insert(k.to_string(), (*v).clone());
        }
        JsonSchema {
            type_name: "object".to_string(),
            required: required.iter().map(|s| s.to_string()).collect(),
            properties: if props.is_empty() {
                None
            } else {
                Some(props)
            },
        }
    }

    #[test]
    fn empty_schema_accepts_anything() {
        let schema = schema_with(&[], &[]);
        assert!(
            validate_params(&schema, &params(&[("anything", serde_json::json!(1))])).is_empty()
        );
    }

    #[test]
    fn valid_params_pass() {
        let s = schema_with(
            &["path"],
            &[
                ("path", &field("string", false)),
                ("recursive", &field("boolean", true)),
            ],
        );
        assert!(
            validate_params(&s, &params(&[("path", serde_json::json!("/tmp")), ("recursive", serde_json::json!(true))])).is_empty()
        );
    }

    #[test]
    fn missing_required_field_is_reported() {
        let s = schema_with(&["path"], &[("path", &field("string", false))]);
        let problems = validate_params(&s, &params(&[]));
        assert_eq!(problems, vec!["missing required parameter 'path'"]);
    }

    #[test]
    fn null_required_non_nullable_is_reported() {
        let s = schema_with(&["path"], &[("path", &field("string", false))]);
        let problems = validate_params(&s, &params(&[("path", serde_json::Value::Null)]));
        assert_eq!(problems, vec!["required parameter 'path' must not be null"]);
    }

    #[test]
    fn null_required_nullable_is_accepted() {
        let s = schema_with(&["path"], &[("path", &field("string", true))]);
        assert!(validate_params(&s, &params(&[("path", serde_json::Value::Null)])).is_empty());
    }

    #[test]
    fn null_optional_nullable_is_accepted() {
        let s = schema_with(&[], &[("path", &field("string", true))]);
        assert!(validate_params(&s, &params(&[("path", serde_json::Value::Null)])).is_empty());
    }

    #[test]
    fn null_optional_non_nullable_is_reported() {
        let s = schema_with(&[], &[("path", &field("string", false))]);
        let problems = validate_params(&s, &params(&[("path", serde_json::Value::Null)]));
        assert_eq!(problems, vec!["parameter 'path': must not be null"]);
    }

    #[test]
    fn wrong_type_is_reported() {
        let s = schema_with(&["timeout"], &[("timeout", &field("integer", false))]);
        let problems = validate_params(&s, &params(&[("timeout", serde_json::json!("fast"))]));
        assert_eq!(problems, vec!["parameter 'timeout': expected integer, got string"]);
    }

    #[test]
    fn integer_accepts_int_and_whole_float() {
        let s = schema_with(&["n"], &[("n", &field("integer", false))]);
        assert!(validate_params(&s, &params(&[("n", serde_json::json!(42))])).is_empty());
        assert!(validate_params(&s, &params(&[("n", serde_json::json!(42.0))])).is_empty());
    }

    #[test]
    fn integer_rejects_fractional_float() {
        let s = schema_with(&["n"], &[("n", &field("integer", false))]);
        let problems = validate_params(&s, &params(&[("n", serde_json::json!(4.5))]));
        assert_eq!(problems, vec!["parameter 'n': expected integer, got number"]);
    }

    #[test]
    fn number_accepts_int_and_float() {
        let s = schema_with(&["n"], &[("n", &field("number", false))]);
        assert!(validate_params(&s, &params(&[("n", serde_json::json!(4))])).is_empty());
        assert!(validate_params(&s, &params(&[("n", serde_json::json!(4.5))])).is_empty());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let s = schema_with(&[], &[("path", &field("string", false))]);
        assert!(
            validate_params(&s, &params(&[("path", serde_json::json!("/x")), ("extra", serde_json::json!(true))])).is_empty()
        );
    }

    #[test]
    fn unknown_type_keyword_is_not_rejected() {
        let s = schema_with(&[], &[("x", &field("integer_or_string", false))]);
        assert!(validate_params(&s, &params(&[("x", serde_json::json!("anything"))])).is_empty());
    }

    #[test]
    fn multiple_problems_are_all_reported() {
        let s = schema_with(
            &["a"],
            &[
                ("a", &field("string", false)),
                ("b", &field("integer", false)),
            ],
        );
        let problems = validate_params(&s, &params(&[("b", serde_json::json!(1.5))]));
        assert_eq!(problems.len(), 2);
        assert!(problems.iter().any(|p| p.contains("missing required")));
        assert!(problems.iter().any(|p| p.contains("expected integer")));
    }
}
