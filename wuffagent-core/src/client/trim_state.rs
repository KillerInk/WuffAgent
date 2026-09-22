//! Token estimation / calibration and trim-state methods for ChatClient.
//! Split out of the client facade (C1).

use crate::types::{Message, Usage};
use crate::trimming::ContextTrimming;

use super::session;
use super::{ChatClient, ContextOverflow};

impl ChatClient {
    /// Calibrated chars-per-token ratio (×100). Falls back to the static
    /// default until the server has reported real usage.
    pub fn chars_per_token_x100(&self) -> u32 {
        self.chars_per_token_x100
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Estimate the token count of `chars` of content using the calibrated
    /// ratio. Clamped to `chars` (a token is never shorter than 1 char, so
    /// this is a guaranteed over-estimate and never an under-estimate).
    pub fn estimate_tokens_from_chars(&self, chars: usize) -> usize {
        let c = self
            .chars_per_token_x100
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(100) as usize;
        chars.saturating_mul(100) / c
    }

    /// Char budget for a given percentage of the current n_ctx window,
    /// converted from tokens to chars via the calibrated chars-per-token
    /// ratio. Returns 0 when the window size is unknown.
    fn char_budget_pct(&self, pct: u64) -> usize {
        let n_ctx = self.n_ctx();
        if n_ctx == 0 {
            return 0;
        }
        // n_ctx tokens × (pct / 100) × (c / 100) chars/token
        let c = self
            .chars_per_token_x100
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(100) as u64;
        ((n_ctx as u64) * pct * c / 10_000) as usize
    }

    /// Trim trigger in char units for the current n_ctx: 90% of the window,
    /// converted via the calibrated ratio. Trimming only kicks in once the
    /// estimated conversation exceeds this.
    pub fn trim_trigger_chars(&self) -> usize {
        self.char_budget_pct(Self::TRIM_TRIGGER_PCT)
    }

    /// Trim target in char units for the current n_ctx: 50% of the window,
    /// converted via the calibrated ratio. Once the trigger is exceeded, the
    /// conversation is trimmed all the way down to this — far below the
    /// limit, not just under it.
    pub fn trim_target_chars(&self) -> usize {
        self.char_budget_pct(Self::TRIM_TARGET_PCT)
    }

    /// Record the estimator char count of a prompt about to be sent, so the
    /// next [`Self::calibrate_from_usage`] can compare it against the
    /// server-reported `prompt_tokens`.
    pub fn note_prompt_chars(&self, chars: usize) {
        *self.last_prompt_chars.lock().unwrap() = chars;
    }

    /// Update the chars-per-token calibration from a server-reported
    /// `usage.prompt_tokens`. Clamped to [1.0, 10.0] chars/token — anything
    /// outside that range indicates a measurement glitch (e.g. a server that
    /// counts only a subset of messages) and is ignored.
    pub fn calibrate_from_usage(&self, usage: Option<&Usage>) {
        let Some(usage) = usage else {
            return;
        };
        let prompt_tokens = usage.prompt_tokens;
        if prompt_tokens == 0 {
            return;
        }
        let chars = *self.last_prompt_chars.lock().unwrap();
        if chars == 0 {
            return;
        }
        // ratio × 100 = chars / prompt_tokens × 100
        let ratio_x100 = (chars as u64) * 100 / prompt_tokens as u64;
        let clamped = ratio_x100.clamp(100, 1000);
        self.chars_per_token_x100
            .store(clamped as u32, std::sync::atomic::Ordering::Relaxed);
        tracing::debug!(
            "calibrated chars/token to {:.2} (chars={}, prompt_tokens={})",
            clamped as f32 / 100.0,
            chars,
            prompt_tokens
        );
    }

    /// Measure the true chars/token ratio from a just-failed overflow request
    /// (the prompt we noted via [`Self::note_prompt_chars`] vs. the server's
    /// reported `n_prompt_tokens`), update the calibration, and return the
    /// **char** budget to trim the retry down to: 85% of the reported window.
    /// Falls back to the current calibration when the measurement is unusable.
    pub fn overflow_retry_char_budget(&self, ov: &ContextOverflow) -> usize {
        let chars = *self.last_prompt_chars.lock().unwrap();
        if ov.n_prompt > 0 && chars > 0 {
            let ratio_x100 = ((chars as u64) * 100 / ov.n_prompt as u64).clamp(100, 1000);
            self.chars_per_token_x100
                .store(ratio_x100 as u32, std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                "overflow measured chars/token = {:.2} (chars={}, n_prompt={})",
                ratio_x100 as f32 / 100.0,
                chars,
                ov.n_prompt
            );
        }
        let ratio_x100 = self
            .chars_per_token_x100
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(100) as u64;
        ((ov.n_ctx as u64) * 85 * ratio_x100) as usize / 10_000
    }


    pub fn trim_conversation(&self, max_messages: usize) {
        session::trim_conversation(&self.conversation, max_messages);
    }

    /// Trim the client's conversation to the given token budget.
    /// Returns the number of messages removed.
    pub fn trim_to_token_budget(&self, target_tokens: usize) -> usize {
        let trimming = ContextTrimming::new();
        let config = crate::trimming::TrimConfig::default();
        trimming.trim_conversation(&self.conversation, target_tokens, &config)
    }

    /// Trim a standalone message vec to the given token budget.
    /// Used by the agent loop to trim its own history, since streaming
    /// writes to a throwaway conversation and never updates this field.
    pub fn trim_to_token_budget_messages(
        messages: &mut Vec<Message>,
        target_tokens: usize,
    ) -> usize {
        let trimming = ContextTrimming::new();
        let config = crate::trimming::TrimConfig::default();
        trimming.trim_messages(messages, target_tokens, &config)
    }
}
