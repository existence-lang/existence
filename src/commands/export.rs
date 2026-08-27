//! Export the ontology as RDF.
//!
//! The mapping is SKOS-first: every node becomes a `skos:Concept` in one
//! `skos:ConceptScheme`, rings become `skos:Collection`s, the lay definition
//! (first line of the Ontology section, SPEC rule 2) becomes
//! `skos:definition`, and the four template sections are carried verbatim as
//! `xl:` annotation literals. `[term](./term.md "broader")` /
//! `"narrower"` links (SPEC "Typed links") become `skos:broader` /
//! `skos:narrower` with the inverse emitted on the target; untitled links are
//! `skos:related`.

use crate::config::Config;
use crate::markdown::{self, Node, Relation};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// Default base IRI for term identity when `existence.toml` does not set
/// `meta.base_iri` and `--base-iri` is not given.
pub const DEFAULT_BASE_IRI: &str = "https://existence-lang.github.io/ontology/";
/// Default namespace for the `xl:` metamodel vocabulary.
pub const DEFAULT_VOCAB_IRI: &str = "https://existence-lang.github.io/ontology/vocab#";

const SKOS: &str = "http://www.w3.org/2004/02/skos/core#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const DCTERMS: &str = "http://purl.org/dc/terms/";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const LANG: &str = "en";

#[derive(Debug)]
pub struct ExportRing {
    pub level: u32,
    pub name: String,
    pub description: String,
    /// Terms declared in the ring that also exist as node files.
    pub terms: Vec<String>,
}

#[derive(Debug)]
pub struct ExportNode {
    pub term: String,
    pub label: String,
    /// Lay definition with markdown stripped.
    pub definition: Option<String>,
    pub ring: Option<u32>,
    pub ontology: Option<String>,
    pub axiology: Option<String>,
    pub ethics: Option<String>,
    pub epistemology: Option<String>,
    /// Targets of `"broader"` links, plus inverses of other nodes' `"narrower"` links.
    pub broader: Vec<String>,
    /// Targets of `"narrower"` links, plus inverses of other nodes' `"broader"` links.
    pub narrower: Vec<String>,
    /// Untitled `[term](./term.md)` links to other exported nodes, minus any
    /// target already typed broader/narrower.
    pub related: Vec<String>,
    /// External `http(s)` references.
    pub see_also: Vec<String>,
}

#[derive(Debug)]
pub struct ExportGraph {
    pub base_iri: String,
    pub vocab_iri: String,
    pub scheme_name: String,
    pub scheme_description: String,
    pub rings: Vec<ExportRing>,
    pub nodes: Vec<ExportNode>,
}

impl ExportGraph {
    pub fn term_iri(&self, term: &str) -> String {
        format!("{}{term}", self.base_iri)
    }

    pub fn ring_iri(&self, level: u32) -> String {
        format!("{}ring/{level}", self.base_iri)
    }
}

/// Export the ontology to stdout in the requested format.
pub fn run(
    ontology_dir: &Path,
    ring: Option<u32>,
    format: &str,
    base_iri: Option<&str>,
) -> Result<(), String> {
    let graph = build(ontology_dir, ring, base_iri)?;
    match format {
        "turtle" | "ttl" => print!("{}", to_turtle(&graph)),
        "jsonld" | "json-ld" => {
            let json = serde_json::to_string_pretty(&to_jsonld(&graph))
                .map_err(|e| format!("JSON serialization error: {e}"))?;
            println!("{json}");
        }
        other => {
            return Err(format!(
                "Unknown export format '{other}' (expected \"turtle\" or \"jsonld\")"
            ));
        }
    }
    Ok(())
}

/// Build the export model from an ontology directory.
///
/// `ring` restricts the export to one ring (nodes and edges). `base_iri`
/// overrides `meta.base_iri` from `existence.toml`; the built-in default
/// applies when neither is set.
pub fn build(
    ontology_dir: &Path,
    ring: Option<u32>,
    base_iri: Option<&str>,
) -> Result<ExportGraph, String> {
    let src_dir = ontology_dir.join("src");
    if !src_dir.is_dir() {
        return Err(format!("Source directory {} not found", src_dir.display()));
    }
    let config = Config::load(&ontology_dir.join("existence.toml"))?;

    let base_iri = normalize_base_iri(
        base_iri
            .map(str::to_string)
            .or_else(|| config.meta.base_iri.clone())
            .unwrap_or_else(|| DEFAULT_BASE_IRI.to_string()),
    );
    let vocab_iri = config
        .meta
        .vocab_iri
        .clone()
        .unwrap_or_else(|| DEFAULT_VOCAB_IRI.to_string());
    for (what, iri) in [("base IRI", &base_iri), ("vocab IRI", &vocab_iri)] {
        if !is_safe_iri(iri) {
            return Err(format!(
                "{what} '{iri}' contains characters not allowed in an IRI"
            ));
        }
    }

    if let Some(level) = ring
        && config.get_ring(level).is_none()
    {
        return Err(format!("Ring {level} not defined in existence.toml"));
    }

    let existing_terms = markdown::list_terms(&src_dir)?;

    // First ring (lowest level) that declares a term wins.
    let mut ring_of: BTreeMap<String, u32> = BTreeMap::new();
    let mut rings = Vec::new();
    for (level, r) in config.rings_sorted() {
        if let Some(only) = ring
            && only != level
        {
            continue;
        }
        let terms: Vec<String> = r
            .terms
            .iter()
            .filter(|t| existing_terms.contains(t))
            .cloned()
            .collect();
        for t in &terms {
            ring_of.entry(t.clone()).or_insert(level);
        }
        rings.push(ExportRing {
            level,
            name: r.name.clone(),
            description: r.description.clone(),
            terms,
        });
    }

    let included = |term: &str| match ring {
        Some(level) => ring_of.get(term) == Some(&level),
        None => true,
    };

    let mut nodes = Vec::new();
    for term in &existing_terms {
        if !included(term) {
            continue;
        }
        if !is_safe_iri_segment(term) {
            return Err(format!(
                "Term '{term}' cannot be used as an IRI segment (allowed: a-z, 0-9, '-', '_')"
            ));
        }
        let file = src_dir.join(format!("{term}.md"));
        let content = std::fs::read_to_string(&file)
            .map_err(|e| format!("Failed to read {}: {e}", file.display()))?;
        let node = Node::parse(&content).map_err(|e| format!("{}: {e}", file.display()))?;

        // First relation declared for a target wins; lint warns on conflicts.
        let mut broader = Vec::new();
        let mut narrower = Vec::new();
        let mut related = Vec::new();
        for link in markdown::extract_typed_links(&content) {
            let target = &link.term;
            if target == term || !existing_terms.contains(target) || !included(target) {
                continue;
            }
            let bucket = match link.relation {
                Relation::Broader => &mut broader,
                Relation::Narrower => &mut narrower,
                Relation::Related => &mut related,
            };
            if !bucket.contains(target) {
                bucket.push(target.clone());
            }
        }
        related.retain(|t| !broader.contains(t) && !narrower.contains(t));
        narrower.retain(|t| !broader.contains(t));
        broader.sort();
        narrower.sort();
        related.sort();
        let see_also: Vec<String> = markdown::extract_external_links(&content)
            .into_iter()
            .filter(|url| is_safe_iri(url))
            .collect();
        let definition = markdown::extract_definition(&content)
            .map(|d| markdown::plain_text(&d))
            .filter(|d| !d.is_empty());

        nodes.push(ExportNode {
            term: term.clone(),
            label: node.title.clone(),
            definition,
            ring: ring_of.get(term).copied(),
            ontology: node.ontology.clone(),
            axiology: node.axiology.clone(),
            ethics: node.ethics.clone(),
            epistemology: node.epistemology.clone(),
            broader,
            narrower,
            related,
            see_also,
        });
    }

    add_inverse_hierarchy(&mut nodes);

    Ok(ExportGraph {
        base_iri,
        vocab_iri,
        scheme_name: config.meta.name.clone(),
        scheme_description: config.meta.description.clone(),
        rings,
        nodes,
    })
}

/// `skos:broader` and `skos:narrower` are inverses: a hierarchy declared on
/// one node is made navigable from the other. A target that is itself
/// `related` to the source is left alone (hierarchy is the stronger claim).
fn add_inverse_hierarchy(nodes: &mut [ExportNode]) {
    let mut add_narrower: Vec<(String, String)> = Vec::new();
    let mut add_broader: Vec<(String, String)> = Vec::new();
    for n in nodes.iter() {
        for b in &n.broader {
            add_narrower.push((b.clone(), n.term.clone()));
        }
        for nar in &n.narrower {
            add_broader.push((nar.clone(), n.term.clone()));
        }
    }
    for n in nodes.iter_mut() {
        for (target, source) in add_narrower.iter().chain(add_broader.iter()) {
            if target != &n.term {
                continue;
            }
            let bucket = if add_narrower.iter().any(|(t, s)| t == target && s == source) {
                &mut n.narrower
            } else {
                &mut n.broader
            };
            if !bucket.contains(source) {
                bucket.push(source.clone());
            }
        }
        n.related
            .retain(|t| !n.broader.contains(t) && !n.narrower.contains(t));
        n.broader.sort();
        n.narrower.sort();
    }
}

/// Serialize as Turtle 1.1.
pub fn to_turtle(g: &ExportGraph) -> String {
    let mut out = String::new();
    out.push_str(&format!("@prefix skos: <{SKOS}> .\n"));
    out.push_str(&format!("@prefix rdfs: <{RDFS}> .\n"));
    out.push_str(&format!("@prefix dcterms: <{DCTERMS}> .\n"));
    out.push_str(&format!("@prefix xsd: <{XSD}> .\n"));
    out.push_str(&format!("@prefix xl: <{}> .\n\n", g.vocab_iri));

    let iri = |s: &str| format!("<{s}>");

    // Concept scheme
    let mut props: Vec<(&str, Vec<String>)> = vec![
        ("a", vec!["skos:ConceptScheme".to_string()]),
        ("rdfs:label", vec![literal(&g.scheme_name)]),
        ("dcterms:description", vec![literal(&g.scheme_description)]),
    ];
    let top: Vec<String> = g
        .rings
        .iter()
        .filter(|r| r.level == 0)
        .flat_map(|r| r.terms.iter())
        .map(|t| iri(&g.term_iri(t)))
        .collect();
    if !top.is_empty() {
        props.push(("skos:hasTopConcept", top));
    }
    push_subject(&mut out, &iri(&g.base_iri), &props);

    // Ring collections
    for r in &g.rings {
        let mut props: Vec<(&str, Vec<String>)> = vec![
            ("a", vec!["skos:Collection".to_string()]),
            ("skos:inScheme", vec![iri(&g.base_iri)]),
            ("rdfs:label", vec![literal(&r.name)]),
            ("dcterms:description", vec![literal(&r.description)]),
            ("xl:ring", vec![r.level.to_string()]),
        ];
        let members: Vec<String> = r.terms.iter().map(|t| iri(&g.term_iri(t))).collect();
        if !members.is_empty() {
            props.push(("skos:member", members));
        }
        push_subject(&mut out, &iri(&g.ring_iri(r.level)), &props);
    }

    // Concepts
    for n in &g.nodes {
        let mut props: Vec<(&str, Vec<String>)> = vec![
            ("a", vec!["skos:Concept".to_string()]),
            ("skos:inScheme", vec![iri(&g.base_iri)]),
            ("skos:prefLabel", vec![literal(&n.label)]),
        ];
        if let Some(d) = &n.definition {
            props.push(("skos:definition", vec![literal(d)]));
        }
        if let Some(level) = n.ring {
            props.push(("xl:ring", vec![level.to_string()]));
        }
        for (pred, section) in [
            ("xl:ontology", &n.ontology),
            ("xl:axiology", &n.axiology),
            ("xl:ethics", &n.ethics),
            ("xl:epistemology", &n.epistemology),
        ] {
            if let Some(text) = section {
                props.push((pred, vec![literal(text)]));
            }
        }
        for (pred, targets) in [
            ("skos:broader", &n.broader),
            ("skos:narrower", &n.narrower),
            ("skos:related", &n.related),
        ] {
            if !targets.is_empty() {
                props.push((pred, targets.iter().map(|t| iri(&g.term_iri(t))).collect()));
            }
        }
        if !n.see_also.is_empty() {
            props.push(("rdfs:seeAlso", n.see_also.iter().map(|u| iri(u)).collect()));
        }
        push_subject(&mut out, &iri(&g.term_iri(&n.term)), &props);
    }

    out
}

/// Serialize as a JSON-LD document with a prefix `@context` and a flat `@graph`.
pub fn to_jsonld(g: &ExportGraph) -> Value {
    let context = json!({
        "skos": SKOS,
        "rdfs": RDFS,
        "dcterms": DCTERMS,
        "xsd": XSD,
        "xl": g.vocab_iri,
    });

    let id = |s: &str| json!({ "@id": s });
    let ids = |items: &[String], to_iri: &dyn Fn(&str) -> String| -> Value {
        Value::Array(items.iter().map(|s| id(&to_iri(s))).collect())
    };
    let term_iri = |t: &str| g.term_iri(t);
    let as_is = |u: &str| u.to_string();

    let mut graph = Vec::new();

    let mut scheme = Map::new();
    scheme.insert("@id".into(), Value::String(g.base_iri.clone()));
    scheme.insert("@type".into(), Value::String("skos:ConceptScheme".into()));
    scheme.insert("rdfs:label".into(), lang(&g.scheme_name));
    scheme.insert("dcterms:description".into(), lang(&g.scheme_description));
    let top: Vec<String> = g
        .rings
        .iter()
        .filter(|r| r.level == 0)
        .flat_map(|r| r.terms.iter().cloned())
        .collect();
    if !top.is_empty() {
        scheme.insert("skos:hasTopConcept".into(), ids(&top, &term_iri));
    }
    graph.push(Value::Object(scheme));

    for r in &g.rings {
        let mut coll = Map::new();
        coll.insert("@id".into(), Value::String(g.ring_iri(r.level)));
        coll.insert("@type".into(), Value::String("skos:Collection".into()));
        coll.insert("skos:inScheme".into(), id(&g.base_iri));
        coll.insert("rdfs:label".into(), lang(&r.name));
        coll.insert("dcterms:description".into(), lang(&r.description));
        coll.insert("xl:ring".into(), json!(r.level));
        if !r.terms.is_empty() {
            coll.insert("skos:member".into(), ids(&r.terms, &term_iri));
        }
        graph.push(Value::Object(coll));
    }

    for n in &g.nodes {
        let mut concept = Map::new();
        concept.insert("@id".into(), Value::String(g.term_iri(&n.term)));
        concept.insert("@type".into(), Value::String("skos:Concept".into()));
        concept.insert("skos:inScheme".into(), id(&g.base_iri));
        concept.insert("skos:prefLabel".into(), lang(&n.label));
        if let Some(d) = &n.definition {
            concept.insert("skos:definition".into(), lang(d));
        }
        if let Some(level) = n.ring {
            concept.insert("xl:ring".into(), json!(level));
        }
        for (key, section) in [
            ("xl:ontology", &n.ontology),
            ("xl:axiology", &n.axiology),
            ("xl:ethics", &n.ethics),
            ("xl:epistemology", &n.epistemology),
        ] {
            if let Some(text) = section {
                concept.insert(key.into(), lang(text));
            }
        }
        for (key, targets) in [
            ("skos:broader", &n.broader),
            ("skos:narrower", &n.narrower),
            ("skos:related", &n.related),
        ] {
            if !targets.is_empty() {
                concept.insert(key.into(), ids(targets, &term_iri));
            }
        }
        if !n.see_also.is_empty() {
            concept.insert("rdfs:seeAlso".into(), ids(&n.see_also, &as_is));
        }
        graph.push(Value::Object(concept));
    }

    json!({ "@context": context, "@graph": graph })
}

fn lang(value: &str) -> Value {
    json!({ "@value": value, "@language": LANG })
}

/// A short-form Turtle literal with a language tag. Newlines, quotes, and
/// backslashes are escaped so any section prose stays on one line.
fn literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 6);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out.push('@');
    out.push_str(LANG);
    out
}

fn push_subject(out: &mut String, subject: &str, props: &[(&str, Vec<String>)]) {
    out.push_str(subject);
    for (i, (pred, objects)) in props.iter().enumerate() {
        out.push_str(if i == 0 { " " } else { " ;\n    " });
        out.push_str(pred);
        out.push(' ');
        out.push_str(&objects.join(", "));
    }
    out.push_str(" .\n\n");
}

/// A base IRI must end in `/` or `#` so term names append cleanly.
fn normalize_base_iri(base: String) -> String {
    let trimmed = base.trim().to_string();
    if trimmed.ends_with('/') || trimmed.ends_with('#') {
        trimmed
    } else {
        format!("{trimmed}/")
    }
}

fn is_safe_iri(s: &str) -> bool {
    !s.is_empty()
        && !s.chars().any(|c| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\')
        })
}

fn is_safe_iri_segment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
[meta]
name = "sample/ontology"
description = "A sample ontology"

[rings.0]
name = "kernel"
description = "core"
terms = ["existence", "entity", "missing"]

[rings.1]
name = "software"
description = "bridge"
terms = ["model"]
"#;

    fn sample_ontology(config: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(dir.path().join("existence.toml"), config).unwrap();
        std::fs::write(
            src.join("existence.md"),
            "# Existence\n\n## [Ontology](./ontology.md)\n\nEverything that 'is'.\n\n## [Axiology](./axiology.md)\n\nIt matters.\n\n## [Epistemology](./epistemology.md)\n\nSee [entity](./entity.md).\n",
        )
        .unwrap();
        std::fs::write(
            src.join("entity.md"),
            "# Entity\n\n## [Ontology](./ontology.md)\n\nAny **information** in [Existence](./existence.md \"broader\").\n\nMore about [existence](./existence.md) and [nowhere](./nowhere.md).\n\n## [Axiology](./axiology.md)\n\nSaid \"quoted\" with a\\backslash.\n\n## [Ethics](./ethics.md)\n\nBe kind.\n\n## [Epistemology](./epistemology.md)\n\n<a href=\"https://en.wikipedia.org/wiki/Entity\" target=\"_blank\">Entity</a> and [ref](https://example.org/entity).\n",
        )
        .unwrap();
        std::fs::write(
            src.join("model.md"),
            "# Model\n\n## [Ontology](./ontology.md)\n\nA [pattern](./pattern.md) of an [entity](./entity.md \"narrower\") or an [orphan](./orphan.md).\n\n## [Axiology](./axiology.md)\n\nUseful.\n\n## [Epistemology](./epistemology.md)\n\nKnown.\n",
        )
        .unwrap();
        std::fs::write(
            src.join("orphan.md"),
            "# Orphan\n\n## [Ontology](./ontology.md)\n\nNot in any ring.\n\n## [Axiology](./axiology.md)\n\nStill matters.\n\n## [Epistemology](./epistemology.md)\n\nKnown.\n",
        )
        .unwrap();
        dir
    }

    fn node<'a>(g: &'a ExportGraph, term: &str) -> &'a ExportNode {
        g.nodes.iter().find(|n| n.term == term).unwrap()
    }

    #[test]
    fn build_maps_rings_links_and_definitions() {
        let dir = sample_ontology(CONFIG);
        let g = build(dir.path(), None, None).unwrap();

        assert_eq!(g.base_iri, DEFAULT_BASE_IRI);
        assert_eq!(g.vocab_iri, DEFAULT_VOCAB_IRI);
        assert_eq!(g.scheme_name, "sample/ontology");
        assert_eq!(g.nodes.len(), 4);

        // Ring membership only counts terms that exist as files.
        assert_eq!(g.rings[0].terms, vec!["existence", "entity"]);
        assert_eq!(g.rings[1].terms, vec!["model"]);

        let entity = node(&g, "entity");
        assert_eq!(entity.label, "Entity");
        assert_eq!(entity.ring, Some(0));
        assert_eq!(
            entity.definition.as_deref(),
            Some("Any information in Existence.")
        );
        // Links dedupe, drop self-links, and drop targets that do not exist.
        // A target typed once stays out of `related` even when also linked untitled.
        assert!(entity.related.is_empty());
        assert_eq!(entity.broader, vec!["existence", "model"]);
        assert_eq!(entity.narrower, Vec::<String>::new());
        // Inverses: existence gains entity as narrower; model declared entity narrower.
        assert_eq!(node(&g, "existence").narrower, vec!["entity"]);
        assert_eq!(node(&g, "existence").related, Vec::<String>::new());
        assert_eq!(node(&g, "model").narrower, vec!["entity"]);
        assert_eq!(node(&g, "model").related, vec!["orphan"]);
        assert_eq!(
            entity.see_also,
            vec![
                "https://en.wikipedia.org/wiki/Entity",
                "https://example.org/entity"
            ]
        );
        assert!(entity.ethics.is_some());

        assert_eq!(node(&g, "orphan").ring, None);
    }

    #[test]
    fn ring_filter_restricts_nodes_and_edges() {
        let dir = sample_ontology(CONFIG);
        let g = build(dir.path(), Some(1), None).unwrap();
        assert_eq!(g.rings.len(), 1);
        assert_eq!(g.rings[0].level, 1);
        let terms: Vec<&str> = g.nodes.iter().map(|n| n.term.as_str()).collect();
        assert_eq!(terms, vec!["model"]);
        // `entity` and `orphan` are outside ring 1, so the edges are dropped.
        assert!(node(&g, "model").related.is_empty());
        assert!(node(&g, "model").narrower.is_empty());

        assert!(build(dir.path(), Some(7), None).is_err());
    }

    #[test]
    fn base_iri_precedence_and_normalization() {
        let dir = sample_ontology(CONFIG);
        let g = build(dir.path(), None, Some("https://example.org/ont")).unwrap();
        assert_eq!(g.base_iri, "https://example.org/ont/");
        assert_eq!(g.term_iri("entity"), "https://example.org/ont/entity");
        assert_eq!(g.ring_iri(0), "https://example.org/ont/ring/0");

        let with_meta = CONFIG.replacen(
            "[meta]\n",
            "[meta]\nbase_iri = \"https://example.org/meta#\"\nvocab_iri = \"https://example.org/v#\"\n",
            1,
        );
        let dir = sample_ontology(&with_meta);
        let g = build(dir.path(), None, None).unwrap();
        assert_eq!(g.base_iri, "https://example.org/meta#");
        assert_eq!(g.vocab_iri, "https://example.org/v#");
        // The flag still wins over the config.
        let g = build(dir.path(), None, Some("https://flag.example/")).unwrap();
        assert_eq!(g.base_iri, "https://flag.example/");

        assert!(build(dir.path(), None, Some("https://bad example/")).is_err());
    }

    #[test]
    fn turtle_output_is_well_formed() {
        let dir = sample_ontology(CONFIG);
        let g = build(dir.path(), None, None).unwrap();
        let ttl = to_turtle(&g);

        assert!(ttl.starts_with("@prefix skos: <http://www.w3.org/2004/02/skos/core#> .\n"));
        assert!(ttl.contains(&format!("@prefix xl: <{DEFAULT_VOCAB_IRI}> .")));
        assert!(ttl.contains(&format!("<{DEFAULT_BASE_IRI}> a skos:ConceptScheme ;")));
        assert!(ttl.contains(&format!(
            "skos:hasTopConcept <{DEFAULT_BASE_IRI}existence>, <{DEFAULT_BASE_IRI}entity>"
        )));
        assert!(ttl.contains(&format!("<{DEFAULT_BASE_IRI}ring/1> a skos:Collection ;")));
        assert!(ttl.contains(&format!("<{DEFAULT_BASE_IRI}entity> a skos:Concept ;")));
        assert!(ttl.contains("skos:prefLabel \"Entity\"@en ;"));
        assert!(ttl.contains("skos:definition \"Any information in Existence.\"@en ;"));
        assert!(ttl.contains("xl:ring 0 ;"));
        assert!(ttl.contains(&format!(
            "skos:broader <{DEFAULT_BASE_IRI}existence>, <{DEFAULT_BASE_IRI}model> ;"
        )));
        assert!(ttl.contains(&format!("skos:narrower <{DEFAULT_BASE_IRI}entity>")));
        assert!(ttl.contains(&format!("skos:related <{DEFAULT_BASE_IRI}orphan>")));
        assert!(ttl.contains(
            "rdfs:seeAlso <https://en.wikipedia.org/wiki/Entity>, <https://example.org/entity> ."
        ));
        // Section prose is escaped onto one line.
        assert!(ttl.contains("xl:axiology \"Said \\\"quoted\\\" with a\\\\backslash.\"@en ;"));
        assert!(ttl.contains(
            "xl:ontology \"Any **information** in [Existence](./existence.md \\\"broader\\\").\\n\\nMore about"
        ));
        // No orphan-ring predicate for a term outside every ring.
        let orphan_block = ttl
            .split("\n\n")
            .find(|b| b.starts_with(&format!("<{DEFAULT_BASE_IRI}orphan>")))
            .unwrap();
        assert!(!orphan_block.contains("xl:ring"));
        // Every statement block terminates.
        for block in ttl
            .trim_end()
            .split("\n\n")
            .filter(|b| !b.starts_with("@prefix"))
        {
            assert!(block.ends_with(" ."), "unterminated block: {block}");
        }
    }

    #[test]
    fn jsonld_output_has_context_and_graph() {
        let dir = sample_ontology(CONFIG);
        let g = build(dir.path(), None, None).unwrap();
        let doc = to_jsonld(&g);

        assert_eq!(doc["@context"]["skos"], SKOS);
        assert_eq!(doc["@context"]["xl"], DEFAULT_VOCAB_IRI);
        let graph = doc["@graph"].as_array().unwrap();
        // 1 scheme + 2 rings + 4 concepts
        assert_eq!(graph.len(), 7);
        assert_eq!(graph[0]["@type"], "skos:ConceptScheme");
        assert_eq!(graph[0]["@id"], DEFAULT_BASE_IRI);

        let entity = graph
            .iter()
            .find(|n| n["@id"] == format!("{DEFAULT_BASE_IRI}entity"))
            .unwrap();
        assert_eq!(entity["@type"], "skos:Concept");
        assert_eq!(entity["skos:prefLabel"]["@value"], "Entity");
        assert_eq!(entity["skos:prefLabel"]["@language"], "en");
        assert_eq!(
            entity["skos:definition"]["@value"],
            "Any information in Existence."
        );
        assert_eq!(entity["xl:ring"], 0);
        assert_eq!(
            entity["skos:broader"][0]["@id"],
            format!("{DEFAULT_BASE_IRI}existence")
        );
        assert!(entity.get("skos:related").is_none());
        let existence = graph
            .iter()
            .find(|n| n["@id"] == format!("{DEFAULT_BASE_IRI}existence"))
            .unwrap();
        assert_eq!(
            existence["skos:narrower"][0]["@id"],
            format!("{DEFAULT_BASE_IRI}entity")
        );
        assert_eq!(entity["rdfs:seeAlso"].as_array().unwrap().len(), 2);
        assert!(
            entity["xl:ethics"]["@value"]
                .as_str()
                .unwrap()
                .contains("Be kind.")
        );

        let orphan = graph
            .iter()
            .find(|n| n["@id"] == format!("{DEFAULT_BASE_IRI}orphan"))
            .unwrap();
        assert!(orphan.get("xl:ring").is_none());
        assert!(orphan.get("skos:related").is_none());
    }

    #[test]
    fn run_rejects_unknown_format() {
        let dir = sample_ontology(CONFIG);
        let err = run(dir.path(), None, "xml", None).unwrap_err();
        assert!(err.contains("Unknown export format 'xml'"));
    }

    #[test]
    fn literal_escapes() {
        assert_eq!(literal("plain"), "\"plain\"@en");
        assert_eq!(literal("a\"b\\c\nd\te\r"), "\"a\\\"b\\\\c\\nd\\te\\r\"@en");
    }
}
