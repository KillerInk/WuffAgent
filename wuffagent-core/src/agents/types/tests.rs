//! Unit tests for the `types` module (see `super`).

use super::*;

#[test]
fn test_agent_id_generate() {
    let id1 = AgentId::generate();
    let id2 = AgentId::generate();
    assert_ne!(id1, id2);
    assert!(id1.0.starts_with("agent-"));
}
