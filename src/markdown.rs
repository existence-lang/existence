use regex::Regex;
use serde::Serialize;
use std::path::Path;

/// A parsed ontology node from a markdown file.
#[derive(Debug, Serialize)]
pub struct Node {
    pub title: String,
    pub ontology: Option<String>,
    pub axiology: Option<String>,
    pub ethics: Option<String>,
    pub epistemology: Option<String>,
    /// Raw full content
    #[serde(skip)]
    #[allow(dead_code)]
    pub raw: String,
}

impl Node {
    /// Parse a markdown file into a Node.
    pub fn parse(content: &str) -> Result<Self, String> {
        let title = extract_title(content).ok_or("No title (# heading) found")?;
        let ontology = extract_section(content, "Ontology");
        let axiology = extract_section(content, "Axiology");
        let ethics = extract_section(content, "Ethics");
        let epistemology = extract_section(content, "Epistemology");

        Ok(Node {
            title,
            ontology,
            axiology,
            ethics,
            epistemology,
            raw: content.to_string(),
        })
    }
}

/// Extract the title from `# Title` at start of file.
fn extract_title(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") && !trimmed.starts_with("## ") {
            return Some(trimmed.trim_start_matches("# ").to_string());
        }
    }
    None
}

/// Extract content under a `## [Section]` or `## Section` heading,
/// up to the next `## ` heading.
fn extract_section(content: &str, section_name: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut start = None;
    let mut end = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        // Match both `## [Ontology](./ontology.md)` and `## Ontology`
        if trimmed.starts_with("## ") && !trimmed.starts_with("### ") {
            let heading_text = trimmed.trim_start_matches("## ");
            // Check if section name appears (possibly within a link)
            if heading_text.contains(section_name) {
                start = Some(i + 1);
            } else if start.is_some() && end.is_none() {
                end = Some(i);
            }
        }
    }

    let start = start?;
    let end = end.unwrap_or(lines.len());
    let section: String = lines[start..end].join("\n").trim().to_string();
    if section.is_empty() {
        None
    } else {
        Some(section)
    }
}

/// Extract all `[term](./term.md)` links from content.
pub fn extract_links(content: &str) -> Vec<String> {
    let re = Regex::new(r"\[([^\]]+)\]\(\./([a-z0-9_-]+)\.md\)").unwrap();
    re.captures_iter(content)
        .map(|cap| cap[2].to_string())
        .collect()
}

/// Extract unique links from a file.
pub fn extract_unique_links(content: &str) -> Vec<String> {
    let mut links = extract_links(content);
    links.sort();
    links.dedup();
    links
}

/// Extract all `http(s)` references: HTML `href="..."` anchors and markdown
/// `[text](https://...)` links. Sorted and deduplicated.
pub fn extract_external_links(content: &str) -> Vec<String> {
    let re = Regex::new(r#"href="(https?://[^"\s]+)"|\]\((https?://[^)\s]+)\)"#).unwrap();
    let mut links: Vec<String> = re
        .captures_iter(content)
        .filter_map(|cap| cap.get(1).or_else(|| cap.get(2)))
        .map(|m| m.as_str().to_string())
        .collect();
    links.sort();
    links.dedup();
    links
}

/// The lay definition: the first non-empty line under `## [Ontology]`
/// (SPEC rule 2). `None` when the node has no Ontology section.
pub fn extract_definition(content: &str) -> Option<String> {
    let node = Node::parse(content).ok()?;
    let ontology = node.ontology?;
    ontology
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// Strip markdown links, emphasis, and code ticks from a line of prose.
pub fn plain_text(text: &str) -> String {
    let link = Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap();
    link.replace_all(text, "$1")
        .replace("**", "")
        .replace(['`', '*'], "")
        .trim()
        .to_string()
}

/// List all term names from `src/*.md` files.
pub fn list_terms(src_dir: &Path) -> Result<Vec<String>, String> {
    let mut terms = Vec::new();
    let entries = std::fs::read_dir(src_dir)
        .map_err(|e| format!("Cannot read {}: {e}", src_dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            terms.push(stem.to_string());
        }
    }
    terms.sort();
    Ok(terms)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# Existence

## [Ontology](./ontology.md)

Everything that 'is', or more simply, everything.

A [scoped](./scope.md) Existence, representing an [Entity's](./entity.md) [perspective](./perspective.md).

## [Axiology](./axiology.md)

Existence is the Universal Set of everything, including itself.

## [Epistemology](./epistemology.md)

Contrary to some [cultural](./culture.md) [definitions](./definition.md), Existence includes all thoughts.
"#;

    #[test]
    fn test_parse_node() {
        let node = Node::parse(SAMPLE).unwrap();
        assert_eq!(node.title, "Existence");
        assert!(node.ontology.is_some());
        assert!(node.axiology.is_some());
        assert!(node.epistemology.is_some());
        assert!(node.ethics.is_none());
    }

    #[test]
    fn test_extract_title() {
        assert_eq!(
            extract_title("# Existence\n\nstuff"),
            Some("Existence".to_string())
        );
        assert_eq!(
            extract_title("## Not Title\n# Real Title"),
            Some("Real Title".to_string())
        );
        assert_eq!(extract_title("no heading"), None);
    }

    #[test]
    fn test_extract_links() {
        let links = extract_links(
            "[scope](./scope.md) and [entity](./entity.md) plus [pattern](./pattern.md)",
        );
        assert_eq!(links, vec!["scope", "entity", "pattern"]);
    }

    #[test]
    fn test_extract_external_links() {
        let links = extract_external_links(
            "<a href=\"https://b.example/x\" target=\"_blank\">x</a> [y](https://a.example/y) \
             [local](./scope.md) <a href=\"https://b.example/x\">again</a>",
        );
        assert_eq!(links, vec!["https://a.example/y", "https://b.example/x"]);
    }

    #[test]
    fn test_extract_definition_and_plain_text() {
        assert_eq!(
            extract_definition(SAMPLE).as_deref(),
            Some("Everything that 'is', or more simply, everything.")
        );
        assert_eq!(extract_definition("# Title\n\n## Axiology\n\nx"), None);
        assert_eq!(
            plain_text("Any **[information](./information.md)** in `[Existence](./existence.md)`."),
            "Any information in Existence."
        );
    }

    #[test]
    fn test_extract_unique_links() {
        let links = extract_unique_links("[a](./scope.md) [b](./scope.md) [c](./entity.md)");
        assert_eq!(links, vec!["entity", "scope"]);
    }
}
