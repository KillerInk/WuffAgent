/// Tracks `<think>` tag boundaries for reasoning models (DeepSeek-R1, Qwen3.x, etc.)
/// that embed reasoning content in the regular `content` stream.
#[derive(Clone, Debug, Default)]
pub struct ReasoningState {
    /// Whether we are currently inside a `<think>` block.
    pub in_think: bool,
    /// Accumulated content since the last think-tag transition.
    pub pending: String,
}

impl ReasoningState {
    /// Process a content chunk, emitting (sub_chunks, is_thinking) pairs based on
    /// `<think>` / `</think>` tag boundaries.
    ///
    /// Returns a Vec of (content, is_thinking) pairs.
    pub fn process_chunk(&mut self, chunk: &str) -> Vec<(String, bool)> {
        let mut result = Vec::new();

        if chunk.is_empty() {
            return result;
        }

        let input = if self.pending.is_empty() {
            chunk.to_string()
        } else {
            let mut s = self.pending.clone();
            s.push_str(chunk);
            self.pending.clear();
            s
        };

        // Log first 300 chars of each combined input to see actual tag content
        let preview = &input[..input.chars().take(300).collect::<String>().len()];
        tracing::debug!(
            "reasoning_state: chunk_len={}, preview='{preview}'",
            input.len()
        );

        let mut chars = input.chars().peekable();
        let mut current: String = String::new();

        while let Some(c) = chars.next() {
            // Check for `<think>` opening tag
            if c == '<' {
                // Flush current buffer
                if !current.is_empty() {
                    result.push((current.clone(), !self.in_think));
                    current.clear();
                }

                // Peek ahead for "think>"
                let peeked: String = chars.by_ref().take(4).collect();
                if peeked == "think" {
                    // Consume the '>'
                    if chars.next() == Some('>') {
                        self.in_think = true;
                        tracing::debug!(
                            "reasoning_state: FOUND tag '<think>' at pos={:?}, current_in_think=false→true",
                            input.len() - chars.clone().count()
                        );
                        continue;
                    }
                }

                // Not a think tag, emit '<' and remaining peeked chars
                current.push('<');
                current.push_str(&peeked);
            }
            // Check for `</think>` closing tag
            else if c == '<' {
                // Flush current buffer
                if !current.is_empty() {
                    result.push((current.clone(), !self.in_think));
                    current.clear();
                }

                // Peek ahead for "/think>"
                let peeked: String = chars.by_ref().take(5).collect();
                if peeked == "/think" {
                    // Consume the '>'
                    if chars.next() == Some('>') {
                        self.in_think = false;
                        tracing::debug!(
                            "reasoning_state: FOUND tag '</think>' at pos={:?}, current_in_think=true→false",
                            input.len() - chars.clone().count()
                        );
                        continue;
                    }
                }

                current.push('<');
                current.push_str(&peeked);
            } else {
                current.push(c);
            }
        }

        // Flush remaining
        if !current.is_empty() {
            result.push((current.clone(), !self.in_think));
        }

        // If we're still inside a think block, buffer for next chunk
        if self.in_think && !current.is_empty() {
            // Still push it — we want streaming updates
            // But if current was split mid-tag, buffer the remainder
        }

        result
    }

    /// Finalize: flush any remaining pending content.
    pub fn finalize(&mut self) -> Option<(String, bool)> {
        if self.pending.is_empty() {
            return None;
        }
        let content = self.pending.clone();
        self.pending.clear();
        Some((content, !self.in_think))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pure_response() {
        let mut state = ReasoningState::default();
        let chunks = state.process_chunk("Hello world");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, "Hello world");
        assert!(!chunks[0].1);
    }

    #[test]
    fn test_pure_think() {
        let mut state = ReasoningState::default();
        let chunks = state.process_chunk("<think>Let me think about this</think>");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, "Let me think about this");
        assert!(chunks[0].1);
    }

    #[test]
    fn test_think_then_response() {
        let mut state = ReasoningState::default();
        let chunks = state.process_chunk("<think>Reasoning here</think>Okay, the answer is 42");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].0, "Reasoning here");
        assert!(chunks[0].1);
        assert_eq!(chunks[1].0, "Okay, the answer is 42");
        assert!(!chunks[1].1);
    }

    #[test]
    fn test_response_then_think() {
        let mut state = ReasoningState::default();
        let chunks = state.process_chunk("First I'll analyze<think>deep reasoning</think>then respond");
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].0, "First I'll analyze");
        assert!(!chunks[0].1);
        assert_eq!(chunks[1].0, "deep reasoning");
        assert!(chunks[1].1);
        assert_eq!(chunks[2].0, "then respond");
        assert!(!chunks[2].1);
    }

    #[test]
    fn test_multi_chunk_split() {
        let mut state = ReasoningState::default();
        // Split across two chunks
        let c1 = state.process_chunk("<think>Part one</thi");
        assert_eq!(c1.len(), 0); // pending buffered

        let c2 = state.process_chunk("nk>Part two</think>");
        assert_eq!(c2.len(), 2);
        assert_eq!(c2[0].0, "Part one");
        assert!(c2[0].1);
        assert_eq!(c2[1].0, "Part two");
        assert!(c2[1].1);
    }
}
