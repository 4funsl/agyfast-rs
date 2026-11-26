//! Fail-fast working-directory guard. Hosts may start the MCP server inside the
//! plugin directory; silently searching or editing there would be wrong.

use std::path::{Path, PathBuf};

fn is_plugin_root(dir: &Path) -> bool {
    let env_root = std::env::var_os("PLUGIN_ROOT")
        .and_then(|p| PathBuf::from(p).canonicalize().ok());
    env_root.as_deref() == Some(dir)
        || (dir.join("plugin.json").is_file() && dir.join("mcp_config.json").is_file())
}

/// Resolve the directory a tool call works in. An explicit `cwd` must be an
/// existing directory; without one the process cwd is used unless it is the
/// plugin's own install directory.
pub fn resolve(cwd: Option<&str>) -> Result<PathBuf, String> {
    match cwd.map(str::trim).filter(|s| !s.is_empty()) {
        Some(given) => PathBuf::from(given)
            .canonicalize()
            .ok()
            .filter(|p| p.is_dir())
            .ok_or_else(|| format!("Error: cwd is not an existing directory: {given}")),
        None => {
            let dir = std::env::current_dir()
                .and_then(|p| p.canonicalize())
                .map_err(|e| format!("Error: cannot read process cwd: {e}"))?;
            if is_plugin_root(&dir) {
                return Err(format!(
                    "Error: no cwd given and the process runs inside the plugin directory ({}). Pass an explicit absolute `cwd` (the workspace root).",
                    dir.display()
                ));
            }
            Ok(dir)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_and_accepts_real_dirs() {
        assert!(resolve(Some("/definitely/not/here")).is_err());
        let tmp = std::env::temp_dir();
        assert_eq!(resolve(tmp.to_str()).unwrap(), tmp.canonicalize().unwrap());
        let root = tmp.join("agy-rs-cwd-test-plugin-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("plugin.json"), "{}").unwrap();
        std::fs::write(root.join("mcp_config.json"), "{}").unwrap();
        assert!(is_plugin_root(&root));
    }
}
