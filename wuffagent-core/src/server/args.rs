//! Tunable CLI arguments for `llama-server`, on top of the core
//! `--model/--host/--port/--n-gpu-layers/--ctx-size/--threads` that
//! `ServerManager` already derives from `Config`.
//!
//! All flag names verified against the local llama.cpp build
//! (`M:\repos\llama.cpp`, b11126 layout: `common/arg.cpp` +
//! `tools/server/`):
//! - `--batch-size` / `--ubatch-size` — NOT `--n-batch` / `--n-ubatch`
//!   (those names do not exist in current builds; unknown arguments are
//!   fatal: `error: invalid argument: ...` → server exits 1).
//! - `--ctx-size` — NOT `--n-ctx` (same reason).
//! - `--no-sliding-window` no longer exists in current builds (use
//!   `extra_args` for older builds).
//! - `--flash-attn` takes `on|off|auto`; `--no-kv-offload`, `--metrics`,
//!   `--no-slots`, `--no-webui`, `--parallel`, `--api-key`,
//!   `--sse-ping-interval`, `--cache-reuse`, `--sleep-idle-seconds` all
//!   exist.

use serde::{Deserialize, Serialize};

fn default_n_batch() -> u32 {
    2048
}

fn default_n_ubatch() -> u32 {
    512
}

fn default_metrics() -> bool {
    true
}

/// Server tuning options, serialized into `config.json` as `server_args`
/// (old configs load with [`ServerArgs::default`] for any missing field).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerArgs {
    /// `--batch-size` — prompt-processing batch size (default 2048).
    #[serde(default = "default_n_batch")]
    pub n_batch: u32,
    /// `--ubatch-size` — physical (GPU) batch size (default 512).
    #[serde(default = "default_n_ubatch")]
    pub n_ubatch: u32,
    /// `--parallel` — concurrent slots (0 = server default, 1).
    #[serde(default)]
    pub parallel: u32,
    /// `--flash-attn on|off` (`None` = not passed, model/server default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flash_attn: Option<bool>,
    /// `--no-kv-offload` — keep the KV cache on the CPU.
    #[serde(default)]
    pub no_kv_offload: bool,
    /// `--cache-reuse N` — min chunk size to reuse from the KV cache via
    /// KV shifting (0 = server default, off).
    #[serde(default)]
    pub cache_reuse: u32,
    /// `--sleep-idle-seconds N` — release model memory after N idle
    /// seconds (0 = server default, never).
    #[serde(default)]
    pub sleep_idle_seconds: u32,
    /// `--metrics` — expose the Prometheus `/v1/metrics` endpoint
    /// (default on).
    #[serde(default = "default_metrics")]
    pub metrics: bool,
    /// `--no-webui` — disable the bundled web UI.
    #[serde(default)]
    pub no_webui: bool,
    /// `--no-slots` — disable slot-based request handling.
    #[serde(default)]
    pub no_slots: bool,
    /// `--api-key KEY` — require this bearer key for API requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// `--sse-ping-interval N` — SSE keep-alive ping period in seconds
    /// (0 = server default).
    #[serde(default)]
    pub sse_ping_interval: u32,
    /// Raw pass-through arguments, appended last (they win on conflict).
    /// Use for flags not modeled here or for older/newer llama.cpp builds.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl Default for ServerArgs {
    fn default() -> Self {
        Self {
            n_batch: default_n_batch(),
            n_ubatch: default_n_ubatch(),
            parallel: 0,
            flash_attn: None,
            no_kv_offload: false,
            cache_reuse: 0,
            sleep_idle_seconds: 0,
            metrics: default_metrics(),
            no_webui: false,
            no_slots: false,
            api_key: None,
            sse_ping_interval: 0,
            extra_args: Vec::new(),
        }
    }
}

impl ServerArgs {
    /// The CLI arguments to append after the core server arguments.
    pub fn to_cli_args(&self) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        if self.n_batch > 0 {
            a.push(format!("--batch-size={}", self.n_batch));
        }
        if self.n_ubatch > 0 {
            a.push(format!("--ubatch-size={}", self.n_ubatch));
        }
        if self.parallel > 0 {
            a.push(format!("--parallel={}", self.parallel));
        }
        if let Some(fa) = self.flash_attn {
            a.push(if fa {
                "--flash-attn=on".to_string()
            } else {
                "--flash-attn=off".to_string()
            });
        }
        if self.no_kv_offload {
            a.push("--no-kv-offload".to_string());
        }
        if self.cache_reuse > 0 {
            a.push(format!("--cache-reuse={}", self.cache_reuse));
        }
        if self.sleep_idle_seconds > 0 {
            a.push(format!(
                "--sleep-idle-seconds={}",
                self.sleep_idle_seconds
            ));
        }
        if self.metrics {
            a.push("--metrics".to_string());
        }
        if self.no_webui {
            a.push("--no-webui".to_string());
        }
        if self.no_slots {
            a.push("--no-slots".to_string());
        }
        if let Some(key) = &self.api_key {
            a.push(format!("--api-key={}", key));
        }
        if self.sse_ping_interval > 0 {
            a.push(format!(
                "--sse-ping-interval={}",
                self.sse_ping_interval
            ));
        }
        a.extend(self.extra_args.iter().cloned());
        a
    }

    /// Rough KV-cache size estimate in GB for `n_ctx` context tokens at this
    /// `parallel` slot count. Delegates to [`kv_estimate_gb`].
    pub fn kv_estimate_gb(&self, n_ctx: u32) -> f64 {
        kv_estimate_gb(self.parallel, n_ctx)
    }
}

/// Rough KV-cache size estimate in GB for `n_ctx` context tokens at
/// `parallel` slot count, assuming a **7B-class** model (32 layers,
/// 40 KV heads, 128 head-dim, fp16 K/V ≈ 0.63 MB per token).
///
/// This is a warning heuristic, not a budget: bigger models use more,
/// smaller less; quantized KV caches (`--cache-type-k/q`) reduce it.
/// It exists so the settings UI and the server-start log can warn before
/// a 4-slot × 32K configuration OOMs the machine.
pub fn kv_estimate_gb(parallel: u32, n_ctx: u32) -> f64 {
    // Llama-2-7B class: 2 (K+V) · 32 layers · 32 KV heads · 128 head-dim · 2
    // bytes (fp16) = 0.5 MB per context token. GQA models (fewer KV heads)
    // use less; this is the conservative upper bound for 7–8B models.
    const KV_BYTES_PER_TOKEN_7B: f64 = 524_288.0; // 2·32·32·128·2
    (n_ctx as f64) * (parallel.max(1) as f64) * KV_BYTES_PER_TOKEN_7B / 1.0e9
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_args_produce_expected_cli() {
        let args = ServerArgs::default();
        let cli = args.to_cli_args();
        assert_eq!(
            cli,
            vec![
                "--batch-size=2048".to_string(),
                "--ubatch-size=512".to_string(),
                "--metrics".to_string()
            ]
        );
        // No flags for zero-valued / off-by-default options.
        assert!(!cli.iter().any(|a| a.starts_with("--parallel")));
        assert!(!cli.iter().any(|a| a.starts_with("--flash-attn")));
        assert!(!cli.iter().any(|a| a.starts_with("--sleep-idle")));
        assert!(!cli.iter().any(|a| a.starts_with("--cache-reuse")));
    }

    #[test]
    fn all_options_enabled_produce_all_flags() {
        let args = ServerArgs {
            n_batch: 4096,
            n_ubatch: 1024,
            parallel: 4,
            flash_attn: Some(true),
            no_kv_offload: true,
            cache_reuse: 64,
            sleep_idle_seconds: 120,
            metrics: true,
            no_webui: true,
            no_slots: true,
            api_key: Some("secret".to_string()),
            sse_ping_interval: 15,
            extra_args: vec!["--no-sliding-window".to_string()],
        };
        let cli = args.to_cli_args();
        let expected = [
            "--batch-size=4096",
            "--ubatch-size=1024",
            "--parallel=4",
            "--flash-attn=on",
            "--no-kv-offload",
            "--cache-reuse=64",
            "--sleep-idle-seconds=120",
            "--metrics",
            "--no-webui",
            "--no-slots",
            "--api-key=secret",
            "--sse-ping-interval=15",
            "--no-sliding-window",
        ];
        for flag in &expected {
            assert!(cli.contains(&flag.to_string()), "missing {flag} in {cli:?}");
        }
    }

    #[test]
    fn flash_attn_off_is_explicit() {
        let args = ServerArgs {
            flash_attn: Some(false),
            ..Default::default()
        };
        assert!(args
            .to_cli_args()
            .contains(&"--flash-attn=off".to_string()));
    }

    #[test]
    fn serde_empty_object_uses_defaults() {
        // Old config files have no `server_args` field at all; a hand-edited
        // empty object must also load with the documented defaults.
        let args: ServerArgs = serde_json::from_str("{}").unwrap();
        assert_eq!(args, ServerArgs::default());
    }

    #[test]
    fn serde_roundtrip_preserves_all_fields() {
        let args = ServerArgs {
            n_batch: 100,
            n_ubatch: 50,
            parallel: 2,
            flash_attn: Some(false),
            no_kv_offload: true,
            cache_reuse: 32,
            sleep_idle_seconds: 60,
            metrics: false,
            no_webui: true,
            no_slots: true,
            api_key: Some("k".to_string()),
            sse_ping_interval: 10,
            extra_args: vec!["--foo".to_string()],
        };
        let json = serde_json::to_string(&args).unwrap();
        let back: ServerArgs = serde_json::from_str(&json).unwrap();
        assert_eq!(args, back);
    }

    #[test]
    fn serde_partial_object_fills_missing_fields() {
        let args: ServerArgs =
            serde_json::from_str(r#"{"parallel": 4, "metrics": false}"#).unwrap();
        assert_eq!(args.parallel, 4);
        assert!(!args.metrics);
        assert_eq!(args.n_batch, 2048);
        assert_eq!(args.n_ubatch, 512);
    }

    #[test]
    fn kv_estimate_matches_7b_rule_of_thumb() {
        // 4 parallel slots × 16K ctx on a 7B-class model (0.5 MB/token):
        // 4 · 16384 · 524288 B = 34.4 GB.
        let a = ServerArgs { parallel: 4, ..Default::default() };
        let gb = a.kv_estimate_gb(16_384);
        assert!((gb - 34.36).abs() < 0.2, "4×16K 7B-class ≈ 34 GB, got {}", gb);
        // Single slot is exactly parallel/4 of that.
        let a1 = ServerArgs::default();
        assert!((a1.kv_estimate_gb(16_384) * 4.0 - gb).abs() < 0.01);
    }
}
