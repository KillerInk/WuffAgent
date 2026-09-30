//! F6: the grouped "Allowed Tools" tree (types + builders + tree rendering).
//!
//! The tree is a pure projection of `available_tools`: leaves carry an index
//! into that list (the dialog's `tool_checkboxes` stay the source of truth
//! for checkbox state), groups only add display structure ("Builtin tools"
//! by category, "MCP tools" by server).

use std::collections::HashMap;

use eframe::egui;

use super::AgentConfigDialog;

/// A node in the grouped "Allowed Tools" tree: either a named group of
/// children or a single tool (an index into `available_tools`, which stays
/// the source of truth for checkbox state).
#[derive(Clone, Debug)]
pub(super) enum ToolNode {
    Group { title: String, children: Vec<ToolNode> },
    Leaf(usize),
}

/// Builtin tool categories (display order; anything unlisted falls into
/// "Other" so newly added tools never vanish from the list).
const BUILTIN_CATEGORIES: &[(&str, &[&str])] = &[
    (
        "Files",
        &[
            "read_file", "write_file", "append_file", "apply_diff", "replace_lines", "file_info",
            "list_dir", "mkdir", "copy", "move", "delete", "search_files", "search_content",
        ],
    ),
    (
        "Memory",
        &["search_memory", "save_memory", "update_memory", "delete_memory", "consolidate_memories"],
    ),
    ("Skills", &["list_skills", "read_skill", "save_skill", "delete_skill"]),
    ("Evals & Metrics", &["list_evals", "save_eval", "delete_eval", "run_eval", "read_metrics"]),
    ("Improvement", &["run_self_improvement", "list_improvement_status"]),
    (
        "Agents",
        &[
            "list_agents", "edit_agent_profile", "handoff", "hand_back", "session_note", "restart",
        ],
    ),
    (
        "MCP servers",
        &[
            "mcp_list", "mcp_add_server", "mcp_connect", "mcp_disconnect", "mcp_remove_server",
            "mcp_refresh_tools", "mcp_set_tool_enabled",
        ],
    ),
    ("Plugins", &["add_plugin_path", "reload_plugins"]),
    ("Web", &["web_search", "fetch_url"]),
];

/// Split an `mcp__<server>__<tool>` name into (server, tool). Server names
/// may contain `__` (tool names don't), so the last `__`-segment is the tool
/// and everything between the `mcp__` prefix and it is the server.
pub(super) fn mcp_server_tool(name: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = name.split("__").collect();
    if parts.len() < 3 || parts[0] != "mcp" {
        return None;
    }
    let server = parts[1..parts.len() - 1].join("__");
    Some((server, parts[parts.len() - 1].to_string()))
}

/// Build the grouped tool tree: top-level "Builtin tools" (sorted per
/// category) and "MCP tools" (one subgroup per server, sorted).
pub(super) fn build_tools_tree(tools: &[String]) -> Vec<ToolNode> {
    let mut builtin: Vec<(String, Vec<usize>)> = BUILTIN_CATEGORIES
        .iter()
        .map(|(title, members)| {
            let mut idx: Vec<usize> = tools
                .iter()
                .enumerate()
                .filter(|(_, name)| members.contains(&name.as_str()))
                .map(|(i, _)| i)
                .collect();
            idx.sort_unstable();
            (title.to_string(), idx)
        })
        .collect();
    // Tools matching no category (e.g. plugin tools like `hello`) — last.
    let mut other: Vec<usize> = (0..tools.len())
        .filter(|i| {
            let name = &tools[*i];
            !name.starts_with("mcp__")
                && !BUILTIN_CATEGORIES
                    .iter()
                    .any(|(_, members)| members.contains(&name.as_str()))
        })
        .collect();
    other.sort_unstable();
    if !other.is_empty() {
        builtin.push(("Other".to_string(), other));
    }
    let builtin_group = ToolNode::Group {
        title: "Builtin tools".to_string(),
        children: builtin
            .into_iter()
            .filter(|(_, idx)| !idx.is_empty())
            .map(|(title, idx)| {
                ToolNode::Group {
                    title,
                    children: idx.into_iter().map(ToolNode::Leaf).collect(),
                }
            })
            .collect(),
    };

    // MCP tools: one subgroup per server.
    let mut servers: HashMap<String, Vec<(String, usize)>> = HashMap::new();
    for (i, name) in tools.iter().enumerate() {
        if let Some((server, tool)) = mcp_server_tool(name) {
            servers.entry(server).or_default().push((tool, i));
        }
    }
    let mut server_names: Vec<String> = servers.keys().cloned().collect();
    server_names.sort();
    let mcp_children: Vec<ToolNode> = server_names
        .into_iter()
        .map(|server| {
            let mut members = servers.get(&server).cloned().unwrap_or_default();
            members.sort_by(|a, b| a.0.cmp(&b.0));
            ToolNode::Group {
                title: server,
                children: members.into_iter().map(|(_, i)| ToolNode::Leaf(i)).collect(),
            }
        })
        .collect();

    let mut tree = Vec::new();
    let builtin_empty = matches!(&builtin_group, ToolNode::Group { children, .. } if children.is_empty());
    if !builtin_empty {
        tree.push(builtin_group);
    }
    if !mcp_children.is_empty() {
        tree.push(ToolNode::Group {
            title: "MCP tools".to_string(),
            children: mcp_children,
        });
    }
    tree
}

impl AgentConfigDialog {
    /// Draw one node of the grouped tools tree. An empty-title group is a
    /// plain (non-collapsing) container; named groups render as collapsing
    /// headers with a "select all" checkbox for the whole group.
    pub(super) fn draw_tool_node(&mut self, ui: &mut egui::Ui, node: &ToolNode) {
        match node {
            ToolNode::Leaf(i) => {
                let name = self.available_tools[*i].clone();
                ui.checkbox(&mut self.tool_checkboxes[*i], name);
            }
            ToolNode::Group { title, children } => {
                if title.is_empty() {
                    for child in children {
                        self.draw_tool_node(ui, child);
                    }
                    return;
                }
                let (checked, total) = self.group_stats(children);
                ui.collapsing(
                    egui::RichText::new(format!(
                        "{} ({}/{})",
                        title, checked, total
                    ))
                    .strong(),
                    |ui| {
                        let mut select_all = checked == total;
                        ui.checkbox(&mut select_all, "Select all in this group");
                        if select_all != (checked == total) {
                            self.set_group_checked(children, select_all);
                        }
                        for child in children {
                            self.draw_tool_node(ui, child);
                        }
                    },
                );
            }
        }
    }

    /// Number of checked + total leaf tools under a group (recursive).
    fn group_stats(&self, nodes: &[ToolNode]) -> (usize, usize) {
        nodes
            .iter()
            .fold((0, 0), |(c, t), node| match node {
                ToolNode::Leaf(i) => (c + self.tool_checkboxes[*i] as usize, t + 1),
                ToolNode::Group { children, .. } => {
                    let (cc, tt) = self.group_stats(children);
                    (c + cc, t + tt)
                }
            })
    }

    /// Set every leaf checkbox under a group (recursive).
    fn set_group_checked(&mut self, nodes: &[ToolNode], value: bool) {
        for node in nodes {
            match node {
                ToolNode::Leaf(i) => self.tool_checkboxes[*i] = value,
                ToolNode::Group { children, .. } => self.set_group_checked(children, value),
            }
        }
    }
}
