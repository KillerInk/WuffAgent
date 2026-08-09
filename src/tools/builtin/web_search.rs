use std::collections::HashMap;

use crate::tools::lib::{Tool, ToolOutput, ToolParams, ToolSchema};

/// A tool that searches the web via a configurable search endpoint.
pub struct WebSearchTool {
    endpoint: String,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            endpoint: "https://lite.duckduckgo.com/lite/".to_string(),
        }
    }

    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = endpoint.to_string();
        self
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for information using a search engine"
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search".to_string(),
            description: "Search the web for information".to_string(),
            input_type: Some(crate::tools::lib::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "query".to_string(),
                        crate::tools::lib::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Search query".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "max_results".to_string(),
                        crate::tools::lib::FieldSchema {
                            type_name: "integer".to_string(),
                            description: "Maximum number of results to return".to_string(),
                            nullable: true,
                        },
                    );
                    map
                }),
                required: vec!["query".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::lib::ToolResult<ToolOutput> {
        let query: String = params
            .get("query")
            .ok_or_else(|| crate::tools::lib::ToolError::InvalidParams("query is required".to_string()))?;

        let max_results: u32 = params.get("max_results").unwrap_or(10);

        // In a real implementation this would make an HTTP request to the search endpoint.
        // For now we return a placeholder result so the system compiles.
        let results = serde_json::json!({
            "query": query,
            "max_results": max_results,
            "results": [],
            "message": "Web search is a placeholder — integrate a real search API"
        });

        Ok(ToolOutput::Success(results))
    }
}
