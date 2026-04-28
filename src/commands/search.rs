use crate::config::Config;
use crate::markdown::Node;
use serde::Serialize;
use std::path::Path;

/// A single search result with relevance score.
#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub term: String,
    pub ring: Option<u32>,
    pub score: u32,
    pub definition: String,
}

/// Weight constants for match types (higher = more relevant).
const WEIGHT_NAME_EXACT: u32 = 100;
const WEIGHT_NAME_SUBSTRING: u32 = 60;
const WEIGHT_DEFINITION: u32 = 30;
const WEIGHT_BODY: u32 = 10;

/// Search across ontology term files and return results ordered by relevance.
///
/// Search targets (in priority order):
/// 1. Term name — exact or substring match on filename (highest weight)
/// 2. Definition — first paragraph under `## [Ontology]` header
/// 3. Full content — any match in the full file body (lowest weight)
pub fn run(ontology_dir: &Path, query: &str, json: bool, limit: usize) -> Result<(), String> {
    let results = search(ontology_dir, query, limit)?;

    if results.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No results for \"{query}\"");
        }
        return Ok(());
    }

    if json {
        let json_output = serde_json::to_string_pretty(&results)
            .map_err(|e| format!("JSON serialization error: {e}"))?;
        println!("{json_output}");
    } else {
        for r in &results {
            let ring_label = match r.ring {
                Some(n) => format!("Ring {n}"),
                None => "Ring ?".to_string(),
            };
            println!("{} ({ring_label}) — {}", r.term, r.definition);
        }
    }

    Ok(())
}

/// Core search logic, separated for testability.
pub fn search(ontology_dir: &Path, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
    let src_dir = ontology_dir.join("src");
    if !src_dir.is_dir() {
        return Err(format!(
            "Ontology src directory not found at {}",
            src_dir.display()
        ));
    }

    // Load config for ring mapping (optional — don't fail if missing)
    let ring_map = load_ring_map(ontology_dir);

    let query_lower = query.to_lowercase();
    let mut results: Vec<SearchResult> = Vec::new();

    let entries = std::fs::read_dir(&src_dir)
        .map_err(|e| format!("Cannot read {}: {e}", src_dir.display()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let term_name = stem.to_string();

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let score = compute_score(&term_name, &content, &query_lower);
        if score == 0 {
            continue;
        }

        let definition = extract_definition(&content);
        let ring = ring_map.as_ref().and_then(|m| m.get(&term_name).copied());

        results.push(SearchResult {
            term: term_name,
            ring,
            score,
            definition,
        });
    }

    // Sort by score descending, then by term name ascending for stability
    results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.term.cmp(&b.term)));
    results.truncate(limit);

    Ok(results)
}

/// Compute a relevance score for a term against a query.
fn compute_score(term_name: &str, content: &str, query_lower: &str) -> u32 {
    let mut score = 0u32;
    let term_lower = term_name.to_lowercase();

    // 1. Term name: exact match
    if term_lower == query_lower || term_lower.replace('-', " ") == query_lower {
        score += WEIGHT_NAME_EXACT;
    }
    // Term name: substring match
    else if term_lower.contains(query_lower) || query_lower.contains(&term_lower) {
        score += WEIGHT_NAME_SUBSTRING;
    }

    // 2. Definition match (first paragraph under ## [Ontology])
    let definition = extract_definition(content);
    if definition.to_lowercase().contains(query_lower) {
        score += WEIGHT_DEFINITION;
    }

    // 3. Full body match
    if content.to_lowercase().contains(query_lower) {
        score += WEIGHT_BODY;
    }

    score
}

/// Extract the one-line definition: the first non-empty line after `## [Ontology]` or `## Ontology`.
fn extract_definition(content: &str) -> String {
    // Try parsing via Node for the ontology section
    if let Ok(node) = Node::parse(content)
        && let Some(ref ontology) = node.ontology
    {
        // Return the first non-empty line as the definition
        for line in ontology.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }

    // Fallback: return the title if available
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") && !trimmed.starts_with("## ") {
            return trimmed.trim_start_matches("# ").to_string();
        }
    }

    "(no definition)".to_string()
}

/// Build a term -> ring level map from existence.toml.
fn load_ring_map(ontology_dir: &Path) -> Option<std::collections::HashMap<String, u32>> {
    let config_path = ontology_dir.join("existence.toml");
    let config = Config::load(&config_path).ok()?;
    let mut map = std::collections::HashMap::new();
    for (level, ring) in config.rings_sorted() {
        for term in &ring.terms {
            map.insert(term.clone(), level);
        }
    }
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_ontology(tmp: &std::path::Path) {
        let src = tmp.join("src");
        fs::create_dir_all(&src).unwrap();

        // existence.toml
        fs::write(
            tmp.join("existence.toml"),
            r#"[meta]
name = "test"
description = "test ontology"

[rings.0]
name = "kernel"
description = "core"
terms = ["existence", "evolution"]

[rings.1]
name = "software"
description = "bridge"
terms = ["state"]
"#,
        )
        .unwrap();

        // existence.md
        fs::write(
            src.join("existence.md"),
            r#"# Existence

## [Ontology](./ontology.md)

Everything that 'is', or more simply, everything.

## [Axiology](./axiology.md)

Existence is the Universal Set of everything, including itself.
"#,
        )
        .unwrap();

        // evolution.md
        fs::write(
            src.join("evolution.md"),
            r#"# Evolution

## [Ontology](./ontology.md)

The altering of an Entity or lineage of Entities as a learned response to the environment.

## [Axiology](./axiology.md)

Evolution is how an Entity changes in response to its context.
"#,
        )
        .unwrap();

        // state.md
        fs::write(
            src.join("state.md"),
            r#"# State

## [Ontology](./ontology.md)

The condition of the Entity and its members.

## [Axiology](./axiology.md)

State is change captured at a moment.
"#,
        )
        .unwrap();
    }

    #[test]
    fn test_search_exact_name() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        let results = search(tmp.path(), "existence", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].term, "existence");
        assert!(results[0].score >= WEIGHT_NAME_EXACT);
    }

    #[test]
    fn test_search_substring_name() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        let results = search(tmp.path(), "exist", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].term, "existence");
    }

    #[test]
    fn test_search_body_match() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        // "change" appears in evolution and state body text
        let results = search(tmp.path(), "change", 10).unwrap();
        assert!(!results.is_empty());
        // Should find matches in body content
        let terms: Vec<&str> = results.iter().map(|r| r.term.as_str()).collect();
        assert!(
            terms.contains(&"evolution") || terms.contains(&"state"),
            "Expected to find evolution or state, got: {terms:?}"
        );
    }

    #[test]
    fn test_search_limit() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        let results = search(tmp.path(), "entity", 1).unwrap();
        assert!(results.len() <= 1);
    }

    #[test]
    fn test_search_no_results() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        let results = search(tmp.path(), "zzzznonexistent", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_ring_mapping() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        let results = search(tmp.path(), "existence", 10).unwrap();
        assert_eq!(results[0].ring, Some(0));

        let results = search(tmp.path(), "state", 10).unwrap();
        let state_result = results.iter().find(|r| r.term == "state").unwrap();
        assert_eq!(state_result.ring, Some(1));
    }

    #[test]
    fn test_extract_definition() {
        let content = r#"# Existence

## [Ontology](./ontology.md)

Everything that 'is', or more simply, everything.

## [Axiology](./axiology.md)

Some value content.
"#;
        let def = extract_definition(content);
        assert_eq!(def, "Everything that 'is', or more simply, everything.");
    }

    #[test]
    fn test_search_json_output() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        // Just test that run() doesn't error with json=true
        let result = run(tmp.path(), "existence", true, 10);
        assert!(result.is_ok());
    }

    #[test]
    fn test_search_definition_match() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        // "condition" appears only in state's definition
        let results = search(tmp.path(), "condition", 10).unwrap();
        assert!(!results.is_empty());
        let state_result = results.iter().find(|r| r.term == "state").unwrap();
        assert!(state_result.score >= WEIGHT_DEFINITION);
    }
}
