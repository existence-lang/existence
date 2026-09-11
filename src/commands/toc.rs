//! `existence toc` — a term index with lay definitions.
//!
//! One bullet per term, grouped by ring in `existence.toml` order, each
//! carrying the node's `# Title` and its lay definition (the first line of the
//! Ontology section, SPEC rule 2). Terms in `src/` that no ring declares are
//! listed as unringed, and terms a ring declares without a node file are
//! reported as missing, so the index doubles as a manifest check.
//!
//! Relative `[text](./term.md)` links inside a definition are rewritten to
//! `<base>/term.md` so an index written outside `src/` keeps its links live.

use crate::config::Config;
use crate::markdown;
use regex::Regex;
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

/// One term in the index.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TocEntry {
    /// The slug: what `lookup` takes and what the file is named.
    pub term: String,
    /// The `# Title` of the node (falls back to the slug).
    pub title: String,
    /// `<base>/<term>.md`.
    pub path: String,
    /// Lay definition as markdown with node links rebased, or `None` when the
    /// node has no Ontology section or it is empty.
    pub definition: Option<String>,
    /// Lay definition with markdown stripped.
    pub definition_text: Option<String>,
}

/// One ring section of the index.
#[derive(Debug, Serialize)]
pub struct TocRing {
    pub level: u32,
    pub name: String,
    pub description: String,
    pub terms: Vec<TocEntry>,
    /// Terms the ring declares that have no `src/<term>.md`.
    pub missing: Vec<String>,
}

/// The whole index.
#[derive(Debug, Serialize)]
pub struct Toc {
    pub name: String,
    pub description: String,
    /// Path prefix links were rebased to.
    pub base: String,
    pub rings: Vec<TocRing>,
    /// Terms in `src/` that no ring declares (alphabetical).
    pub unringed: Vec<TocEntry>,
}

/// A `--check` finding: `term` is the slug, `message` explains the problem.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TocProblem {
    pub term: String,
    pub message: String,
}

/// Build and print (or write) the index.
///
/// `base` overrides the link prefix; when it is `None` and `output` is set,
/// the prefix is the relative path from the output file's directory to
/// `src/`, otherwise `src` (an index at the repository root). With `check`,
/// nothing is emitted: problems print to stderr and the command fails when
/// there are any.
pub fn run(
    ontology_dir: &Path,
    ring: Option<u32>,
    format: &str,
    base: Option<&str>,
    output: Option<&Path>,
    check: bool,
) -> Result<(), String> {
    let base = match base {
        Some(b) => b.to_string(),
        None => default_base(ontology_dir, output),
    };
    let toc = build(ontology_dir, ring, &base)?;

    if check {
        let problems = problems(&toc);
        let count = toc.rings.iter().map(|r| r.terms.len()).sum::<usize>() + toc.unringed.len();
        if problems.is_empty() {
            println!("toc: {count} term(s), no problems.");
            return Ok(());
        }
        for p in &problems {
            eprintln!("{}: {}", p.term, p.message);
        }
        return Err(format!("{} toc problem(s) found", problems.len()));
    }

    let text = match format {
        "markdown" | "md" => to_markdown(&toc),
        "json" => {
            let mut json = serde_json::to_string_pretty(&toc)
                .map_err(|e| format!("JSON serialization error: {e}"))?;
            json.push('\n');
            json
        }
        other => {
            return Err(format!(
                "Unknown toc format '{other}' (expected \"markdown\" or \"json\")"
            ));
        }
    };

    match output {
        Some(file) => std::fs::write(file, text)
            .map_err(|e| format!("Failed to write {}: {e}", file.display())),
        None => {
            print!("{text}");
            Ok(())
        }
    }
}

/// Build the index model.
///
/// `ring` restricts the index to one ring; unringed terms are then omitted.
/// `base` is the link prefix, used verbatim (a trailing `/` is dropped).
pub fn build(ontology_dir: &Path, ring: Option<u32>, base: &str) -> Result<Toc, String> {
    let src_dir = ontology_dir.join("src");
    if !src_dir.is_dir() {
        return Err(format!("Source directory {} not found", src_dir.display()));
    }
    let config = Config::load(&ontology_dir.join("existence.toml"))?;
    if let Some(level) = ring
        && config.get_ring(level).is_none()
    {
        return Err(format!("Ring {level} not defined in existence.toml"));
    }

    let base = base.trim_end_matches('/').to_string();
    let existing_terms = markdown::list_terms(&src_dir)?;
    let link_re = Regex::new(NODE_LINK_TARGET).unwrap();

    let entry = |term: &str| -> Result<TocEntry, String> {
        let file = src_dir.join(format!("{term}.md"));
        let content = std::fs::read_to_string(&file)
            .map_err(|e| format!("Failed to read {}: {e}", file.display()))?;
        Ok(entry_from_content(term, &content, &base, &link_re))
    };

    let mut ringed: Vec<String> = Vec::new();
    let mut rings = Vec::new();
    for (level, r) in config.rings_sorted() {
        let mut terms = Vec::new();
        let mut missing = Vec::new();
        for term in &r.terms {
            if existing_terms.contains(term) {
                ringed.push(term.clone());
                if ring.is_none_or(|only| only == level) {
                    terms.push(entry(term)?);
                }
            } else {
                missing.push(term.clone());
            }
        }
        if ring.is_none_or(|only| only == level) {
            rings.push(TocRing {
                level,
                name: r.name.clone(),
                description: r.description.clone(),
                terms,
                missing,
            });
        }
    }

    let mut unringed = Vec::new();
    if ring.is_none() {
        for term in &existing_terms {
            if !ringed.contains(term) {
                unringed.push(entry(term)?);
            }
        }
    }

    Ok(Toc {
        name: config.meta.name.clone(),
        description: config.meta.description.clone(),
        base,
        rings,
        unringed,
    })
}

/// `](./term.md)`, `](./term.md#anchor)`, `](./term.md "title")` — the target
/// part of a node link, so only the path is rewritten.
const NODE_LINK_TARGET: &str = r#"\]\(\./([a-z0-9_-]+)\.md(#[^)\s]*)?((?:\s+"[^"]*")?)\)"#;

fn entry_from_content(term: &str, content: &str, base: &str, link_re: &Regex) -> TocEntry {
    let title = markdown::extract_title(content).unwrap_or_else(|| term.to_string());
    let definition = markdown::extract_definition(content)
        .map(|d| rebase_links(&d, base, link_re))
        .filter(|d| !d.is_empty());
    let definition_text = definition
        .as_deref()
        .map(markdown::plain_text)
        .filter(|d| !d.is_empty());
    TocEntry {
        term: term.to_string(),
        title,
        path: node_path(base, term),
        definition,
        definition_text,
    }
}

fn node_path(base: &str, term: &str) -> String {
    if base.is_empty() {
        format!("{term}.md")
    } else {
        format!("{base}/{term}.md")
    }
}

fn rebase_links(text: &str, base: &str, link_re: &Regex) -> String {
    let prefix = if base.is_empty() {
        String::new()
    } else {
        format!("{base}/")
    };
    link_re
        .replace_all(text, |cap: &regex::Captures| {
            format!(
                "]({prefix}{}.md{}{})",
                &cap[1],
                cap.get(2).map_or("", |m| m.as_str()),
                cap.get(3).map_or("", |m| m.as_str()),
            )
        })
        .into_owned()
}

/// The link prefix when `--base` is not given: relative path from the output
/// file's directory to `src/`, or `src` for stdout (an index at the root).
/// The link prefix for an index written at `output`: the relative path from
/// its directory to `src/`, or `src` for stdout.
pub fn default_base(ontology_dir: &Path, output: Option<&Path>) -> String {
    let Some(out) = output else {
        return "src".to_string();
    };
    let out_dir = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    match (
        std::fs::canonicalize(&out_dir),
        std::fs::canonicalize(ontology_dir.join("src")),
    ) {
        (Ok(from), Ok(to)) => relative_path(&from, &to),
        _ => "src".to_string(),
    }
}

/// `to` relative to `from` (both absolute), with `/` separators. Same
/// directory yields `.`.
fn relative_path(from: &Path, to: &Path) -> String {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - common];
    parts.extend(
        to[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// Render the index as markdown.
pub fn to_markdown(toc: &Toc) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", toc.name));
    if !toc.description.is_empty() {
        out.push_str(&format!("{}\n\n", toc.description));
    }
    for r in &toc.rings {
        out.push_str(&format!("## Ring {} — {}\n\n", r.level, r.name));
        if !r.description.is_empty() {
            out.push_str(&format!("{}\n\n", r.description));
        }
        for e in &r.terms {
            out.push_str(&bullet(e));
        }
        for m in &r.missing {
            out.push_str(&format!(
                "- `{m}` — *missing: no `{}`*\n",
                node_path(&toc.base, m)
            ));
        }
        if !r.terms.is_empty() || !r.missing.is_empty() {
            out.push('\n');
        }
    }
    if !toc.unringed.is_empty() {
        out.push_str(
            "## Unringed\n\nTerms in `src/` that no ring in `existence.toml` declares.\n\n",
        );
        for e in &toc.unringed {
            out.push_str(&bullet(e));
        }
        out.push('\n');
    }
    // One trailing newline.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

fn bullet(e: &TocEntry) -> String {
    match &e.definition {
        Some(d) => format!("- [**{}**]({}) — {d}\n", e.title, e.path),
        None => format!("- [**{}**]({}) — *(no lay definition)*\n", e.title, e.path),
    }
}

/// `--check`: a node whose Ontology section does not open with a plain
/// sentence, a ring term with no node file, or a node no ring declares.
pub fn problems(toc: &Toc) -> Vec<TocProblem> {
    let mut out = Vec::new();
    for r in &toc.rings {
        for e in &r.terms {
            if let Some(msg) = definition_problem(e) {
                out.push(TocProblem {
                    term: e.term.clone(),
                    message: msg,
                });
            }
        }
        for m in &r.missing {
            out.push(TocProblem {
                term: m.clone(),
                message: format!(
                    "declared in ring {} of existence.toml but {} does not exist",
                    r.level,
                    node_path(&toc.base, m)
                ),
            });
        }
    }
    for e in &toc.unringed {
        if let Some(msg) = definition_problem(e) {
            out.push(TocProblem {
                term: e.term.clone(),
                message: msg,
            });
        }
        out.push(TocProblem {
            term: e.term.clone(),
            message: "not declared in any ring of existence.toml".to_string(),
        });
    }
    out
}

fn definition_problem(e: &TocEntry) -> Option<String> {
    let Some(def) = &e.definition else {
        return Some("Ontology section is missing or empty (no lay definition)".to_string());
    };
    if !is_plain_sentence(def) {
        return Some(format!(
            "Ontology section does not open with a plain sentence: {}",
            preview(def)
        ));
    }
    if e.definition_text.is_none() {
        return Some("lay definition has no text once markdown is stripped".to_string());
    }
    None
}

/// A line that reads as prose rather than block markup: not a blockquote,
/// heading, list item, table row, fence, HTML tag, image, or footnote.
pub fn is_plain_sentence(line: &str) -> bool {
    let t = line.trim_start();
    if t.is_empty() {
        return false;
    }
    const BLOCK_STARTS: [&str; 10] = ["> ", "#", "- ", "+ ", "* ", "|", "```", "~~~", "<", "!["];
    if t == ">" || BLOCK_STARTS.iter().any(|s| t.starts_with(s)) {
        return false;
    }
    if t.starts_with("[^") {
        return false;
    }
    // `1. item` ordered list
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") ")) {
        return false;
    }
    true
}

fn preview(text: &str) -> String {
    const MAX: usize = 60;
    let t = text.trim();
    if t.chars().count() <= MAX {
        format!("\"{t}\"")
    } else {
        let cut: String = t.chars().take(MAX).collect();
        format!("\"{cut}…\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup(tmp: &Path) {
        let src = tmp.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            tmp.join("existence.toml"),
            r#"[meta]
name = "test/ontology"
description = "A test ontology"

[rings.0]
name = "kernel"
description = "core terms"
terms = ["existence", "entity", "ghost"]

[rings.1]
name = "software"
description = "bridge"
terms = ["state"]
"#,
        )
        .unwrap();
        fs::write(
            src.join("existence.md"),
            "# Existence\n\n## [Ontology](./ontology.md)\n\nEverything that 'is'; a [scoped](./scope.md \"narrower\") whole of [entities](./entity.md#top).\n\nMore depth.\n\n## [Axiology](./axiology.md)\n\nx\n",
        )
        .unwrap();
        fs::write(
            src.join("entity.md"),
            "# Entity\n\n## [Ontology](./ontology.md)\n\n> A quoted definition instead of a sentence.\n\n## [Axiology](./axiology.md)\n\nx\n",
        )
        .unwrap();
        fs::write(
            src.join("state.md"),
            "# State\n\n## [Ontology](./ontology.md)\n\nThe **condition** of an [Entity](./entity.md).\n",
        )
        .unwrap();
        // Unringed, and no Ontology section at all.
        fs::write(
            src.join("stray.md"),
            "# Stray\n\n## [Axiology](./axiology.md)\n\nx\n",
        )
        .unwrap();
    }

    #[test]
    fn build_groups_by_ring_and_reports_manifest_drift() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let toc = build(tmp.path(), None, "src/").unwrap();
        assert_eq!(toc.name, "test/ontology");
        assert_eq!(toc.base, "src");
        assert_eq!(toc.rings.len(), 2);
        let r0 = &toc.rings[0];
        assert_eq!((r0.level, r0.name.as_str()), (0, "kernel"));
        let terms: Vec<&str> = r0.terms.iter().map(|e| e.term.as_str()).collect();
        assert_eq!(
            terms,
            vec!["existence", "entity"],
            "manifest order, missing dropped"
        );
        assert_eq!(r0.missing, vec!["ghost"]);
        assert_eq!(toc.rings[1].terms[0].term, "state");
        let unringed: Vec<&str> = toc.unringed.iter().map(|e| e.term.as_str()).collect();
        assert_eq!(unringed, vec!["stray"]);
    }

    #[test]
    fn entries_carry_title_path_and_rebased_definition() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let toc = build(tmp.path(), None, "docs/nodes").unwrap();
        let e = &toc.rings[0].terms[0];
        assert_eq!(e.title, "Existence");
        assert_eq!(e.path, "docs/nodes/existence.md");
        assert_eq!(
            e.definition.as_deref(),
            Some(
                "Everything that 'is'; a [scoped](docs/nodes/scope.md \"narrower\") whole of [entities](docs/nodes/entity.md#top)."
            )
        );
        assert_eq!(
            e.definition_text.as_deref(),
            Some("Everything that 'is'; a scoped whole of entities.")
        );
        let stray = &toc.unringed[0];
        assert_eq!(stray.title, "Stray");
        assert_eq!(stray.definition, None);
        assert_eq!(stray.definition_text, None);
    }

    #[test]
    fn empty_base_links_to_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let toc = build(tmp.path(), Some(1), "").unwrap();
        assert_eq!(toc.rings.len(), 1);
        assert!(toc.unringed.is_empty(), "ring filter omits unringed terms");
        let e = &toc.rings[0].terms[0];
        assert_eq!(e.path, "state.md");
        assert_eq!(
            e.definition.as_deref(),
            Some("The **condition** of an [Entity](entity.md).")
        );
    }

    #[test]
    fn ring_filter_rejects_unknown_ring() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let err = build(tmp.path(), Some(7), "src").unwrap_err();
        assert!(err.contains("Ring 7 not defined"), "{err}");
    }

    #[test]
    fn markdown_rendering() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let md = to_markdown(&build(tmp.path(), None, "src").unwrap());
        let expected = "# test/ontology\n\nA test ontology\n\n\
## Ring 0 — kernel\n\ncore terms\n\n\
- [**Existence**](src/existence.md) — Everything that 'is'; a [scoped](src/scope.md \"narrower\") whole of [entities](src/entity.md#top).\n\
- [**Entity**](src/entity.md) — > A quoted definition instead of a sentence.\n\
- `ghost` — *missing: no `src/ghost.md`*\n\n\
## Ring 1 — software\n\nbridge\n\n\
- [**State**](src/state.md) — The **condition** of an [Entity](src/entity.md).\n\n\
## Unringed\n\nTerms in `src/` that no ring in `existence.toml` declares.\n\n\
- [**Stray**](src/stray.md) — *(no lay definition)*\n";
        assert_eq!(md, expected);
    }

    #[test]
    fn json_rendering_has_slug_and_title() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let toc = build(tmp.path(), None, "src").unwrap();
        let v: serde_json::Value = serde_json::to_value(&toc).unwrap();
        assert_eq!(v["base"], "src");
        assert_eq!(v["rings"][0]["level"], 0);
        assert_eq!(v["rings"][0]["terms"][0]["term"], "existence");
        assert_eq!(v["rings"][0]["terms"][0]["title"], "Existence");
        assert_eq!(v["rings"][0]["missing"][0], "ghost");
        assert_eq!(v["unringed"][0]["term"], "stray");
        assert!(v["unringed"][0]["definition"].is_null());
    }

    #[test]
    fn check_reports_blockquote_missing_file_and_unringed() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let toc = build(tmp.path(), None, "src").unwrap();
        let found = problems(&toc);
        let terms: Vec<&str> = found.iter().map(|p| p.term.as_str()).collect();
        assert_eq!(terms, vec!["entity", "ghost", "stray", "stray"]);
        assert!(
            found[0].message.contains("plain sentence"),
            "{}",
            found[0].message
        );
        assert!(
            found[1].message.contains("does not exist"),
            "{}",
            found[1].message
        );
        assert!(
            found[2].message.contains("no lay definition"),
            "{}",
            found[2].message
        );
        assert!(
            found[3].message.contains("not declared"),
            "{}",
            found[3].message
        );

        assert!(run(tmp.path(), None, "markdown", Some("src"), None, true).is_err());
        assert!(run(tmp.path(), Some(1), "markdown", Some("src"), None, true).is_ok());
    }

    #[test]
    fn plain_sentence_heuristic() {
        for ok in [
            "A sentence.",
            "  Indented prose",
            "[link](./a.md) first",
            "**Bold** start",
            "3 things",
        ] {
            assert!(is_plain_sentence(ok), "{ok:?}");
        }
        for bad in [
            "> quote",
            ">",
            "# heading",
            "- item",
            "* item",
            "+ item",
            "1. item",
            "2) item",
            "| a | b |",
            "```rust",
            "~~~",
            "<a href=\"x\">x</a>",
            "![img](x.png)",
            "[^1]: note",
            "",
        ] {
            assert!(!is_plain_sentence(bad), "{bad:?}");
        }
    }

    #[test]
    fn output_file_defaults_base_to_relative_src() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let docs = tmp.path().join("docs").join("deep");
        fs::create_dir_all(&docs).unwrap();
        let out = docs.join("TERMS.md");
        run(tmp.path(), None, "markdown", None, Some(&out), false).unwrap();
        let text = fs::read_to_string(&out).unwrap();
        assert!(text.contains("](../../src/existence.md)"), "{text}");
        assert!(
            text.contains("[entities](../../src/entity.md#top)"),
            "{text}"
        );

        let root_out = tmp.path().join("TERMS.md");
        run(tmp.path(), None, "json", None, Some(&root_out), false).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&root_out).unwrap()).unwrap();
        assert_eq!(v["base"], "src");

        let inside = tmp.path().join("src").join("README.md");
        run(tmp.path(), None, "json", None, Some(&inside), false).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&inside).unwrap()).unwrap();
        assert_eq!(v["base"], ".");
        assert_eq!(v["rings"][0]["terms"][0]["path"], "./existence.md");
    }

    #[test]
    fn relative_path_cases() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(relative_path(&p("/a/b"), &p("/a/b/src")), "src");
        assert_eq!(relative_path(&p("/a/b/docs"), &p("/a/b/src")), "../src");
        assert_eq!(relative_path(&p("/a/b/src"), &p("/a/b/src")), ".");
        assert_eq!(relative_path(&p("/x/y"), &p("/a/b/src")), "../../a/b/src");
    }

    #[test]
    fn unknown_format_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let err = run(tmp.path(), None, "yaml", Some("src"), None, false).unwrap_err();
        assert!(err.contains("Unknown toc format"), "{err}");
    }
}
