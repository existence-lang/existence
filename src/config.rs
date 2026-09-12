use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Config {
    #[allow(dead_code)]
    pub meta: Meta,
    #[serde(default)]
    pub rings: BTreeMap<String, Ring>,
    #[serde(default)]
    pub sources: BTreeMap<String, String>,
    /// Surfaces that copy the ontology, checked by `existence audit --mirrors`.
    #[serde(default)]
    pub mirrors: Vec<Mirror>,
}

/// One `[[mirrors]]` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct Mirror {
    /// `~/…`, absolute, or relative to the ontology directory.
    pub path: String,
    /// `nodes` (node-per-file copy), `toc` (generated index), or `table`
    /// (`| **Term** | summary |` rows).
    pub kind: String,
    /// Skip silently when the path does not exist (a mirror on another machine).
    #[serde(default)]
    pub optional: bool,
    /// For `table` mirrors, how closely a row must track its node.
    /// `paraphrase` (default) checks only that every row names a real node, so
    /// a one-line summary may say the same thing in its own words.
    /// `wording` also requires the row to share vocabulary with the node's lay
    /// definition. Ignored by `nodes` and `toc`, which are always exact.
    #[serde(default = "default_compare")]
    pub compare: String,
}

fn default_compare() -> String {
    "paraphrase".to_string()
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Meta {
    pub name: String,
    pub description: String,
    /// Base IRI for RDF export; term IRIs are `{base_iri}{term}`.
    /// Defaults to `https://existence-lang.github.io/ontology/`.
    pub base_iri: Option<String>,
    /// Namespace for the `xl:` metamodel vocabulary in RDF export.
    /// Defaults to `https://existence-lang.github.io/vocab#`.
    pub vocab_iri: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Ring {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub terms: Vec<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        toml::from_str(&content).map_err(|e| format!("Failed to parse {}: {e}", path.display()))
    }

    /// Get a ring by its numeric level, parsing the string key.
    pub fn get_ring(&self, level: u32) -> Option<&Ring> {
        self.rings.get(&level.to_string())
    }

    /// Iterate rings in sorted numeric order, yielding (level, ring) pairs.
    pub fn rings_sorted(&self) -> Vec<(u32, &Ring)> {
        let mut result: Vec<(u32, &Ring)> = self
            .rings
            .iter()
            .filter_map(|(k, v)| k.parse::<u32>().ok().map(|n| (n, v)))
            .collect();
        result.sort_by_key(|(n, _)| *n);
        result
    }
}

/// Resolve the ontology root directory.
///
/// Priority:
/// 1. `--ontology <path>` flag (passed as `explicit`)
/// 2. Current directory if it contains `existence.toml`
/// 3. `~/.existence/sources/<first-source>/`
pub fn resolve_ontology_dir(explicit: Option<&Path>) -> Result<PathBuf, String> {
    // 1. Explicit flag
    if let Some(p) = explicit {
        let p = p.to_path_buf();
        if p.join("existence.toml").exists() || p.join("src").is_dir() {
            return Ok(p);
        }
        return Err(format!(
            "Specified ontology path {} does not look like an ontology directory",
            p.display()
        ));
    }

    // 2. Current directory
    let cwd = std::env::current_dir().map_err(|e| format!("Cannot get cwd: {e}"))?;
    if cwd.join("existence.toml").exists() {
        return Ok(cwd);
    }

    // 3. First source in ~/.existence/sources/
    let home = home_dir()?;
    let sources_dir = home.join(".existence").join("sources");
    if sources_dir.is_dir() {
        // Look for any directory that contains existence.toml or src/
        if let Ok(entries) = std::fs::read_dir(&sources_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    // Check org/repo structure
                    if let Ok(inner) = std::fs::read_dir(&p) {
                        for inner_entry in inner.flatten() {
                            let ip = inner_entry.path();
                            if ip.is_dir()
                                && (ip.join("existence.toml").exists() || ip.join("src").is_dir())
                            {
                                return Ok(ip);
                            }
                        }
                    }
                    // Or direct repo
                    if p.join("existence.toml").exists() || p.join("src").is_dir() {
                        return Ok(p);
                    }
                }
            }
        }
    }

    Err(
        "Cannot find ontology directory. Use --ontology <path>, cd into an ontology, or run `existence fetch`."
            .to_string(),
    )
}

pub fn home_dir() -> Result<PathBuf, String> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| "HOME environment variable not set".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config() {
        let toml_str = r#"
[meta]
name = "existential-kernel/ontology"
description = "Reference existential ontology"

[rings.0]
name = "kernel"
description = "Universal terms, always loaded"
terms = ["existence", "entity", "abstraction"]

[rings.1]
name = "software"
description = "Software engineering bridge"
terms = ["project", "model"]

[sources]
upstream = "github:existential-kernel/ontology"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.meta.name, "existential-kernel/ontology");
        assert_eq!(config.rings.len(), 2);
        assert_eq!(config.get_ring(0).unwrap().name, "kernel");
        assert_eq!(config.get_ring(0).unwrap().terms.len(), 3);
        assert_eq!(config.get_ring(1).unwrap().name, "software");
        assert_eq!(config.get_ring(1).unwrap().terms.len(), 2);
        assert_eq!(
            config.sources["upstream"],
            "github:existential-kernel/ontology"
        );
    }

    #[test]
    fn test_parse_config_no_sources() {
        let toml_str = r#"
[meta]
name = "test"
description = "test ontology"

[rings.0]
name = "kernel"
description = "core"
terms = ["existence"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.sources.is_empty());
        assert_eq!(config.rings.len(), 1);
    }

    #[test]
    fn test_rings_sorted() {
        let toml_str = r#"
[meta]
name = "test"
description = "test"

[rings.1]
name = "software"
description = "bridge"
terms = ["project"]

[rings.0]
name = "kernel"
description = "core"
terms = ["existence"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let sorted = config.rings_sorted();
        assert_eq!(sorted[0].0, 0);
        assert_eq!(sorted[0].1.name, "kernel");
        assert_eq!(sorted[1].0, 1);
        assert_eq!(sorted[1].1.name, "software");
    }

    #[test]
    fn test_resolve_explicit_path() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("existence.toml"),
            "[meta]\nname = \"t\"\ndescription = \"t\"\n",
        )
        .unwrap();
        let result = resolve_ontology_dir(Some(tmp.path()));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), tmp.path());
    }
}
