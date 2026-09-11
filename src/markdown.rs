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
pub fn extract_title(content: &str) -> Option<String> {
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

/// `[text](./term.md)` and `[text](./term.md "relation")` links: capture 2 is
/// the term, capture 3 the optional title.
const NODE_LINK: &str = r#"\[([^\]]+)\]\(\./([a-z0-9_-]+)\.md(?:\s+"([^"]*)")?\)"#;

/// The SKOS relation a node link asserts (SPEC "Typed links"). An untitled
/// link is `Related`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Relation {
    Broader,
    Narrower,
    Related,
}

impl Relation {
    /// Parse a link title; `None` for anything outside the recognized vocabulary.
    pub fn parse(title: &str) -> Option<Relation> {
        match title.trim().to_ascii_lowercase().as_str() {
            "broader" => Some(Relation::Broader),
            "narrower" => Some(Relation::Narrower),
            "related" => Some(Relation::Related),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Relation::Broader => "broader",
            Relation::Narrower => "narrower",
            Relation::Related => "related",
        }
    }
}

/// One `[text](./term.md "relation")` link with its relation resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TypedLink {
    pub term: String,
    pub relation: Relation,
    /// The raw title when it was not a recognized relation (the link then
    /// counts as `Related`, and lint warns).
    pub unknown_title: Option<String>,
}

/// Extract all node links in document order, keeping duplicates.
pub fn extract_typed_links(content: &str) -> Vec<TypedLink> {
    let re = Regex::new(NODE_LINK).unwrap();
    re.captures_iter(content)
        .map(|cap| {
            let term = cap[2].to_string();
            match cap.get(3).map(|m| m.as_str()) {
                None => TypedLink {
                    term,
                    relation: Relation::Related,
                    unknown_title: None,
                },
                Some(title) => match Relation::parse(title) {
                    Some(relation) => TypedLink {
                        term,
                        relation,
                        unknown_title: None,
                    },
                    None => TypedLink {
                        term,
                        relation: Relation::Related,
                        unknown_title: Some(title.to_string()),
                    },
                },
            }
        })
        .collect()
}

/// Extract all `[term](./term.md)` link targets from content (titled or not).
pub fn extract_links(content: &str) -> Vec<String> {
    extract_typed_links(content)
        .into_iter()
        .map(|link| link.term)
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
    fn test_extract_typed_links() {
        let links = extract_typed_links(
            "[a](./information.md \"broader\") [b](./system.md \"Narrower\") \
             [c](./scope.md) [d](./scope.md \"related\") [e](./soul.md \"part-of\")",
        );
        assert_eq!(links.len(), 5);
        assert_eq!(links[0].term, "information");
        assert_eq!(links[0].relation, Relation::Broader);
        assert_eq!(links[1].relation, Relation::Narrower);
        assert_eq!(links[2].relation, Relation::Related);
        assert_eq!(links[2].unknown_title, None);
        assert_eq!(links[3].relation, Relation::Related);
        assert_eq!(links[4].relation, Relation::Related);
        assert_eq!(links[4].unknown_title.as_deref(), Some("part-of"));
        // Titled links still count as plain links.
        assert_eq!(
            extract_unique_links("[a](./b.md \"broader\") [c](./a.md)"),
            vec!["a", "b"]
        );
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
