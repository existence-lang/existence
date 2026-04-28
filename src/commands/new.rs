use crate::config::Config;
use std::path::Path;

/// Create a new ontology node from the SPEC.md template.
///
/// Generates `src/<term>.md` with pre-filled sections.
/// Optionally adds the term to a ring in `existence.toml` and opens `$EDITOR`.
pub fn run(
    ontology_dir: &Path,
    term: &str,
    ring: Option<u32>,
    no_edit: bool,
    description: Option<&str>,
) -> Result<(), String> {
    let src_dir = ontology_dir.join("src");
    if !src_dir.is_dir() {
        std::fs::create_dir_all(&src_dir)
            .map_err(|e| format!("Failed to create src directory: {e}"))?;
    }

    let file_path = src_dir.join(format!("{term}.md"));
    if file_path.exists() {
        return Err(format!(
            "Term '{}' already exists at {}",
            term,
            file_path.display()
        ));
    }

    let title = titlecase(term);
    let ontology_text = description.unwrap_or("*Definition to be documented.*");

    let content = format!(
        "# {title}\n\
         \n\
         ## [Ontology](./ontology.md)\n\
         \n\
         {ontology_text}\n\
         \n\
         ## [Axiology](./axiology.md)\n\
         \n\
         *Value and significance to be documented.*\n\
         \n\
         ## [Epistemology](./epistemology.md)\n\
         \n\
         *How this concept is known and validated to be documented.*\n"
    );

    std::fs::write(&file_path, &content)
        .map_err(|e| format!("Failed to write {}: {e}", file_path.display()))?;

    // Optionally add to ring in existence.toml
    if let Some(ring_level) = ring {
        add_term_to_ring(ontology_dir, term, ring_level)?;
    }

    println!("Created {}", file_path.display());

    // Open in $EDITOR if set and --no-edit not specified
    if !no_edit && let Ok(editor) = std::env::var("EDITOR") {
        let status = std::process::Command::new(&editor)
            .arg(&file_path)
            .status()
            .map_err(|e| format!("Failed to launch editor '{editor}': {e}"))?;
        if !status.success() {
            return Err(format!("Editor '{editor}' exited with non-zero status"));
        }
    }

    Ok(())
}

/// Capitalize the first letter of each word, replacing hyphens with spaces.
fn titlecase(term: &str) -> String {
    term.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    upper + &chars.as_str().to_lowercase()
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Add a term to the specified ring in existence.toml.
///
/// Reads the TOML, modifies the ring's terms array, and writes back.
/// Uses string manipulation to preserve formatting since `toml` crate
/// serialization may reorder keys.
fn add_term_to_ring(ontology_dir: &Path, term: &str, ring_level: u32) -> Result<(), String> {
    let config_path = ontology_dir.join("existence.toml");
    if !config_path.exists() {
        return Err(format!(
            "existence.toml not found at {}. Cannot add term to ring.",
            config_path.display()
        ));
    }

    // Validate the ring exists
    let config = Config::load(&config_path)?;
    if config.get_ring(ring_level).is_none() {
        return Err(format!("Ring {ring_level} not defined in existence.toml"));
    }

    // Check if term is already in the ring
    if let Some(r) = config.get_ring(ring_level)
        && r.terms.contains(&term.to_string())
    {
        // Already present, nothing to do
        return Ok(());
    }

    // Read raw content and insert the term into the ring's terms array
    let raw = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read {}: {e}", config_path.display()))?;

    let updated = insert_term_in_toml(&raw, ring_level, term)?;

    std::fs::write(&config_path, &updated)
        .map_err(|e| format!("Failed to write {}: {e}", config_path.display()))?;

    println!("Added '{term}' to ring {ring_level} in existence.toml");
    Ok(())
}

/// Insert a term into a ring's terms array in raw TOML text.
///
/// Finds the `terms = [...]` line under `[rings.N]` and appends the new term.
fn insert_term_in_toml(raw: &str, ring_level: u32, term: &str) -> Result<String, String> {
    let ring_header = format!("[rings.{}]", ring_level);
    let lines: Vec<&str> = raw.lines().collect();
    let mut result = Vec::new();
    let mut in_target_ring = false;
    let mut term_inserted = false;

    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();

        // Detect ring section headers
        if trimmed.starts_with('[') {
            in_target_ring = trimmed == ring_header;
        }

        if in_target_ring && !term_inserted && trimmed.starts_with("terms") {
            // Handle multi-line terms array: collect until closing ]
            if trimmed.contains(']') {
                // Single-line terms array: terms = ["a", "b"]
                let closing = lines[i].rfind(']').unwrap();
                let before_bracket = &lines[i][..closing];
                let after_bracket = &lines[i][closing..];
                // Check if the array is empty
                if before_bracket.trim().ends_with('[') {
                    result.push(format!("{}\"{}\"{}", before_bracket, term, after_bracket));
                } else {
                    result.push(format!("{}, \"{}\"{}", before_bracket, term, after_bracket));
                }
                term_inserted = true;
            } else {
                // Multi-line: push lines until we find the closing ]
                result.push(lines[i].to_string());
                i += 1;
                while i < lines.len() {
                    let line_trimmed = lines[i].trim();
                    if line_trimmed.contains(']') {
                        // Insert before the closing bracket
                        let indent = "  ";
                        result.push(format!("{indent}\"{term}\","));
                        result.push(lines[i].to_string());
                        term_inserted = true;
                        break;
                    }
                    result.push(lines[i].to_string());
                    i += 1;
                }
            }
        } else {
            result.push(lines[i].to_string());
        }
        i += 1;
    }

    if !term_inserted {
        return Err(format!(
            "Could not find terms array for ring {ring_level} in existence.toml"
        ));
    }

    // Preserve trailing newline if original had one
    let mut output = result.join("\n");
    if raw.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_ontology(tmp: &std::path::Path) {
        let src = tmp.join("src");
        fs::create_dir_all(&src).unwrap();

        fs::write(
            tmp.join("existence.toml"),
            r#"[meta]
name = "test"
description = "test ontology"

[rings.0]
name = "kernel"
description = "core"
terms = ["existence", "entity"]

[rings.1]
name = "software"
description = "bridge"
terms = ["state"]
"#,
        )
        .unwrap();
    }

    #[test]
    fn test_titlecase_simple() {
        assert_eq!(titlecase("existence"), "Existence");
    }

    #[test]
    fn test_titlecase_hyphenated() {
        assert_eq!(titlecase("domain-model"), "Domain Model");
    }

    #[test]
    fn test_titlecase_multiple_hyphens() {
        assert_eq!(titlecase("my-long-term"), "My Long Term");
    }

    #[test]
    fn test_create_new_term() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        let result = run(tmp.path(), "focus", None, true, None);
        assert!(result.is_ok(), "Expected Ok, got: {result:?}");

        let file = tmp.path().join("src/focus.md");
        assert!(file.exists());

        let content = fs::read_to_string(&file).unwrap();
        assert!(content.starts_with("# Focus\n"));
        assert!(content.contains("## [Ontology](./ontology.md)"));
        assert!(content.contains("## [Axiology](./axiology.md)"));
        assert!(content.contains("## [Epistemology](./epistemology.md)"));
        assert!(content.contains("*Definition to be documented.*"));
    }

    #[test]
    fn test_create_with_description() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        let result = run(
            tmp.path(),
            "pattern",
            None,
            true,
            Some("Elements that repeat in a predictable manner."),
        );
        assert!(result.is_ok());

        let content = fs::read_to_string(tmp.path().join("src/pattern.md")).unwrap();
        assert!(content.contains("Elements that repeat in a predictable manner."));
        assert!(!content.contains("*Definition to be documented.*"));
    }

    #[test]
    fn test_create_hyphenated_term() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        let result = run(tmp.path(), "domain-model", None, true, None);
        assert!(result.is_ok());

        let content = fs::read_to_string(tmp.path().join("src/domain-model.md")).unwrap();
        assert!(content.starts_with("# Domain Model\n"));
    }

    #[test]
    fn test_error_if_exists() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        // Create the term first
        run(tmp.path(), "focus", None, true, None).unwrap();

        // Second attempt should fail
        let result = run(tmp.path(), "focus", None, true, None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("already exists"), "Unexpected error: {err}");
    }

    #[test]
    fn test_create_with_ring() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        let result = run(tmp.path(), "focus", Some(0), true, None);
        assert!(result.is_ok(), "Expected Ok, got: {result:?}");

        // Verify the term was added to existence.toml
        let config = Config::load(&tmp.path().join("existence.toml")).unwrap();
        let ring0 = config.get_ring(0).unwrap();
        assert!(
            ring0.terms.contains(&"focus".to_string()),
            "Expected 'focus' in ring 0 terms: {:?}",
            ring0.terms
        );
    }

    #[test]
    fn test_create_with_invalid_ring() {
        let tmp = tempfile::tempdir().unwrap();
        setup_test_ontology(tmp.path());

        let result = run(tmp.path(), "focus", Some(5), true, None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("Ring 5 not defined"),
            "Unexpected error: {err}"
        );
    }

    #[test]
    fn test_insert_term_in_toml_single_line() {
        let toml = r#"[meta]
name = "test"
description = "test"

[rings.0]
name = "kernel"
description = "core"
terms = ["existence", "entity"]
"#;
        let updated = insert_term_in_toml(toml, 0, "focus").unwrap();
        assert!(
            updated.contains(r#"terms = ["existence", "entity", "focus"]"#),
            "Got: {updated}"
        );
    }

    #[test]
    fn test_insert_term_in_toml_multi_line() {
        let toml = r#"[meta]
name = "test"
description = "test"

[rings.0]
name = "kernel"
description = "core"
terms = [
  "existence",
  "entity",
]
"#;
        let updated = insert_term_in_toml(toml, 0, "focus").unwrap();
        assert!(updated.contains("\"focus\","), "Got: {updated}");
        // Verify it parses correctly
        let config: Config = toml::from_str(&updated).unwrap();
        let ring0 = config.get_ring(0).unwrap();
        assert!(ring0.terms.contains(&"focus".to_string()));
    }

    #[test]
    fn test_insert_term_in_toml_empty_array() {
        let toml = r#"[meta]
name = "test"
description = "test"

[rings.0]
name = "kernel"
description = "core"
terms = []
"#;
        let updated = insert_term_in_toml(toml, 0, "focus").unwrap();
        assert!(updated.contains(r#"terms = ["focus"]"#), "Got: {updated}");
    }

    #[test]
    fn test_creates_src_dir_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        // Only create existence.toml, no src dir
        fs::write(
            tmp.path().join("existence.toml"),
            "[meta]\nname = \"t\"\ndescription = \"t\"\n\n[rings.0]\nname = \"k\"\ndescription = \"c\"\nterms = []\n",
        )
        .unwrap();

        let result = run(tmp.path(), "focus", None, true, None);
        assert!(result.is_ok());
        assert!(tmp.path().join("src/focus.md").exists());
    }
}
