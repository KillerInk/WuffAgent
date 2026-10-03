//! Unit tests for the `registry` module (see `super`).

use super::*;
use crate::tools::builtin::{CalculationTool, ReadFileTool};
use crate::tools::types::{
    HostApi, HostEventCallback, ToolLogger, ToolOutput, ToolParams, TracingToolLogger,
    HOST_API_VERSION,
};

fn mock_logger() -> Arc<dyn ToolLogger> {
    Arc::new(TracingToolLogger)
}

fn make_entry(tool: Arc<dyn Tool>) -> ToolEntry {
    ToolEntry {
        tool,
        metadata: ToolMetadata {
            name: "test_tool".to_string(),
            version: "1.0.0".to_string(),
            description: "A test tool".to_string(),
            dependencies: vec![],
        },
        loaded_at: Instant::now(),
        plugin: None,
    }
}

#[test]
fn test_register_and_get_tool() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    let tool = Arc::new(CalculationTool::new());
    let name = tool.name().to_string();
    registry.register(make_entry(tool)).unwrap();

    let retrieved = registry.get(&name);
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().name(), name);
}

#[test]
fn test_register_duplicate_tool_fails() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    let tool = Arc::new(CalculationTool::new());
    registry.register(make_entry(tool.clone())).unwrap();

    let result = registry.register(make_entry(tool));
    assert!(result.is_err());
}

#[test]
fn test_unregister_tool() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    let tool = Arc::new(CalculationTool::new());
    let name = tool.name().to_string();
    registry.register(make_entry(tool)).unwrap();

    let result = registry.unregister(&name);
    assert!(result.is_ok());
    assert!(registry.get(&name).is_none());
}

#[test]
fn test_unregister_missing_tool_fails() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    let result = registry.unregister("nonexistent");
    assert!(result.is_err());
}

#[test]
fn test_list_tools_empty() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    assert_eq!(registry.list().len(), 0);
}

#[test]
fn test_list_tools_after_adds() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    registry
        .register(make_entry(Arc::new(CalculationTool::new())))
        .unwrap();
    registry
        .register(make_entry(Arc::new(ReadFileTool::new())))
        .unwrap();

    assert_eq!(registry.list().len(), 2);
}

#[test]
fn test_list_schemas() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    assert!(registry.list_schemas().is_empty());

    registry
        .register(make_entry(Arc::new(CalculationTool::new())))
        .unwrap();
    let schemas = registry.list_schemas();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, "calculation");
}

#[test]
fn test_to_tool_definitions() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    assert!(registry.to_tool_definitions().is_empty());

    registry
        .register(make_entry(Arc::new(CalculationTool::new())))
        .unwrap();
    let definitions = registry.to_tool_definitions();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].function.name, "calculation");
    assert_eq!(definitions[0].type_name, "function");
}

// ─── T3b: discovery paths ────────────────────────────────────────────────────

#[test]
fn test_add_discovery_path_idempotent() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    let p = PathBuf::from("/tmp/wa-test-plugins");
    assert!(registry.add_discovery_path(p.clone()), "first add is new");
    assert!(!registry.add_discovery_path(p.clone()), "second add is a no-op");
    assert_eq!(registry.discovery_paths(), vec![p]);
}

#[test]
fn test_discovery_paths_seeded_at_construction() {
    let seeded = PathBuf::from("/seeded");
    let registry = ToolRegistry::new(vec![seeded.clone()], mock_logger());
    assert_eq!(registry.discovery_paths(), vec![seeded]);
}

#[test]
fn test_discover_plugins_empty_dir_loads_nothing() {
    let dir = std::env::temp_dir().join(format!("wa-reg-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let registry = ToolRegistry::new(vec![dir.clone()], mock_logger());
    let outcomes = registry.discover_plugins_detailed().unwrap();
    assert!(outcomes.is_empty(), "empty dir has no plugin files: {:?}", outcomes);
    assert_eq!(registry.discover_plugins().unwrap(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_discover_plugins_ignores_non_plugin_files() {
    let dir = std::env::temp_dir().join(format!("wa-reg-ign-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "not a plugin").unwrap();
    let registry = ToolRegistry::new(vec![dir.clone()], mock_logger());
    let outcomes = registry.discover_plugins_detailed().unwrap();
    assert!(outcomes.is_empty(), "non-.dll/.so files are skipped: {:?}", outcomes);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_discover_plugins_broken_dll_reports_failed_not_err() {
    let dir = std::env::temp_dir().join(format!("wa-reg-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bogus.dll"), b"definitely not a PE file").unwrap();
    let registry = ToolRegistry::new(vec![dir.clone()], mock_logger());
    let outcomes = registry.discover_plugins_detailed().unwrap();
    assert_eq!(outcomes.len(), 1, "one file scanned");
    assert_eq!(outcomes[0].status, PluginLoadStatus::Failed);
    assert!(outcomes[0].error.as_ref().unwrap().contains("bogus.dll"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_plugin_tool_usable_after_discover_returns() {
    // Regression test for the use-after-unload crash: `load_one_plugin` used
    // to drop the `PluginHandle` (owner of the `Library`) when it returned,
    // which unloaded the DLL while the registered `Arc<dyn Tool>` still
    // carried the plugin's vtable. The next vtable call — e.g.
    // `to_tool_definitions` when the agent loads its tool set — crashed with
    // STATUS_ACCESS_VIOLATION.
    let manifest = match std::env::var("CARGO_MANIFEST_DIR").ok() {
        Some(m) => m,
        None => return,
    };
    let dll = std::path::Path::new(&manifest)
        .parent()
        .unwrap()
        .join("target/debug/hello_plugin.dll");
    if !dll.exists() {
        eprintln!("skipping: hello_plugin.dll not built (cargo build -p hello_plugin)");
        return;
    }
    // Copy the DLL into a scratch dir so the scan only ever sees it.
    let dir = std::env::temp_dir().join(format!("wa-reg-hello-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(&dll, dir.join("hello_plugin.dll")).unwrap();

    let registry = ToolRegistry::new(vec![dir.clone()], mock_logger());
    let outcomes = registry.discover_plugins_detailed().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, PluginLoadStatus::Loaded);
    assert_eq!(registry.list().len(), 1);

    // `discover_plugins` has returned — the handle that loaded the DLL is
    // gone from the loader's scope. Every call below goes through the
    // plugin's vtable and must still work.
    let tool = registry.get("hello").expect("hello registered");
    assert_eq!(tool.name(), "hello");
    assert_eq!(tool.parameters_schema().name, "hello");
    match tool.execute(ToolParams::default()) {
        Ok(ToolOutput::Success(_)) => {}
        other => panic!("hello execute failed: {other:?}"),
    }
    assert_eq!(registry.to_tool_definitions().len(), 1);

    // Drop the local tool clone FIRST (its Arc's drop glue lives in the DLL),
    // then the registry (entry + keepalive handle → FreeLibrary), and only
    // then remove the dir (the DLL file is locked while mapped).
    drop(tool);
    drop(registry);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─── Host API (optional wuff_tool_host_api) ─────────────────────────────────

/// A host-API vtable whose fns must never run (the tests only hand the
/// pointer across; no loaded test plugin calls it).
fn dummy_host_api() -> HostApi {
    extern "C" fn inject(_sid: *const u8, _sl: usize, _t: *const u8, _tl: usize) -> bool {
        unreachable!()
    }
    extern "C" fn create(_n: *const u8, _nl: usize, _o: *mut u8, _c: usize) -> bool {
        unreachable!()
    }
    extern "C" fn resolve(_q: *const u8, _ql: usize, _o: *mut u8, _c: usize) -> bool {
        unreachable!()
    }
    extern "C" fn switch(_s: *const u8, _sl: usize) -> bool {
        unreachable!()
    }
    extern "C" fn count() -> usize {
        unreachable!()
    }
    extern "C" fn get(
        _i: usize,
        _oi: *mut u8,
        _ci: usize,
        _on: *mut u8,
        _cn: usize,
    ) -> bool {
        unreachable!()
    }
    extern "C" fn reg(_cb: HostEventCallback, _ud: *mut std::ffi::c_void) {
        unreachable!()
    }
    HostApi {
        version: HOST_API_VERSION,
        inject_user_message: inject,
        create_session: create,
        resolve_session: resolve,
        switch_session: switch,
        session_count: count,
        get_session: get,
        register_event_callback: reg,
    }
}

#[test]
fn test_set_host_api_roundtrip() {
    let registry = ToolRegistry::new(vec![], mock_logger());
    assert!(registry.host_api_ptr().is_null(), "default is null");

    let api: *mut HostApi = Box::leak(Box::new(dummy_host_api()));
    registry.set_host_api(Some(std::ptr::NonNull::new(api).unwrap()));
    assert_eq!(
        registry.host_api_ptr() as *const () as usize,
        api as *const () as usize,
        "set pointer round-trips"
    );

    registry.set_host_api(None);
    assert!(registry.host_api_ptr().is_null(), "None clears to null");
}

#[test]
fn test_host_api_version_and_layout() {
    assert_eq!(HOST_API_VERSION, 1);
    // repr(C) sanity: a u32 plus six fn pointers, aligned to the wider of
    // the two (8 on 64-bit). Guards against accidental field reordering.
    let fnptr = std::mem::size_of::<extern "C" fn() -> bool>();
    assert_eq!(
        std::mem::align_of::<HostApi>(),
        fnptr.max(std::mem::align_of::<u32>())
    );
    assert!(std::mem::size_of::<HostApi>() >= 6 * fnptr);
}

#[test]
fn test_tool_only_plugin_loads_with_host_api_set() {
    // End-to-end for the get-missing path: hello_plugin has NO
    // `wuff_tool_host_api` export, so loading it with a host API set must
    // behave exactly as without — no call, no failure (ABI backward
    // compatible), and a re-scan still skips (no re-invocation).
    let manifest = match std::env::var("CARGO_MANIFEST_DIR").ok() {
        Some(m) => m,
        None => return,
    };
    let dll = Path::new(&manifest)
        .parent()
        .unwrap()
        .join("target/debug/hello_plugin.dll");
    if !dll.exists() {
        eprintln!("skipping: hello_plugin.dll not built (cargo build -p hello_plugin)");
        return;
    }
    let dir = std::env::temp_dir().join(format!("wa-reg-hapi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(&dll, dir.join("hello_plugin.dll")).unwrap();

    let registry = ToolRegistry::new(vec![dir.clone()], mock_logger());
    let api: *mut HostApi = Box::leak(Box::new(dummy_host_api()));
    registry.set_host_api(Some(std::ptr::NonNull::new(api).unwrap()));

    let outcomes = registry.discover_plugins_detailed().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].status,
        PluginLoadStatus::Loaded,
        "tool-only plugin unaffected by a set host API: {:?}",
        outcomes
    );

    // Re-scan: skip path — the already-registered instance keeps its
    // (missing) vtable; the host-API export is not invoked twice.
    let outcomes = registry.discover_plugins_detailed().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].status,
        PluginLoadStatus::Skipped,
        "re-scan skips the registered plugin: {:?}",
        outcomes
    );

    drop(registry);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_host_api_validate_helper() {
    // A v1 table validates.
    let api: *mut HostApi = Box::leak(Box::new(dummy_host_api()));
    let validated = HostApi::validate(api).expect("v1 table must validate");
    assert_eq!(validated.version, HOST_API_VERSION);

    // Null rejects.
    assert!(HostApi::validate(std::ptr::null::<HostApi>()).is_none());

    // A future (unrecognized) version rejects — a plugin built against a
    // newer host must degrade instead of assuming the layout.
    let mut newer = dummy_host_api();
    newer.version = HOST_API_VERSION + 1;
    let newer_ptr: *mut HostApi = Box::leak(Box::new(newer));
    assert!(HostApi::validate(newer_ptr).is_none());
}
