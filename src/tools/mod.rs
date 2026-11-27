pub mod check;
pub mod edit;
pub mod outline;
pub mod search;
pub mod slices;
pub mod snapshot;

use crate::core::budget;
use serde_json::Value;

/// Lazy `call_mcp_tool` hosts sometimes send nested arrays as JSON strings.
fn unwrap_stringified(args: &mut Value) {
    let Some(map) = args.as_object_mut() else { return };
    for v in map.values_mut() {
        if let Some(parsed) = v
            .as_str()
            .filter(|s| s.trim_start().starts_with('['))
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
        {
            *v = parsed;
        }
    }
}

/// Single dispatch point shared by the MCP server and the CLI. The short names are the
/// published ones; the long names are what 0.1.0-0.1.4 published and stay as aliases.
/// `Err` is a tool-level failure whose text is still shown to the model.
pub fn call(name: &str, mut args: Value) -> Result<String, String> {
    unwrap_stringified(&mut args);
    let result = match name {
        "search" | "batch_search" => search::run(args),
        "outline" | "quick_outline" => outline::run(args),
        "read_slices" => slices::run(args),
        "edit" | "apply_edits" => edit::run(args),
        "check" | "run_checks" => check::run(args),
        other => Err(format!("Error: unknown tool `{other}`")),
    };
    // Last line of defence: no tool may exceed the inline cap.
    match result {
        Ok(s) => Ok(budget::fold(&s, budget::CAP)),
        Err(s) => Err(budget::fold(&s, budget::CAP)),
    }
}
