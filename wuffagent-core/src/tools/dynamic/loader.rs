use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::{Library, Symbol};

use crate::tools::types::{
    PluginTool, Tool, ToolError, ToolLogger, ToolMetadata, ToolResult, PLUGIN_ABI_VERSION,
};

/// A handle to a dynamically loaded plugin.
///
/// The underlying `Library` is reference-counted (`Arc`), so cloning is cheap
/// and does NOT re-load the DLL. The registry stores a clone in the tool's
/// `ToolEntry` (`plugin` field) so the DLL stays mapped for as long as the
/// tool is registered: the tool's vtable lives INSIDE the plugin's DLL, so if
/// the last handle were dropped while the `Arc<dyn Tool>` was still alive,
/// `FreeLibrary` would unmap the DLL and every subsequent vtable call
/// (`name()` / `parameters_schema()` / `execute()`) would crash with
/// STATUS_ACCESS_VIOLATION.
#[derive(Clone)]
pub struct PluginHandle {
    lib: Arc<Library>,
    metadata: ToolMetadata,
    path: PathBuf,
}

/// Signature of the plugin's entry point.
// Plugin ABI: plugin exports this symbol to create a new tool instance.
// The plugin wraps its Tool via PluginTool::from_box(); the loader takes
// ownership and turns it into an Arc<dyn Tool> (see `create_tool`).
type PluginCreateFn = unsafe extern "C" fn() -> PluginTool;

impl PluginHandle {
    /// Load a plugin from the given path.
    pub fn load(path: &Path, logger: Arc<dyn ToolLogger>) -> ToolResult<Self> {
        tracing::debug!(path = %path.display(), "Loading plugin library");
        // SAFETY: libloading and raw pointer operations below are all safe in context.
        unsafe {
            let lib = Arc::new(
                Library::new(path).map_err(|e| {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "Plugin library failed to load (LoadLibrary)"
                    );
                    ToolError::PluginLoad(format!("Failed to load '{}': {}", path.display(), e))
                })?,
            );
            tracing::info!(path = %path.display(), "Plugin library loaded");

            // ABI gate FIRST: a u32 is the only FFI payload we can trust
            // before knowing the plugin's struct/vtable layouts match ours.
            // (See PLUGIN_ABI_VERSION.)
            let abi_fn: Symbol<unsafe extern "C" fn() -> u32> = lib
                .get(b"wuff_tool_abi_version")
                .map_err(|e| {
                    ToolError::PluginLoad(format!(
                        "Missing wuff_tool_abi_version symbol — plugin was built against an \
                         older wuffagent-core and must be rebuilt: {}",
                        e
                    ))
                })?;
            let plugin_abi = abi_fn();
            if plugin_abi != PLUGIN_ABI_VERSION {
                return Err(ToolError::PluginLoad(format!(
                    "Plugin ABI version mismatch: plugin has {}, WuffAgent supports {}. \
                     Rebuild the plugin against the current wuffagent-core.",
                    plugin_abi, PLUGIN_ABI_VERSION
                )));
            }
            tracing::debug!(
                path = %path.display(),
                abi = plugin_abi,
                "Plugin ABI version check passed"
            );

            // Try to read the metadata symbol
            let metadata_ptr: Symbol<unsafe extern "C" fn() -> *const ToolMetadata> = lib
                .get(b"wuff_tool_metadata")
                .map_err(|e| ToolError::PluginLoad(format!("Missing metadata symbol: {}", e)))?;
            let metadata_ref = &*metadata_ptr();
            let metadata = metadata_ref.clone();
            tracing::debug!(
                path = %path.display(),
                name = metadata.name.as_str(),
                version = metadata.version.as_str(),
                "Read plugin metadata"
            );

            if metadata.name.is_empty() {
                return Err(ToolError::PluginLoad(
                    "Plugin metadata has empty name".to_string(),
                ));
            }

            logger.log_plugin_load(path, &metadata);

            Ok(Self {
                lib,
                metadata,
                path: path.to_path_buf(),
            })
        }
    }

    /// Create a new tool instance from this plugin.
    pub fn create_tool(&self) -> ToolResult<Arc<dyn Tool>> {
        unsafe {
            tracing::debug!(
                tool = self.metadata.name.as_str(),
                "Calling plugin entry point wuff_tool_create"
            );
            let create_fn: Symbol<PluginCreateFn> = self
                .lib
                .get(b"wuff_tool_create")
                .map_err(|e| ToolError::PluginLoad(format!("Missing create symbol: {}", e)))?;
            let raw = create_fn();
            // SAFETY: The plugin is responsible for returning a valid PluginTool
            // created via PluginTool::from_box(). We take ownership and convert.
            let tool: Box<dyn Tool> = raw.into_box();
            // `Arc::from` (std's `from_box_in`) COPIES the tool into a fresh
            // ArcInner allocation (with proper strong/weak headers) and frees
            // the Box. Do NOT use `Arc::from_raw(Box::into_raw(tool))` here:
            // `from_raw` assumes the 16 refcount bytes directly before the
            // data pointer (the ArcInner header), but a plain Box allocation
            // has no such header — the Arc would then read and write its
            // counters out of bounds in NEIGHBOURING heap memory, corrupting
            // the process heap (observed: STATUS_ACCESS_VIOLATION when the
            // first agent cloned the plugin's Arc while loading its tool set).
            let arc: Arc<dyn Tool> = Arc::from(tool);
            tracing::debug!(tool = arc.name(), "Created tool instance from plugin");
            Ok(arc)
        }
    }

    /// A reference-counted clone of the underlying library handle.
    pub fn library(&self) -> Arc<Library> {
        self.lib.clone()
    }

    /// The path the plugin was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn metadata(&self) -> &ToolMetadata {
        &self.metadata
    }
}

impl Drop for PluginHandle {
    fn drop(&mut self) {
        // `strong_count` includes `self`, so 1 means this is the LAST handle:
        // the DLL is about to be unmapped.
        if Arc::strong_count(&self.lib) == 1 {
            tracing::info!(
                plugin = self.metadata.name.as_str(),
                path = %self.path.display(),
                "Plugin library unloaded (last handle dropped; FreeLibrary) — \
                 any tool whose vtable lives in this DLL must no longer be called"
            );
        } else {
            tracing::debug!(
                plugin = self.metadata.name.as_str(),
                remaining_refs = Arc::strong_count(&self.lib) - 1,
                "Plugin handle dropped (library kept alive by other handles)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::types::{ToolOutput, ToolParams, TracingToolLogger};

    /// The hello plugin DLL, when built (`cargo build -p hello_plugin`).
    fn hello_dll_path() -> Option<std::path::PathBuf> {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
        // wuffagent-core/ -> repo root -> target/debug
        let p = std::path::Path::new(&manifest)
            .parent()?
            .join("target/debug/hello_plugin.dll");
        p.exists().then_some(p)
    }

    /// Full FFI round-trip against the real hello plugin DLL: load, create,
    /// then CLONE the tool Arc many times over — exactly what
    /// `registry.list()` does every time an agent loads its tool set —
    /// calling `parameters_schema`/`execute` through the plugin's vtable in
    /// each clone, and dropping everything at the end.
    ///
    /// Regression test for the `Arc::from_raw(Box::into_raw(tool))` bug in
    /// `create_tool`: the Arc's refcounts lived 16 bytes BEFORE the Box's
    /// allocation (inside neighbouring heap objects), so every clone/drop
    /// corrupted the process heap (observed: STATUS_ACCESS_VIOLATION at the
    /// first agent tool load after a plugin was registered).
    #[test]
    fn hello_plugin_roundtrip() {
        let path = match hello_dll_path() {
            Some(p) => p,
            None => {
                eprintln!(
                    "skipping hello_plugin_roundtrip: target/debug/hello_plugin.dll not built \
                     (run `cargo build -p hello_plugin`)"
                );
                return;
            }
        };
        let logger = Arc::new(TracingToolLogger);
        let handle = PluginHandle::load(&path, logger).expect("hello_plugin should load");
        assert_eq!(handle.metadata().name, "hello");

        let tool = handle.create_tool().expect("create_tool");
        assert_eq!(tool.name(), "hello");

        for _ in 0..64 {
            let clone = Arc::clone(&tool);
            assert_eq!(clone.parameters_schema().name, "hello");
            match clone.execute(ToolParams::default()) {
                Ok(ToolOutput::Success(_)) => {}
                other => panic!("hello execute failed: {other:?}"),
            }
            drop(clone);
        }

        // The original Arc must still be fully functional after all clones
        // were dropped (the refcount bookkeeping must be intact).
        assert_eq!(tool.parameters_schema().name, "hello");
        drop(tool);
    }

    /// Regression test for the use-after-unload crash in `load_one_plugin`:
    /// the `PluginHandle` (owner of the `Library`) used to be dropped the
    /// moment `load_one_plugin` returned, which unloaded the DLL while the
    /// registered `Arc<dyn Tool>` still carried the plugin's vtable. The next
    /// vtable call — `name()`/`parameters_schema()` on the next agent turn —
    /// jumped into unmapped memory: STATUS_ACCESS_VIOLATION ("as soon as the
    /// agent loads the plugin").
    ///
    /// The registry now stores a CLONE of the handle in the entry (`plugin`
    /// field); this test mimics that: drop the original handle, keep the
    /// clone alive the way the entry does, then use the tool through its
    /// vtable.
    #[test]
    fn hello_plugin_tool_survives_handle_drop() {
        let path = match hello_dll_path() {
            Some(p) => p,
            None => {
                eprintln!(
                    "skipping hello_plugin_tool_survives_handle_drop: hello_plugin.dll not built"
                );
                return;
            }
        };
        let logger = Arc::new(TracingToolLogger);
        let handle = PluginHandle::load(&path, logger).expect("hello_plugin should load");
        let tool = handle.create_tool().expect("create_tool");

        // What the registry entry keeps alive:
        let keepalive = handle.clone();
        // What load_one_plugin used to do: drop the original right after
        // registering the tool.
        drop(handle);

        // The tool's vtable lives inside the DLL — these calls must still
        // work while the keepalive clone is alive.
        assert_eq!(tool.name(), "hello");
        assert_eq!(tool.parameters_schema().name, "hello");
        match tool.execute(ToolParams::default()) {
            Ok(ToolOutput::Success(_)) => {}
            other => panic!("hello execute failed: {other:?}"),
        }

        // Drop the tool FIRST (its Arc's drop glue runs code from the DLL),
        // then the keepalive (which unloads the DLL).
        drop(tool);
        drop(keepalive);
    }
}
