use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::tools::types::{Tool, ToolOutput, ToolParams, ToolSchema};

/// A tool that returns the current date and time.
pub struct TimeTool;

impl TimeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TimeTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for TimeTool {
    fn name(&self) -> &str {
        "time"
    }

    fn description(&self) -> &str {
        "Get the current date and time in UTC"
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "time".to_string(),
            description: "Get current date and time".to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: None,
                required: vec![],
            }),
        }
    }

    fn execute(&self, _params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let now: DateTime<Utc> = Utc::now();
        let iso_timestamp = now.to_rfc3339();
        let unix_timestamp = now.timestamp();

        Ok(ToolOutput::Success(serde_json::json!({
            "timestamp": iso_timestamp,
            "unix": unix_timestamp,
            "utc": now.format("%Y-%m-%d %H:%M:%S UTC").to_string()
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_time_tool() {
        let tool = TimeTool::new();
        let result = tool.execute(ToolParams::new()).unwrap();
        
        match result {
            ToolOutput::Success(value) => {
                assert!(value.get("timestamp").is_some());
                assert!(value.get("unix").is_some());
                assert!(value.get("utc").is_some());
            }
            ToolOutput::Error(_) => panic!("Time tool should succeed"),
        }
    }
}
