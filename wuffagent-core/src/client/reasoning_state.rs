/// Tracks `think` tag boundaries for reasoning models (DeepSeek-R1, Qwen3.x, etc.)
/// that embed reasoning content in the regular `content` stream.
///
/// Tag literals are assembled with `concat!` so the raw tag sequence is not
/// spelled out in source (some harnesses mangle it).
#[derive(Clone, Debug, Default)]
pub struct ReasoningState {
    /// Whether we are currently inside a `think` block.
    pub in_think: bool,
    /// A trailing partial tag (split across chunk boundaries), if any.
    pub pending: String,
}

/// Opening think tag (built at compile time).
const OPEN_TAG: &str = concat!("<", "think>");
/// Closing think tag (built at compile time).
const CLOSE_TAG: &str = concat!("<", "/think>");
/// Portion of the closing tag after the leading `<` (what we peek after `<`).
const CLOSE_AFTER_LT: &str = "/think>";
/// Portion of the opening tag after the leading `<`.
const OPEN_AFTER_LT: &str = "think>";

impl ReasoningState {
    /// Process a content chunk, emitting `(content, is_thinking)` pairs based on
    /// think-tag boundaries.
    ///
    /// Semantics:
    /// - A confirmed opening tag flips `in_think` to true; text accumulated
    ///   before it is flushed as a non-thinking segment.
    /// - A confirmed closing tag flips `in_think` to false; the thinking text
    ///   accumulated since the opening tag is flushed as a thinking segment.
    /// - Text between the last confirmed boundary and the end of the chunk is
    ///   flushed with the current `in_think` state (streaming updates).
    /// - A trailing partial tag (e.g. the chunk ends mid-tag) is stashed in
    ///   `pending` and re-prefixed to the next chunk.
    /// - Tags that are not confirmed (e.g. a closing tag while not thinking,
    ///   or an opening tag while already thinking) pass through as literal text.
    pub fn process_chunk(&mut self, chunk: &str) -> Vec<(String, bool)> {
        let mut result: Vec<(String, bool)> = Vec::new();

        let mut input = std::mem::take(&mut self.pending);
        input.push_str(chunk);
        if input.is_empty() {
            return result;
        }

        let preview: String = input.chars().take(300).collect();
        tracing::debug!(
            "reasoning_state: chunk_len={}, preview='{preview}'",
            input.len()
        );

        let mut chars = input.chars().peekable();
        let mut seg: String = String::new(); // current-state text since last boundary
        let mut buf: String = String::new(); // text since last '<'

        while let Some(c) = chars.next() {
            if c == '<' {
                seg.push_str(&buf);
                buf.clear();

                if chars.peek() == Some(&'/') {
                    // Possible closing tag: '<' + "/think>" (7 chars after '<')
                    let peeked: String = chars.by_ref().take(7).collect();
                    if peeked == CLOSE_AFTER_LT {
                        if self.in_think {
                            if !seg.is_empty() {
                                result.push((seg.clone(), true));
                                seg.clear();
                            }
                            self.in_think = false;
                            tracing::debug!("reasoning_state: EXITED think mode");
                        } else {
                            // Literal closing tag outside think mode — pass through
                            buf.push('<');
                            buf.push_str(&peeked);
                        }
                    } else {
                        buf.push('<');
                        buf.push_str(&peeked);
                    }
                } else {
                    // Possible opening tag: '<' + "think>" (6 chars after '<')
                    let peeked: String = chars.by_ref().take(6).collect();
                    if peeked == OPEN_AFTER_LT {
                        if !self.in_think {
                            if !seg.is_empty() {
                                result.push((seg.clone(), false));
                                seg.clear();
                            }
                            self.in_think = true;
                            tracing::debug!("reasoning_state: ENTERED think mode");
                        } else {
                            // Literal opening tag while already thinking — pass through
                            buf.push('<');
                            buf.push_str(&peeked);
                        }
                    } else {
                        buf.push('<');
                        buf.push_str(&peeked);
                    }
                }
            } else {
                buf.push(c);
            }
        }

        // Stash a trailing partial tag (split across chunks) for the next call
        if let Some(pos) = buf.rfind('<') {
            let tail = &buf[pos..];
            if OPEN_TAG.starts_with(tail) || CLOSE_TAG.starts_with(tail) {
                self.pending = tail.to_string();
                buf.truncate(pos);
            }
        }

        seg.push_str(&buf);
        if !seg.is_empty() {
            result.push((seg, self.in_think));
        }

        result
    }

    /// Finalize: flush any remaining pending (partial-tag) content.
    /// The `is_thinking` flag reflects the state at finalization.
    pub fn finalize(&mut self) -> Option<(String, bool)> {
        if self.pending.is_empty() {
            return None;
        }
        let content = std::mem::take(&mut self.pending);
        Some((content, self.in_think))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tag literals assembled at runtime to keep the raw sequence out of source.
    fn open() -> String {
        OPEN_TAG.to_string()
    }
    fn close() -> String {
        CLOSE_TAG.to_string()
    }

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
        let input = format!("{}Let me think about this{}", open(), close());
        let chunks = state.process_chunk(&input);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, "Let me think about this");
        assert!(chunks[0].1);
    }

    #[test]
    fn test_think_then_response() {
        let mut state = ReasoningState::default();
        let input = format!("{}Reasoning here{}Okay, the answer is 42", open(), close());
        let chunks = state.process_chunk(&input);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].0, "Reasoning here");
        assert!(chunks[0].1);
        assert_eq!(chunks[1].0, "Okay, the answer is 42");
        assert!(!chunks[1].1);
    }

    #[test]
    fn test_response_then_think() {
        let mut state = ReasoningState::default();
        let input = format!("First I'll analyze{}deep reasoning{}then respond", open(), close());
        let chunks = state.process_chunk(&input);
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
        // Closing tag split across two chunks: `</thi` | `nk>`
        let c1 = state.process_chunk(&format!("{}Part one</thi", open()));
        assert_eq!(c1.len(), 1);
        assert_eq!(c1[0].0, "Part one");
        assert!(c1[0].1);

        let c2 = state.process_chunk(&format!("nk>Part two{}", close()));
        assert_eq!(c2.len(), 1);
        assert_eq!(c2[0].0, "Part two");
        assert!(!c2[0].1);
    }

    #[test]
    fn test_literal_close_tag_outside_think() {
        let mut state = ReasoningState::default();
        let input = format!("Use {} here", close());
        let chunks = state.process_chunk(&input);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, input);
        assert!(!chunks[0].1);
    }

    #[test]
    fn test_open_tag_split_across_chunks() {
        let mut state = ReasoningState::default();
        // Opening tag split: `<thi` | `nk>answer`
        let c1 = state.process_chunk("prefix <thi");
        assert_eq!(c1.len(), 1);
        assert_eq!(c1[0].0, "prefix ");
        assert!(!c1[0].1);

        let c2 = state.process_chunk("nk>the answer");
        assert_eq!(c2.len(), 1);
        assert_eq!(c2[0].0, "the answer");
        assert!(c2[0].1);
    }

    #[test]
    fn test_empty_chunk() {
        let mut state = ReasoningState::default();
        assert!(state.process_chunk("").is_empty());
        assert!(state.finalize().is_none());
    }
}
