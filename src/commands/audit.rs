//! `existence audit` — a regular health check of an ontology, reported as
//! JSON (the interface) or text (a view of it).
//!
//! Classes of check, each selected by a flag; with no flag every implemented
//! class runs:
//!
//! - `--structure`: `lint` errors and warnings, `toc --check` manifest
//!   problems, node links that miss the `.md` suffix (`[x](./term)`, which
//!   neither lint nor the link rebaser sees), and near-duplicate slugs
//!   (`signal`/`signals`) scored by lay-definition similarity.
//!
//! - `--contradictions`: cycles in the broader graph after inverses (`A`
//!   broader `B` and `B` broader `A`, or longer), mutual lay definitions
//!   (`A`'s first sentence links `B` and `B`'s links `A`, reported with both
//!   sentences side by side), and known terms mentioned in a lay definition
//!   without a link. All report only.
//!
//! - `--sources` (network): re-fetch every URL in `audit/sources.lock.json`,
//!   record status, page hash, and per-term quote presence, pin a Wayback
//!   snapshot for dead links. Not run unless asked for.
//!
//! - `--mirrors`: the `[[mirrors]]` declared in `existence.toml` — per-term
//!   lay-definition drift for node copies, stale generated indexes, table
//!   rows that no longer summarise their node.
//!
//! - `--semantic` (paid, needs a key): an LLM judges each lay definition
//!   against the lay definitions of the nodes it links; verdicts are cached
//!   by content hash so an unchanged ontology makes zero model calls. Report
//!   only, never on the PR path, never part of `--all`.
//!
//! `--fix` applies the safe resolutions only: appending `.md` to a
//! suffix-less link whose target node exists, replacing a dead link with its
//! pinned archive copy, and regenerating `toc` mirrors. Everything else is a
//! decision and stays reported.
//!
//! Exit status: 0 when there is no error-severity finding, 1 when there is,
//! 2 when the audit could not run (the ontology is unreadable or its
//! manifest does not parse). Warnings never fail the audit.

use crate::commands::semantic::{self, SemanticOptions};
use crate::commands::source_check::{self, SourceOptions};
use crate::commands::{lint, mirrors, toc};
use crate::config::Config;
use crate::markdown;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One audit finding.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    /// Check class: `structure` (later `contradictions`, `sources`, …).
    pub class: String,
    /// Which check produced it: `lint`, `toc`, `link_suffix`, `near_duplicate`.
    pub check: String,
    /// `error` fails the audit; `warning` is advisory.
    pub severity: String,
    /// The node the finding is about (the slug).
    pub term: String,
    pub message: String,
    /// The safe resolution, when one exists.
    pub fix: Option<String>,
    /// Whether `--fix` applied that resolution in this run.
    pub fixed: bool,
}

/// Counts over the findings.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct Summary {
    pub errors: usize,
    pub warnings: usize,
    pub fixable: usize,
    pub fixed: usize,
}

/// The audit report.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub ontology: String,
    pub classes: Vec<String>,
    pub findings: Vec<Finding>,
    pub summary: Summary,
    /// `true` when no error-severity finding remains.
    pub clean: bool,
}

/// Which classes to run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Classes {
    pub structure: bool,
    pub contradictions: bool,
    /// Network: re-fetch every cited URL against the lockfile.
    pub sources: bool,
    /// Declared mirrors of the ontology: per-term drift, stale generated indexes.
    pub mirrors: bool,
    /// Paid: an LLM judges each lay definition against its linked neighbours.
    /// Never part of `all()` or `offline()`; only `--semantic` selects it.
    pub semantic: bool,
}

impl Classes {
    /// Every class this build implements, including the network pass.
    pub fn all() -> Self {
        Classes {
            structure: true,
            contradictions: true,
            sources: true,
            mirrors: true,
            semantic: false,
        }
    }
    /// The offline classes: what runs when no class flag is given.
    pub fn offline() -> Self {
        Classes {
            structure: true,
            contradictions: true,
            sources: false,
            mirrors: true,
            semantic: false,
        }
    }
    fn any(self) -> bool {
        self.structure || self.contradictions || self.sources || self.mirrors || self.semantic
    }
}

/// Run the audit and print the report.
///
/// Returns `Ok(true)` when the audit is clean, `Ok(false)` when it has
/// error-severity findings, and `Err` when it could not run; `main` maps
/// those to exit 0 / 1 / 2.
pub fn run(
    ontology_dir: &Path,
    classes: Classes,
    format: &str,
    fix: bool,
    output: Option<&Path>,
    source_opts: &SourceOptions,
    semantic_opts: &SemanticOptions,
) -> Result<bool, String> {
    let classes = if classes.any() {
        classes
    } else {
        Classes::offline()
    };
    let report = build_with(ontology_dir, classes, fix, source_opts, semantic_opts)?;
    let text = match format {
        "json" => {
            let mut json = serde_json::to_string_pretty(&report)
                .map_err(|e| format!("JSON serialization error: {e}"))?;
            json.push('\n');
            json
        }
        "text" => to_text(&report),
        other => {
            return Err(format!(
                "Unknown audit format '{other}' (expected \"text\" or \"json\")"
            ));
        }
    };
    match output {
        Some(file) => std::fs::write(file, text)
            .map_err(|e| format!("Failed to write {}: {e}", file.display()))?,
        None => print!("{text}"),
    }
    Ok(report.clean)
}

/// Build the report, applying safe fixes first when `fix` is set so the
/// report describes the ontology as it is left. `source_opts` only matters
/// when `classes.sources` is set.
pub fn build_with(
    ontology_dir: &Path,
    classes: Classes,
    fix: bool,
    source_opts: &SourceOptions,
    semantic_opts: &SemanticOptions,
) -> Result<Report, String> {
    let src_dir = ontology_dir.join("src");
    if !src_dir.is_dir() {
        return Err(format!("Source directory {} not found", src_dir.display()));
    }
    let config = Config::load(&ontology_dir.join("existence.toml"))?;
    let mut findings = Vec::new();
    let mut class_names = Vec::new();

    if classes.structure {
        class_names.push("structure".to_string());
        findings.extend(structure(ontology_dir, fix)?);
    }
    if classes.contradictions {
        class_names.push("contradictions".to_string());
        findings.extend(contradictions(ontology_dir)?);
    }
    if classes.sources {
        class_names.push("sources".to_string());
        findings.extend(source_check::check(ontology_dir, source_opts, fix)?);
    }
    if classes.mirrors {
        class_names.push("mirrors".to_string());
        findings.extend(mirrors::check(ontology_dir, &config, fix)?);
    }
    if classes.semantic {
        class_names.push("semantic".to_string());
        findings.extend(semantic::check(ontology_dir, semantic_opts)?);
    }

    let summary = Summary {
        errors: findings.iter().filter(|f| f.severity == "error").count(),
        warnings: findings.iter().filter(|f| f.severity == "warning").count(),
        fixable: findings.iter().filter(|f| f.fix.is_some()).count(),
        fixed: findings.iter().filter(|f| f.fixed).count(),
    };
    let clean = summary.errors == 0;
    Ok(Report {
        ontology: config.meta.name,
        classes: class_names,
        findings,
        summary,
        clean,
    })
}

/// The structure class: lint, toc manifest check, suffix-less links, and
/// near-duplicate slugs.
fn structure(ontology_dir: &Path, fix: bool) -> Result<Vec<Finding>, String> {
    let src_dir = ontology_dir.join("src");
    let mut out = Vec::new();

    // Suffix-less links first: with --fix they are rewritten before lint
    // reads the files, so a fixed link never also shows as a lint finding.
    out.extend(link_suffix(&src_dir, fix)?);

    for result in lint::collect(&src_dir)? {
        let term = result.file.trim_end_matches(".md").to_string();
        for message in result.errors {
            out.push(finding("lint", "error", &term, message, None));
        }
        for message in result.warnings {
            out.push(finding("lint", "warning", &term, message, None));
        }
    }

    let index = toc::build(ontology_dir, None, "src")?;
    for problem in toc::problems(&index) {
        out.push(finding(
            "toc",
            "error",
            &problem.term,
            problem.message,
            None,
        ));
    }

    out.extend(near_duplicates(&src_dir)?);
    Ok(out)
}

fn finding(
    check: &str,
    severity: &str,
    term: &str,
    message: String,
    fix: Option<String>,
) -> Finding {
    finding_in("structure", check, severity, term, message, fix)
}

fn finding_in(
    class: &str,
    check: &str,
    severity: &str,
    term: &str,
    message: String,
    fix: Option<String>,
) -> Finding {
    Finding {
        class: class.into(),
        check: check.into(),
        severity: severity.into(),
        term: term.into(),
        message,
        fix,
        fixed: false,
    }
}

/// `[text](./term)` links with no `.md` suffix. Fixable when `src/term.md`
/// exists; with `fix`, the file is rewritten and the finding marked fixed.
fn link_suffix(src_dir: &Path, fix: bool) -> Result<Vec<Finding>, String> {
    let existing: BTreeSet<String> = markdown::list_terms(src_dir)?.into_iter().collect();
    let re = suffixless_link_re();
    let mut out = Vec::new();
    for term in &existing {
        let path = src_dir.join(format!("{term}.md"));
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        let mut seen = BTreeSet::new();
        for cap in re.captures_iter(&content) {
            let target = cap[2].to_string();
            if !seen.insert(target.clone()) {
                continue;
            }
            let fixable = existing.contains(&target);
            let message = if fixable {
                format!("link [{}](./{target}) is missing the .md suffix", &cap[1])
            } else {
                format!(
                    "link [{}](./{target}) is missing the .md suffix and src/{target}.md does not exist",
                    &cap[1]
                )
            };
            out.push(finding(
                "link_suffix",
                "error",
                term,
                message,
                fixable.then(|| format!("rewrite (./{target}) to (./{target}.md)")),
            ));
        }
        if fix {
            let fixed_content = fix_suffixless_links(&content, &existing);
            if fixed_content != content {
                std::fs::write(&path, fixed_content)
                    .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
                for f in out
                    .iter_mut()
                    .filter(|f| f.term == *term && f.fix.is_some())
                {
                    f.fixed = true;
                }
            }
        }
    }
    Ok(out)
}

/// `[text](./slug)`, `[text](./slug#anchor)`, `[text](./slug "title")` —
/// group 1 is the text, group 2 the slug, group 3 the anchor/title tail.
fn suffixless_link_re() -> Regex {
    Regex::new(r#"\[([^\]]*)\]\(\./([A-Za-z0-9_-]+)((?:#[^)\s"]*)?(?:\s+"[^"]*")?)\)"#).unwrap()
}

/// Append `.md` to every suffix-less link whose target node exists.
pub fn fix_suffixless_links(content: &str, existing: &BTreeSet<String>) -> String {
    suffixless_link_re()
        .replace_all(content, |cap: &regex::Captures| {
            if existing.contains(&cap[2]) {
                format!("[{}](./{}.md{})", &cap[1], &cap[2], &cap[3])
            } else {
                cap[0].to_string()
            }
        })
        .into_owned()
}

/// Slug pairs that share a stem once one inflectional or derivational
/// suffix is stripped (`signal`/`signals`, `agree`/`agreement`,
/// `redefine`/`redefinition`). Hyphenated compounds are skipped. Report
/// only, with the Jaccard similarity of the two lay definitions so the pair
/// reads as merge or keep.
fn near_duplicates(src_dir: &Path) -> Result<Vec<Finding>, String> {
    let terms = markdown::list_terms(src_dir)?;
    let simple: Vec<&String> = terms.iter().filter(|t| !t.contains('-')).collect();
    let mut out = Vec::new();
    for (i, a) in simple.iter().enumerate() {
        for b in &simple[i + 1..] {
            if !near(a, b) {
                continue;
            }
            let da = definition_words(src_dir, a)?;
            let db = definition_words(src_dir, b)?;
            let union = da.union(&db).count();
            let score = if union == 0 {
                0.0
            } else {
                da.intersection(&db).count() as f64 / union as f64
            };
            out.push(finding(
                "near_duplicate",
                "warning",
                a,
                format!(
                    "`{a}` and `{b}` look like the same term (definition similarity {score:.2})"
                ),
                None,
            ));
        }
    }
    Ok(out)
}

const SUFFIXES: [&str; 13] = [
    "ition", "ation", "ment", "ence", "ance", "tion", "ion", "ity", "ing", "es", "ed", "s", "e",
];

/// The candidate stems of a slug: the slug itself and the slug with any one
/// suffix stripped, each also with a trailing `e` dropped. Stems shorter
/// than four characters are discarded so `art`/`arts` do not pair.
pub fn stems(slug: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut push = |s: &str| {
        if s.len() >= 4 {
            out.insert(s.to_string());
            if let Some(r) = s.strip_suffix('e')
                && r.len() >= 4
            {
                out.insert(r.to_string());
            }
        }
    };
    push(slug);
    for suffix in SUFFIXES {
        if let Some(rest) = slug.strip_suffix(suffix) {
            push(rest);
        }
    }
    out
}

/// Two slugs are near-duplicates when they share a candidate stem.
pub fn near(a: &str, b: &str) -> bool {
    a != b && !stems(a).is_disjoint(&stems(b))
}

fn definition_words(src_dir: &Path, term: &str) -> Result<BTreeSet<String>, String> {
    let path = src_dir.join(format!("{term}.md"));
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    Ok(markdown::extract_definition(&content)
        .map(|d| markdown::plain_text(&d))
        .unwrap_or_default()
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(str::to_string)
        .collect())
}

/// The contradictions class: broader-graph cycles, mutual lay definitions,
/// and unlinked mentions of known terms.
fn contradictions(ontology_dir: &Path) -> Result<Vec<Finding>, String> {
    let src_dir = ontology_dir.join("src");
    let terms = markdown::list_terms(&src_dir)?;
    let mut nodes: BTreeMap<String, NodeView> = BTreeMap::new();
    for term in &terms {
        let path = src_dir.join(format!("{term}.md"));
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        let definition = markdown::extract_definition(&content).unwrap_or_default();
        let definition_links: Vec<String> = markdown::extract_links(&definition)
            .into_iter()
            .filter(|t| t != term && terms.contains(t))
            .collect();
        nodes.insert(
            term.clone(),
            NodeView {
                typed: markdown::extract_typed_links(&content),
                definition,
                definition_links,
            },
        );
    }

    let mut out = Vec::new();
    out.extend(broader_cycles(&nodes));
    out.extend(mutual_definitions(&nodes));
    out.extend(unlinked_mentions(&nodes));
    Ok(out)
}

struct NodeView {
    typed: Vec<markdown::TypedLink>,
    definition: String,
    definition_links: Vec<String>,
}

/// Cycles in the broader graph, with `narrower` links inverted the way
/// `export` does. Each cycle is reported once, from its smallest member.
fn broader_cycles(nodes: &BTreeMap<String, NodeView>) -> Vec<Finding> {
    let mut broader: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (term, node) in nodes {
        for link in &node.typed {
            if !nodes.contains_key(&link.term) {
                continue;
            }
            match link.relation {
                markdown::Relation::Broader => {
                    broader.entry(term).or_default().insert(&link.term);
                }
                markdown::Relation::Narrower => {
                    broader.entry(&link.term).or_default().insert(term);
                }
                markdown::Relation::Related => {}
            }
        }
    }
    let mut out = Vec::new();
    for cycle in elementary_cycles(&broader) {
        let path = cycle
            .iter()
            .chain(std::iter::once(&cycle[0]))
            .cloned()
            .collect::<Vec<_>>()
            .join(" → ");
        out.push(finding_in(
            "contradictions",
            "broader_cycle",
            "error",
            &cycle[0],
            format!("broader links form a cycle: {path}"),
            None,
        ));
    }
    out
}

/// Elementary cycles of a small directed graph, each rotated to start at its
/// smallest node and listed once, sorted. Cycles are bounded to length 6.
fn elementary_cycles(graph: &BTreeMap<&str, BTreeSet<&str>>) -> Vec<Vec<String>> {
    let mut found: BTreeSet<Vec<String>> = BTreeSet::new();
    for &start in graph.keys() {
        let mut stack = vec![start];
        walk(graph, start, start, &mut stack, &mut found);
    }
    found.into_iter().collect()
}

fn walk<'a>(
    graph: &BTreeMap<&'a str, BTreeSet<&'a str>>,
    start: &'a str,
    at: &'a str,
    stack: &mut Vec<&'a str>,
    found: &mut BTreeSet<Vec<String>>,
) {
    if stack.len() > 6 {
        return;
    }
    let Some(next) = graph.get(at) else { return };
    for &n in next {
        if n == start {
            // Only record the cycle from its smallest member so each is unique.
            if stack.iter().all(|s| *s >= start) {
                found.insert(stack.iter().map(|s| s.to_string()).collect());
            }
        } else if n > start && !stack.contains(&n) {
            stack.push(n);
            walk(graph, start, n, stack, found);
            stack.pop();
        }
    }
}

/// Lay definitions that define each other: 2-cycles and 3-cycles in the
/// graph of links that appear in first sentences only.
fn mutual_definitions(nodes: &BTreeMap<String, NodeView>) -> Vec<Finding> {
    let graph: BTreeMap<&str, BTreeSet<&str>> = nodes
        .iter()
        .map(|(t, n)| {
            (
                t.as_str(),
                n.definition_links.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    let mut out = Vec::new();
    for cycle in elementary_cycles(&graph) {
        if cycle.len() > 3 {
            continue;
        }
        let mut message = format!(
            "lay definitions define each other in a cycle: {}",
            cycle.join(" → ")
        );
        for term in &cycle {
            message.push_str(&format!(
                "\n  {term}: {}",
                markdown::plain_text(&nodes[term].definition)
            ));
        }
        out.push(finding_in(
            "contradictions",
            "mutual_definition",
            "warning",
            &cycle[0],
            message,
            None,
        ));
    }
    out
}

/// Known terms named in a lay definition that never links them: a term
/// linked once and then repeated in plain prose is ordinary writing, not a
/// finding. Slugs are matched as whole words, hyphens as spaces, with an
/// optional plural `s`; slugs shorter than four letters are skipped as too
/// ambiguous (`art`, `god`).
fn unlinked_mentions(nodes: &BTreeMap<String, NodeView>) -> Vec<Finding> {
    let link = Regex::new(r"\[[^\]]*\]\([^)]*\)").unwrap();
    let mut out = Vec::new();
    for (term, node) in nodes {
        if node.definition.is_empty() {
            continue;
        }
        let prose = link.replace_all(&node.definition, " ").to_lowercase();
        let mut hits = Vec::new();
        for other in nodes.keys() {
            if other == term || other.len() < 4 || node.definition_links.contains(other) {
                continue;
            }
            let word = regex::escape(&other.replace('-', " "));
            let re = Regex::new(&format!(r"\b{word}(s|es)?\b")).unwrap();
            if re.is_match(&prose) {
                hits.push(other.as_str());
            }
        }
        if hits.is_empty() {
            continue;
        }
        out.push(finding_in(
            "contradictions",
            "unlinked_mention",
            "warning",
            term,
            format!(
                "lay definition mentions {} without a link: {}",
                if hits.len() == 1 {
                    "a known term"
                } else {
                    "known terms"
                },
                hits.iter()
                    .map(|h| format!("`{h}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None,
        ));
    }
    out
}

/// Text rendering: one line per finding, then the summary.
pub fn to_text(report: &Report) -> String {
    let mut out = String::new();
    for f in &report.findings {
        let tag = if f.fixed {
            " (fixed)"
        } else if f.fix.is_some() {
            " (fixable)"
        } else {
            ""
        };
        let level = if f.severity == "warning" {
            "warn: "
        } else {
            ""
        };
        out.push_str(&format!(
            "{}: [{}] {level}{}{tag}\n",
            f.term, f.check, f.message
        ));
    }
    let s = &report.summary;
    out.push_str(&format!(
        "audit ({}): {} error(s), {} warning(s), {} fixable, {} fixed — {}\n",
        report.classes.join(","),
        s.errors,
        s.warnings,
        s.fixable,
        s.fixed,
        if report.clean { "clean" } else { "findings" }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn build(dir: &Path, classes: Classes, fix: bool) -> Result<Report, String> {
        build_with(
            dir,
            classes,
            fix,
            &SourceOptions::default(),
            &SemanticOptions::default(),
        )
    }

    fn node(title: &str, ontology: &str) -> String {
        format!(
            "# {title}\n\n## Ontology\n\n{ontology}\n\n## Axiology\n\nx\n\n## Epistemology\n\ny\n"
        )
    }

    fn setup(tmp: &Path) {
        let src = tmp.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            tmp.join("existence.toml"),
            "[meta]\nname = \"test/ontology\"\ndescription = \"d\"\n\n[rings.0]\nname = \"kernel\"\ndescription = \"core\"\nterms = [\"entity\", \"signal\", \"signals\", \"ghost\"]\n",
        )
        .unwrap();
        // Suffix-less links: one fixable (with a title), one to a missing node.
        fs::write(
            src.join("entity.md"),
            node(
                "Entity",
                "Anything with a [signal](./signal \"broader\") or an [echo](./echo).",
            ),
        )
        .unwrap();
        fs::write(
            src.join("signal.md"),
            node("Signal", "Information that moves between entities."),
        )
        .unwrap();
        fs::write(
            src.join("signals.md"),
            node(
                "Signals",
                "Information that moves between entities, plural.",
            ),
        )
        .unwrap();
        // Unringed, empty Ontology, missing Axiology: toc + lint findings.
        fs::write(
            src.join("stray.md"),
            "# Stray\n\n## Ontology\n\n## Epistemology\n\ny\n",
        )
        .unwrap();
    }

    fn checks(report: &Report) -> Vec<(String, String, String)> {
        report
            .findings
            .iter()
            .map(|f| (f.check.clone(), f.term.clone(), f.severity.clone()))
            .collect()
    }

    #[test]
    fn structure_reports_every_check_class() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let report = build(tmp.path(), Classes::offline(), false).unwrap();
        assert_eq!(report.ontology, "test/ontology");
        assert_eq!(report.classes, ["structure", "contradictions", "mirrors"]);
        let c = checks(&report);
        let s = |a: &str, b: &str, d: &str| (a.to_string(), b.to_string(), d.to_string());
        assert!(c.contains(&s("link_suffix", "entity", "error")));
        assert!(c.contains(&s("lint", "stray", "error")));
        assert!(c.contains(&s("toc", "stray", "error")));
        assert!(c.contains(&s("toc", "ghost", "error")));
        assert!(c.contains(&s("near_duplicate", "signal", "warning")));
        // Two suffix-less links in entity: the existing target is fixable, the
        // missing one is not.
        let links: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.check == "link_suffix")
            .collect();
        assert_eq!(links.len(), 2);
        assert_eq!(
            links[0].fix.as_deref(),
            Some("rewrite (./signal) to (./signal.md)")
        );
        assert!(links[1].message.contains("src/echo.md does not exist"));
        assert_eq!(links[1].fix, None);
        let dup = report
            .findings
            .iter()
            .find(|f| f.check == "near_duplicate")
            .unwrap();
        assert!(
            dup.message
                .starts_with("`signal` and `signals` look like the same term")
        );
        assert_eq!(report.summary.fixable, 1);
        assert_eq!(report.summary.fixed, 0);
        assert!(!report.clean);
        assert!(report.summary.errors >= 5);
    }

    #[test]
    fn fix_appends_md_only_where_the_target_exists() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let report = build(tmp.path(), Classes::offline(), true).unwrap();
        let entity = fs::read_to_string(tmp.path().join("src/entity.md")).unwrap();
        assert!(entity.contains("[signal](./signal.md \"broader\")"));
        assert!(entity.contains("[echo](./echo)"));
        let links: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.check == "link_suffix")
            .collect();
        assert!(links[0].fixed);
        assert!(!links[1].fixed);
        assert_eq!(report.summary.fixed, 1);
        // The fixed link is not also a broken-link lint error; the missing
        // one is (lint sees `./echo` as no link at all, so nothing there).
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.check == "lint" && f.term == "entity")
        );
        // A second pass finds only the unfixable link.
        let again = build(tmp.path(), Classes::offline(), false).unwrap();
        let links: Vec<&Finding> = again
            .findings
            .iter()
            .filter(|f| f.check == "link_suffix")
            .collect();
        assert_eq!(links.len(), 1);
        assert!(links[0].message.contains("echo"));
    }

    #[test]
    fn suffixless_rewrite_preserves_anchor_and_title() {
        let existing: BTreeSet<String> = ["scope".to_string(), "entity".to_string()].into();
        let text =
            "[a](./scope) [b](./scope#top) [c](./entity \"narrower\") [d](./gone) [e](./scope.md)";
        assert_eq!(
            fix_suffixless_links(text, &existing),
            "[a](./scope.md) [b](./scope.md#top) [c](./entity.md \"narrower\") [d](./gone) [e](./scope.md)"
        );
    }

    #[test]
    fn stems_pair_the_known_near_duplicates_only() {
        for (a, b) in [
            ("signal", "signals"),
            ("human", "humans"),
            ("agree", "agreement"),
            ("redefine", "redefinition"),
            ("abstract", "abstraction"),
            ("exist", "existence"),
        ] {
            assert!(near(a, b), "{a} / {b}");
        }
        for (a, b) in [
            ("state", "story"),
            ("scope", "soul"),
            ("art", "arts"),
            ("spirit", "spirituality"),
            ("belief", "believe"),
        ] {
            assert!(!near(a, b), "{a} / {b}");
        }
    }

    #[test]
    fn clean_ontology_is_clean_and_json_has_the_report_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            tmp.path().join("existence.toml"),
            "[meta]\nname = \"t\"\ndescription = \"d\"\n\n[rings.0]\nname = \"k\"\ndescription = \"c\"\nterms = [\"entity\"]\n",
        )
        .unwrap();
        fs::write(src.join("entity.md"), node("Entity", "A thing.")).unwrap();
        let report = build(tmp.path(), Classes::default(), false).unwrap();
        assert!(report.findings.is_empty());
        assert!(report.clean);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "ontology": "t", "classes": [], "findings": [],
                "summary": {"errors": 0, "warnings": 0, "fixable": 0, "fixed": 0},
                "clean": true
            })
        );
        assert_eq!(
            to_text(&report),
            "audit (): 0 error(s), 0 warning(s), 0 fixable, 0 fixed — clean\n"
        );
        // `run` with no class flag runs every class and reports clean.
        let out = tmp.path().join("report.json");
        assert!(
            run(
                tmp.path(),
                Classes::default(),
                "json",
                false,
                Some(&out),
                &SourceOptions::default(),
                &SemanticOptions::default()
            )
            .unwrap()
        );
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(
            written["classes"],
            serde_json::json!(["structure", "contradictions", "mirrors"])
        );
        assert!(
            run(
                tmp.path(),
                Classes::offline(),
                "yaml",
                false,
                None,
                &SourceOptions::default(),
                &SemanticOptions::default()
            )
            .is_err()
        );
    }

    fn contra_setup(tmp: &Path) {
        let src = tmp.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            tmp.join("existence.toml"),
            "[meta]\nname = \"t\"\ndescription = \"d\"\n\n[rings.0]\nname = \"k\"\ndescription = \"c\"\nterms = [\"entity\", \"information\", \"system\", \"scope\", \"domain\", \"focus\", \"story\"]\n",
        )
        .unwrap();
        // Planted 2-cycle in lay definitions, plus an unlinked mention of `story`.
        fs::write(
            src.join("entity.md"),
            node("Entity", "Any [information](./information.md) in a story."),
        )
        .unwrap();
        fs::write(
            src.join("information.md"),
            node(
                "Information",
                "That which distinguishes; an [Entity](./entity.md).",
            ),
        )
        .unwrap();
        // Broader cycle: system broader scope, scope narrower system (inverse
        // agrees), scope broader domain, domain broader system.
        fs::write(
            src.join("system.md"),
            node("System", "A whole; see [scope](./scope.md \"broader\")."),
        )
        .unwrap();
        fs::write(src.join("scope.md"), node("Scope", "Reach of a [system](./system.md \"narrower\") within a [domain](./domain.md \"broader\").")).unwrap();
        fs::write(
            src.join("domain.md"),
            node("Domain", "A bounded [system](./system.md \"broader\")."),
        )
        .unwrap();
        // Clean: links its terms, no cycle.
        fs::write(
            src.join("focus.md"),
            node("Focus", "Attention on a [scope](./scope.md \"broader\")."),
        )
        .unwrap();
        fs::write(src.join("story.md"), node("Story", "A sequence of events.")).unwrap();
    }

    #[test]
    fn contradictions_catch_the_planted_cycles_and_mentions() {
        let tmp = tempfile::tempdir().unwrap();
        contra_setup(tmp.path());
        let classes = Classes {
            structure: false,
            contradictions: true,
            sources: false,
            mirrors: false,
            semantic: false,
        };
        let report = build(tmp.path(), classes, false).unwrap();
        assert_eq!(report.classes, ["contradictions"]);
        let by_check = |c: &str| -> Vec<&Finding> {
            report.findings.iter().filter(|f| f.check == c).collect()
        };
        let cycles = by_check("broader_cycle");
        assert_eq!(cycles.len(), 1, "{cycles:?}");
        assert_eq!(cycles[0].term, "domain");
        assert_eq!(cycles[0].severity, "error");
        assert_eq!(
            cycles[0].message,
            "broader links form a cycle: domain → system → scope → domain"
        );
        // The broader fixture's first sentences also link each other, so the
        // lay-definition graph carries a 3-cycle and a 2-cycle of its own.
        let mutual = by_check("mutual_definition");
        let mutual_terms: Vec<&str> = mutual.iter().map(|f| f.term.as_str()).collect();
        assert_eq!(mutual_terms, ["domain", "entity", "scope"], "{mutual:?}");
        assert!(mutual[0].message.starts_with(
            "lay definitions define each other in a cycle: domain → system → scope\n"
        ));
        assert_eq!(mutual[1].severity, "warning");
        assert_eq!(
            mutual[1].message,
            "lay definitions define each other in a cycle: entity → information\n  entity: Any information in a story.\n  information: That which distinguishes; an Entity."
        );
        let mentions = by_check("unlinked_mention");
        assert_eq!(mentions.len(), 1, "{mentions:?}");
        assert_eq!(mentions[0].term, "entity");
        assert_eq!(
            mentions[0].message,
            "lay definition mentions a known term without a link: `story`"
        );
        assert_eq!(report.summary.errors, 1);
        assert_eq!(report.summary.warnings, 4);
        assert!(!report.clean);
    }

    #[test]
    fn elementary_cycles_are_unique_and_rotated_to_the_smallest_node() {
        let mut g: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        g.insert("b", ["a"].into());
        g.insert("a", ["b", "c"].into());
        g.insert("c", ["a"].into());
        g.insert("d", ["d"].into());
        let cycles = elementary_cycles(&g);
        assert_eq!(
            cycles,
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["a".to_string(), "c".to_string()],
                vec!["d".to_string()],
            ]
        );
    }
}
