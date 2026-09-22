//! System prompt construction. Split out of agents/agent.rs (A1).

use super::Agent;

impl Agent {

    /// Build the system prompt for this agent.
    /// `query` is the user's current request; it makes memory injection query-aware.
    pub(crate) fn build_system_prompt(&self, query: &str) -> String {
        let mut prompt = if self.config.system_prompt.is_empty() {
            format!(
                "You are the '{}' agent. {}",
                self.config.name, self.config.description
            )
        } else {
            self.config.system_prompt.clone()
        };

        // Add memory management tool guidance
        prompt.push_str(
            "\n\nYou have memory management tools (save_memory, update_memory, search_memory, consolidate_memories, delete_memory). Use them proactively:\n",
        );
        prompt.push_str(
            "- search_memory: Search memory before starting tasks and before saving anything new\n",
        );
        prompt.push_str("- save_memory: Save non-obvious facts, lessons, or decisions as you discover them during work\n");
        prompt.push_str("- update_memory: Refine an existing entry (by ID) instead of re-adding similar information\n");
        prompt.push_str(
            "- consolidate_memories: Merge related entries (by IDs) into one comprehensive entry\n",
        );
        prompt.push_str("- delete_memory: Remove entries that turn out to be stale or wrong\n");
        prompt.push_str(&format!(
            "Tool responses include entry IDs - use them for updates, consolidation, and deletion. \
             Only save information that is persistent and useful across sessions; don't save routine \
             operations or temporary information. When saving a lesson, tag it with your agent name \
             (tags: [\"agent:{}\", ...]) so per-agent improvement checks can find it.",
            self.config.name
        ));

        // Handoff guidance: only when the agent actually has the tool.
        if self.config.handoff_enabled {
            prompt.push_str(
                "\n\n## HANDOFF\n\
                 You can switch the session to a different agent by calling the `handoff` tool with:\n\
                 - `agent`: the target agent's profile name (e.g. \"coder\")\n\
                 - `task`: what the target agent should do next (include the context it needs — it sees the full conversation too)\n\
                 Call it when your part of the work is complete and another specialist should continue \
                 (e.g. after finishing a plan, hand off to a coder to implement it). \
                 Your turn ends when you call it; the session continues with the target agent.",
            );
            if !self.config.handoff_targets.is_empty() {
                prompt.push_str(&format!(
                    "\nYou may only hand off to: {}.",
                    self.config.handoff_targets.join(", ")
                ));
            }
        }
        // Restart guidance: only when the agent actually has the tool. Emphasize
        // the Windows `--target-dir` self-build pattern (dogfooding WuffAgent's
        // own source: edit → build → restart to load the new code → resume).
        if self.config.restart_enabled {
            prompt.push_str(
                "\n\n## RESTART\\\n\
                 You can restart WuffAgent — then resume this session automatically — by calling the `restart` tool with:\\n\\\n\
                 - `reason` (required): what you changed and why you are restarting; shown to the user and used to resume the work\\n\\\n\
                 - `build_cmd` (optional): a command to run FIRST (e.g. a rebuild); if it fails the restart is skipped so you can fix it\\n\\\n\
                 - `exe_path` (optional): the binary to launch; omit to relaunch the current executable\\n\\\n\
                 Use it after making changes that require a rebuild. Most useful when editing WuffAgent's own source. \
                 For WuffAgent itself, OMIT build_cmd and exe_path: the tool then builds and launches the OTHER of \
                 WuffAgent's two standard builds — the default `cargo build` output (target/debug) and a second copy \
                 (target/relaunch) — alternating between them on every restart, since on Windows the running exe \
                 cannot be relinked in place. Your turn ends when you call it; WuffAgent closes and reopens, then \
                 continues the same work.",
            );
        }

        // Inject memories relevant to the current request (query-aware)
        if let Some(memory) = &self.memory {
            let memory_block = memory.build_context_block(query);
            if !memory_block.is_empty() {
                prompt.push_str("\n\n");
                prompt.push_str(&memory_block);
            }
        }

        // Note: system prompt caching would require &'mut self, which conflicts
        // with the LLM loop. The prompt is cheap to rebuild (~100ns).
        prompt
    }
}
