//! The language servers JDC knows: which files they serve, how to start them and where a
//! project's root is.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub struct ServerConfig {
    /// Shown in messages and as the source of its problems.
    pub name: &'static str,
    /// The LSP `languageId` of its documents.
    pub language_id: &'static str,
    pub extensions: &'static [&'static str],
    /// Program name on `PATH`, and the environment variable that overrides it.
    pub program: &'static str,
    pub env_var: &'static str,
    /// A folder holding one of these files is a project root.
    pub root_markers: &'static [&'static str],
    pub init_options: fn(&Path) -> Value,
}

pub static SERVERS: &[ServerConfig] = &[ServerConfig {
    name: "ols",
    language_id: "odin",
    extensions: &["odin"],
    program: "ols",
    env_var: "JDC_OLS",
    root_markers: &["ols.json"],
    init_options: odin_options,
}];

/// ols reads `ols.json` (collections, …) from the root itself; these are the defaults.
fn odin_options(_root: &Path) -> Value {
    let mut o = json!({
        "enable_hover": true,
        "enable_references": true,
        "enable_document_symbols": true,
        "enable_format": true,
        "enable_snippets": false,
        "enable_procedure_snippet": false,
    });
    if let Some(odin) = crate::process::find_tool("odin", "JDC_ODIN") {
        o["odin_command"] = json!(odin.to_string_lossy());
    }
    o
}

/// The server for a file, by its extension.
pub fn for_path(path: &Path) -> Option<&'static ServerConfig> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    SERVERS
        .iter()
        .find(|s| s.extensions.contains(&ext.as_str()))
}

/// The project root for `file`: the nearest folder with a root marker, else the workspace
/// folder holding it, else the file's folder.
pub fn root_for(cfg: &ServerConfig, file: &Path, folders: &[PathBuf]) -> PathBuf {
    let dir = file.parent().unwrap_or(Path::new("/"));
    if let Some(d) = dir
        .ancestors()
        .find(|d| cfg.root_markers.iter().any(|m| d.join(m).is_file()))
    {
        return d.to_path_buf();
    }
    folders
        .iter()
        .filter(|f| file.starts_with(f))
        .max_by_key(|f| f.components().count())
        .cloned()
        .unwrap_or_else(|| dir.to_path_buf())
}
