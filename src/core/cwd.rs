//! Fail-fast working-directory guard. Hosts may start the MCP server inside the
//! plugin directory; silently searching or editing there would be wrong.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `\\?\D:\x` -> `D:\x` when it is a plain drive path under MAX_PATH (not `\\?\UNC\`), else `None`.
#[cfg_attr(not(windows), allow(dead_code))]
fn strip_verbatim(s: &str) -> Option<&str> {
    let rest = s.strip_prefix(r"\\?\")?;
    let b = rest.as_bytes();
    (s.len() < 260 && b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\').then_some(rest)
}

/// Windows `canonicalize()` returns verbatim paths that cmd.exe rejects as a cwd; undo that. Identity elsewhere.
pub fn simplify(p: PathBuf) -> PathBuf {
    #[cfg(windows)]
    if let Some(r) = p.to_str().and_then(strip_verbatim) {
        return PathBuf::from(r);
    }
    p
}

/// Identity key for dedup and lock names: NTFS is case-insensitive, so fold case on Windows (key only, never open it).
pub fn key(p: &Path) -> OsString {
    if cfg!(windows) { p.to_string_lossy().to_lowercase().into() } else { p.as_os_str().to_owned() }
}

fn is_plugin_root_with(dir: &Path, env_root: Option<OsString>) -> bool {
    let env_root = env_root.and_then(|p| PathBuf::from(p).canonicalize().ok()).map(simplify);
    env_root.as_deref() == Some(dir)
        || (dir.join("plugin.json").is_file() && dir.join("mcp_config.json").is_file())
}

fn is_plugin_root(dir: &Path) -> bool {
    is_plugin_root_with(dir, std::env::var_os("PLUGIN_ROOT"))
}

/// Resolve the directory a tool call works in. An explicit `cwd` must be an
/// existing directory; without one the process cwd is used unless it is the
/// plugin's own install directory.
pub fn resolve(cwd: Option<&str>) -> Result<PathBuf, String> {
    match cwd.map(str::trim).filter(|s| !s.is_empty()) {
        Some(given) => PathBuf::from(given)
            .canonicalize()
            .ok()
            .map(simplify)
            .filter(|p| p.is_dir())
            .ok_or_else(|| format!("Error: cwd is not an existing directory: {given}")),
        None => {
            let dir = std::env::current_dir()
                .and_then(|p| p.canonicalize())
                .map(simplify)
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
        assert_eq!(resolve(tmp.to_str()).unwrap(), simplify(tmp.canonicalize().unwrap()));
        let root = tmp.join("agyfast-rs-cwd-test-plugin-root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("plugin.json"), "{}").unwrap();
        std::fs::write(root.join("mcp_config.json"), "{}").unwrap();
        assert!(is_plugin_root(&root));
        let env_only = tmp.join("agyfast-rs-cwd-test-env-only");
        std::fs::create_dir_all(&env_only).unwrap();
        let resolved = resolve(env_only.to_str()).unwrap();
        assert!(!is_plugin_root_with(&resolved, None));
        assert!(is_plugin_root_with(&resolved, Some(env_only.as_os_str().to_os_string())));
        let _ = std::fs::remove_dir_all(&env_only);
    }

    #[test]
    fn strips_only_plain_drive_verbatim_paths() {
        assert_eq!(strip_verbatim(r"\\?\D:\work\x"), Some(r"D:\work\x"));
        assert_eq!(strip_verbatim(r"\\?\UNC\srv\share"), None);
        assert_eq!(strip_verbatim(r"D:\work"), None);
        assert_eq!(strip_verbatim(&format!(r"\\?\D:\{}", "a".repeat(260))), None);
        assert_eq!(key(Path::new("/a/B")), OsString::from(if cfg!(windows) { "/a/b" } else { "/a/B" }));
    }
}
