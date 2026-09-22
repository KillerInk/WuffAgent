//! Unit tests for the `time` module (see `super`).

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
