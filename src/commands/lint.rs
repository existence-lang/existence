use crate::markdown;
use std::path::Path;

/// Lint result for a single node file.
struct LintResult {
    file: String,
    errors: Vec<String>,
    warnings: Vec<String>,
}

/// Findings for one node: errors fail the run, warnings are advisory.
#[derive(Default)]
struct LintFindings {
    errors: Vec<String>,
    warnings: Vec<String>,
}

/// The recognized `###` subsection vocabulary per `##` section, as
/// (section, spelling groups) — each group lists the accepted spellings of one
/// subsection (kernel spelling first, plain spelling second where one exists).
/// Subsection headings are non-normative per SPEC.md, so anything outside a
/// section's set is a warning, not an error; sections absent from this table
/// (Axiology, Ethics) accept any subsection silently.
const SUBSECTION_VOCABULARY: [(&str, &[&[&str]]); 2] = [
    // A pattern node carries the invariant and its per-scale senses under Ontology.
    ("Ontology", &[&["Pattern"], &["Senses"]]),
    (
        "Epistemology",
        &[
            &["Cultural Definition", "Sources"],
            &["Pattern Expression", "Examples"],
        ],
    ),
];

/// Validate ontology nodes against SPEC.md rules.
///
/// Errors:
/// - Title (# Term) is required
/// - Ontology section is required
/// - Axiology section is required
/// - Epistemology section is required
/// - Broken links: references to `./term.md` where `src/term.md` doesn't exist
///
/// Warnings:
/// - Ontology `###` subsections outside `Pattern` | `Senses` (the pattern-node shape)
/// - Epistemology `###` subsections outside the recognized vocabulary
pub fn run(ontology_dir: &Path, path: Option<&str>) -> Result<(), String> {
    let src_dir = match path {
        Some(p) => {
            let p = Path::new(p);
            if p.is_dir() {
                p.to_path_buf()
            } else {
                // Lint a single file
                return lint_single_file(p, ontology_dir);
            }
        }
        None => ontology_dir.join("src"),
    };

    if !src_dir.is_dir() {
        return Err(format!("Source directory {} not found", src_dir.display()));
    }

    let existing_terms = markdown::list_terms(&src_dir)?;
    let mut results = Vec::new();

    let mut entries: Vec<_> = std::fs::read_dir(&src_dir)
        .map_err(|e| format!("Cannot read {}: {e}", src_dir.display()))?
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "md")
        })
        .collect();
    entries.sort_by_key(|e| e.path());

    for entry in entries {
        let path = entry.path();
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("unknown")
            .to_string();

        let findings = lint_content(&content, &filename, &existing_terms);
        if !findings.errors.is_empty() || !findings.warnings.is_empty() {
            results.push(LintResult {
                file: filename,
                errors: findings.errors,
                warnings: findings.warnings,
            });
        }
    }

    let mut total_errors = 0;
    let mut total_warnings = 0;
    let mut failing_files = 0;
    for result in &results {
        println!("{}:", result.file);
        for err in &result.errors {
            println!("  - {err}");
            total_errors += 1;
        }
        for warning in &result.warnings {
            println!("  - warn: {warning}");
            total_warnings += 1;
        }
        if !result.errors.is_empty() {
            failing_files += 1;
        }
        println!();
    }

    if total_errors == 0 {
        if total_warnings == 0 {
            println!("All nodes pass lint checks.");
        } else {
            println!("All nodes pass lint checks ({total_warnings} warning(s)).");
        }
        Ok(())
    } else {
        println!(
            "{total_errors} error(s) in {failing_files} file(s), {total_warnings} warning(s)."
        );
        // Return Err so the process exits with code 1
        Err(format!("{total_errors} lint error(s) found"))
    }
}

fn lint_single_file(path: &Path, ontology_dir: &Path) -> Result<(), String> {
    let src_dir = ontology_dir.join("src");
    let existing_terms = if src_dir.is_dir() {
        markdown::list_terms(&src_dir)?
    } else {
        Vec::new()
    };

    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let filename = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("unknown")
        .to_string();

    let findings = lint_content(&content, &filename, &existing_terms);
    if findings.errors.is_empty() && findings.warnings.is_empty() {
        println!("{filename}: OK");
        return Ok(());
    }
    println!("{filename}:");
    for err in &findings.errors {
        println!("  - {err}");
    }
    for warning in &findings.warnings {
        println!("  - warn: {warning}");
    }
    if findings.errors.is_empty() {
        Ok(())
    } else {
        Err(format!("{} lint error(s) found", findings.errors.len()))
    }
}

/// Reduce a heading line to comparable text: strip the `###` marker, markdown
/// links (`[Pattern](./pattern.md) Expression` -> `Pattern Expression`), and
/// surrounding whitespace.
fn normalize_heading(line: &str) -> String {
    let text = line.trim().trim_start_matches('#').trim();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close_rel) = rest[open..].find("](") else {
            break;
        };
        let close = open + close_rel;
        let Some(end_rel) = rest[close..].find(')') else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..close]);
        rest = &rest[close + end_rel + 1..];
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Warn on `###` headings that fall outside the recognized subsection vocabulary of
/// the `##` section they sit under (see `SUBSECTION_VOCABULARY`). Subsections are
/// non-normative, so this never errors.
fn check_subsections(content: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut current: Option<&(&str, &[&[&str]])> = None;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("## ") {
            current = SUBSECTION_VOCABULARY
                .iter()
                .find(|(section, _)| t.contains(section));
            continue;
        }
        let Some((section, groups)) = current else {
            continue;
        };
        if !t.starts_with("### ") {
            continue;
        }
        let heading = normalize_heading(t);
        let recognized = groups
            .iter()
            .any(|spellings| spellings.iter().any(|s| heading == *s));
        if !recognized {
            let vocabulary = groups
                .iter()
                .map(|spellings| spellings.join(" | "))
                .collect::<Vec<_>>()
                .join(", ");
            warnings.push(format!(
                "Non-standard {section} subsection `{heading}` — recognized vocabulary: {vocabulary}"
            ));
        }
    }
    warnings
}

fn lint_content(content: &str, filename: &str, existing_terms: &[String]) -> LintFindings {
    let mut errors = Vec::new();

    // Check title
    let has_title = content
        .lines()
        .any(|l| l.trim().starts_with("# ") && !l.trim().starts_with("## "));
    if !has_title {
        errors.push("Missing title (# Term)".to_string());
    }

    // Check required sections
    let has_section = |name: &str| -> bool {
        content.lines().any(|l| {
            let t = l.trim();
            t.starts_with("## ") && !t.starts_with("### ") && t.contains(name)
        })
    };

    if !has_section("Ontology") {
        errors.push("Missing required section: ## [Ontology]".to_string());
    }
    if !has_section("Axiology") {
        errors.push("Missing required section: ## [Axiology]".to_string());
    }
    if !has_section("Epistemology") {
        errors.push("Missing required section: ## [Epistemology]".to_string());
    }

    // Check broken links
    let links = markdown::extract_unique_links(content);
    for link in links {
        if !existing_terms.contains(&link) {
            errors.push(format!(
                "Broken link: [{link}](./{link}.md) — file src/{link}.md not found (referenced in {filename})"
            ));
        }
    }

    LintFindings {
        errors,
        warnings: check_subsections(content),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lint_valid_content() {
        let content = r#"# Test

## [Ontology](./ontology.md)

Definition here.

## [Axiology](./axiology.md)

Value here.

## [Epistemology](./epistemology.md)

Knowledge here.
"#;
        let errors = lint_content(content, "test.md", &[]).errors;
        // No structural errors (broken links are expected since no terms exist)
        let structural: Vec<_> = errors
            .iter()
            .filter(|e| !e.starts_with("Broken link"))
            .collect();
        assert!(structural.is_empty());
    }

    #[test]
    fn test_lint_missing_sections() {
        let content = "# Test\n\nSome content.\n";
        let errors = lint_content(content, "test.md", &[]).errors;
        assert!(errors.iter().any(|e| e.contains("Ontology")));
        assert!(errors.iter().any(|e| e.contains("Axiology")));
        assert!(errors.iter().any(|e| e.contains("Epistemology")));
    }

    #[test]
    fn test_lint_missing_title() {
        let content = "## [Ontology](./ontology.md)\n## [Axiology](./axiology.md)\n## [Epistemology](./epistemology.md)\n";
        let errors = lint_content(content, "test.md", &[]).errors;
        assert!(errors.iter().any(|e| e.contains("Missing title")));
    }

    #[test]
    fn test_recognized_subsections_both_spellings_no_warning() {
        let content = r#"# Test

## Ontology

Definition.

## Axiology

Value.

## Epistemology

### [Cultural](./culture.md) Definition

Quoted.

### [Pattern](./pattern.md) Expression

Shown.

### Sources

Quoted.

### Examples

Shown.
"#;
        let findings = lint_content(content, "test.md", &[]);
        assert!(findings.warnings.is_empty(), "got: {:?}", findings.warnings);
    }

    #[test]
    fn test_non_standard_subsection_warns_without_error() {
        let content = r#"# Test

## Ontology

Definition.

## Axiology

Value.

## Epistemology

### Example

A near miss.
"#;
        let findings = lint_content(content, "test.md", &[]);
        assert!(findings.errors.is_empty(), "got: {:?}", findings.errors);
        assert_eq!(findings.warnings.len(), 1);
        assert!(findings.warnings[0].contains("`Example`"));
        assert!(findings.warnings[0].contains("Pattern Expression | Examples"));
    }

    #[test]
    fn test_subsections_in_unlisted_sections_are_ignored() {
        let content = r#"# Test

## Ontology

Definition.

## Axiology

### Anything Goes Here

Value.

## Ethics

### Or Here

Care.

## Epistemology

Knowledge.
"#;
        let findings = lint_content(content, "test.md", &[]);
        assert!(findings.warnings.is_empty(), "got: {:?}", findings.warnings);
    }

    #[test]
    fn test_pattern_node_subsections_under_ontology_no_warning() {
        let content = r#"# Test

## Ontology

Lay definition.

### [Pattern](./pattern.md)

**Context.** Recurs at several scales.

### Senses

| Scale | Sense | What it means | How the software spells it |
|-------|-------|---------------|----------------------------|
| Product | **Thing** | The product meaning. | `thing_id` |

## Axiology

Value.

## Epistemology

### Examples

Shown.
"#;
        let findings = lint_content(content, "test.md", &[]);
        assert!(findings.warnings.is_empty(), "got: {:?}", findings.warnings);
    }

    #[test]
    fn test_non_standard_ontology_subsection_warns_without_error() {
        let content = r#"# Test

## Ontology

Definition.

### Meanings

A near miss for Senses.

## Axiology

Value.

## Epistemology

Knowledge.
"#;
        let findings = lint_content(content, "test.md", &[]);
        assert!(findings.errors.is_empty(), "got: {:?}", findings.errors);
        assert_eq!(findings.warnings.len(), 1);
        assert!(findings.warnings[0].contains("Non-standard Ontology subsection `Meanings`"));
        assert!(findings.warnings[0].contains("Pattern, Senses"));
    }

    #[test]
    fn test_normalize_heading_strips_links() {
        assert_eq!(
            normalize_heading("### [Pattern](./pattern.md) Expression"),
            "Pattern Expression"
        );
        assert_eq!(normalize_heading("###   Sources  "), "Sources");
    }

    #[test]
    fn test_lint_broken_links() {
        let content = r#"# Test

## [Ontology](./ontology.md)

Links to [foo](./foo.md) and [bar](./bar.md).

## [Axiology](./axiology.md)

Value.

## [Epistemology](./epistemology.md)

Knowledge.
"#;
        let existing = vec!["foo".to_string()];
        let errors = lint_content(content, "test.md", &existing).errors;
        // "bar" and "ontology" are broken, "foo" is OK
        assert!(errors.iter().any(|e| e.contains("bar")));
        assert!(!errors.iter().any(|e| e.contains("[foo]")));
    }
}
