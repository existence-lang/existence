//! `existence sources` — the external sources each node cites and the passages
//! it quotes from them, derived from the markdown SPEC rules 7 and 8 mandate.
//!
//! A source is an `<a href="http…">` anchor. Two shapes are recognised:
//!
//! - **standalone** — the anchor on its own line, followed by `>` blockquote
//!   lines holding the quoted passage (a `> ### Noun` heading and indented
//!   `>     sub-sense` lines are part of the same quote block);
//! - **inline** — `> <a href="…">Label</a>: passage`, the anchor inside the
//!   blockquote with the passage on the same line.
//!
//! A blockquote block ends at the next anchor, at a heading, or at any other
//! prose line; an anchor with no blockquote after it is a source with no
//! quotes (a keynote video, a mid-paragraph reference).
//!
//! `--lock` derives `audit/sources.lock.json` from the same data: one entry
//! per URL with the terms citing it and, per term, the status of its quotes.
//! This phase never touches the network, so a fresh entry carries
//! `fetched_at: null` and `quotes: { term: "unchecked" }`; rewriting the lock
//! keeps whatever a later fetch recorded for URLs and terms that still cite.

use crate::markdown;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Default lockfile location, relative to the ontology directory.
pub const DEFAULT_LOCK: &str = "audit/sources.lock.json";

/// Quote status recorded for a term before any fetch has checked it.
pub const UNCHECKED: &str = "unchecked";

/// One anchor in a node and the passages quoted under it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SourceRef {
    pub url: String,
    /// The anchor text (empty when the label is not on the anchor's line).
    pub label: String,
    /// Quoted lines with the `>` prefix stripped, in document order.
    pub quotes: Vec<String>,
    /// A Wayback snapshot pinned beside the anchor: a second anchor on the
    /// same line whose URL is `…/<timestamp>/<this url>`. Quotes are verified
    /// against it when the live page has moved on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
}

/// All sources of one node.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TermSources {
    pub term: String,
    pub sources: Vec<SourceRef>,
}

/// One URL in the lockfile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockEntry {
    /// Terms whose node cites this URL (sorted, unique).
    pub cited_by: Vec<String>,
    /// RFC 3339 time of the last fetch; `None` until the audit fetches.
    pub fetched_at: Option<String>,
    /// HTTP status of the last fetch.
    pub status: Option<u16>,
    /// SHA-256 of the normalised page text at the last fetch.
    pub content_sha256: Option<String>,
    /// Per citing term: `unchecked` until fetched, then `present`, `moved`,
    /// or `missing`.
    pub quotes: BTreeMap<String, String>,
    /// Pinned Wayback snapshot URL.
    pub archive: Option<String>,
}

/// The lockfile: URL → entry, sorted by URL.
pub type Lock = BTreeMap<String, LockEntry>;

/// List sources (text or JSON) or write the lockfile.
///
/// `term` restricts the listing to one node. `lock` writes the lockfile at
/// that path (relative paths resolve against `ontology_dir`); it always covers
/// the whole ontology, so it cannot be combined with `term`.
pub fn run(
    ontology_dir: &Path,
    term: Option<&str>,
    json: bool,
    lock: Option<&Path>,
) -> Result<(), String> {
    if let Some(lock_path) = lock {
        if term.is_some() {
            return Err("--lock covers the whole ontology; drop the term argument".into());
        }
        let all = build(ontology_dir, None)?;
        let lock_path = if lock_path.is_absolute() {
            lock_path.to_path_buf()
        } else {
            ontology_dir.join(lock_path)
        };
        let existing = read_lock(&lock_path)?;
        let lock = build_lock(&all, existing.as_ref());
        write_lock(&lock_path, &lock)?;
        let (anchors, quotes) = totals(&all);
        println!(
            "sources: {} url(s) from {} anchor(s) in {} term(s) ({} quote(s)) written to {}",
            lock.len(),
            anchors,
            all.len(),
            quotes,
            lock_path.display()
        );
        return Ok(());
    }

    let all = build(ontology_dir, term)?;
    if json {
        let mut text = serde_json::to_string_pretty(&all)
            .map_err(|e| format!("JSON serialization error: {e}"))?;
        text.push('\n');
        print!("{text}");
    } else {
        print!("{}", to_text(&all));
    }
    Ok(())
}

/// Derive the sources of every node (or of `term` alone), in term order.
pub fn build(ontology_dir: &Path, term: Option<&str>) -> Result<Vec<TermSources>, String> {
    let src_dir = ontology_dir.join("src");
    if !src_dir.is_dir() {
        return Err(format!("Source directory {} not found", src_dir.display()));
    }
    let terms = match term {
        Some(t) => {
            if !src_dir.join(format!("{t}.md")).is_file() {
                return Err(format!("Term '{t}' not found in {}", src_dir.display()));
            }
            vec![t.to_string()]
        }
        None => markdown::list_terms(&src_dir)?,
    };
    let mut out = Vec::with_capacity(terms.len());
    for term in terms {
        let path = src_dir.join(format!("{term}.md"));
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        out.push(TermSources {
            term,
            sources: extract_sources(&content),
        });
    }
    Ok(out)
}

/// Extract every anchor in a node with the passages quoted under it.
pub fn extract_sources(content: &str) -> Vec<SourceRef> {
    let anchor = Regex::new(r#"<a\s+href="(https?://[^"\s]+)"[^>]*>"#).unwrap();
    let label_re = Regex::new(r#"<a\s+href="https?://[^"\s]+"[^>]*>([^<]*)</a>"#).unwrap();

    let lines: Vec<&str> = content.lines().collect();
    let mut refs: Vec<SourceRef> = Vec::new();
    // Index of the ref currently collecting blockquote lines.
    let mut current: Option<usize> = None;

    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();

        if let Some(quoted) = trimmed.strip_prefix('>') {
            let q = quoted.trim();
            if anchor.is_match(q) {
                // Inline shape: the passage follows the anchor on this line.
                let mut last = None;
                for cap in anchor.captures_iter(q) {
                    if pin_previous(&mut refs, last, &cap[1]) {
                        continue;
                    }
                    refs.push(SourceRef {
                        url: cap[1].to_string(),
                        label: label_on_line(&label_re, q, &cap[1]),
                        quotes: Vec::new(),
                        archive: None,
                    });
                    last = Some(refs.len() - 1);
                }
                let passage = q
                    .rsplit_once("</a>")
                    .map(|(_, rest)| rest)
                    .unwrap_or("")
                    .trim_start_matches([':', ' ', '\u{a0}', '—', '-'])
                    .trim();
                if let Some(idx) = last {
                    if !passage.is_empty() {
                        refs[idx].quotes.push(passage.to_string());
                    }
                    current = Some(idx);
                }
                continue;
            }
            if q.is_empty() || q.starts_with('#') {
                continue;
            }
            if let Some(idx) = current {
                refs[idx].quotes.push(q.to_string());
            }
            continue;
        }

        if anchor.is_match(line) {
            // Standalone shape: quotes follow on later lines.
            let mut last = None;
            for cap in anchor.captures_iter(line) {
                if pin_previous(&mut refs, last, &cap[1]) {
                    continue;
                }
                let mut label = label_on_line(&label_re, line, &cap[1]);
                if label.is_empty() && !line.contains("</a>") {
                    // Label wrapped onto the next line.
                    if let Some(next) = lines.get(i + 1)
                        && let Some((text, _)) = next.split_once("</a>")
                    {
                        label = text.trim().to_string();
                    }
                }
                refs.push(SourceRef {
                    url: cap[1].to_string(),
                    label,
                    quotes: Vec::new(),
                    archive: None,
                });
                last = Some(refs.len() - 1);
            }
            current = last;
            continue;
        }

        if trimmed.is_empty() {
            // Blank lines between quotes do not end the block.
            continue;
        }

        // Any other prose, heading, or list line ends the quote block.
        current = None;
    }
    refs
}

/// The page a Wayback-style URL (`…/<timestamp>[id_]/<url>`) is a copy of.
pub fn archived_original(url: &str) -> Option<&str> {
    let re = Regex::new(r"/(\d{4,14})(?:id_)?/(https?://.+)$").unwrap();
    re.captures(url).map(|c| c.get(2).unwrap().as_str())
}

/// When `url` is an archived copy of the anchor just before it on the same
/// line, record it as that source's pin instead of a source of its own.
fn pin_previous(refs: &mut [SourceRef], last: Option<usize>, url: &str) -> bool {
    let (Some(idx), Some(original)) = (last, archived_original(url)) else {
        return false;
    };
    if same_page(&refs[idx].url, original) && refs[idx].archive.is_none() {
        refs[idx].archive = Some(url.to_string());
        return true;
    }
    false
}

/// Equal up to scheme and an explicit `:80`, which archives rewrite.
fn same_page(a: &str, b: &str) -> bool {
    let strip = |u: &str| {
        u.trim_start_matches("https://")
            .trim_start_matches("http://")
            .replacen(":80/", "/", 1)
            .trim_end_matches('/')
            .to_string()
    };
    strip(a) == strip(b)
}

fn label_on_line(label_re: &Regex, line: &str, url: &str) -> String {
    label_re
        .captures_iter(line)
        .find(|cap| cap[0].contains(url))
        .map(|cap| cap[1].trim().to_string())
        .unwrap_or_default()
}

/// Aggregate sources into a lockfile, keeping fetch results from `existing`
/// for URLs still cited and quote statuses for terms still citing.
pub fn build_lock(all: &[TermSources], existing: Option<&Lock>) -> Lock {
    let mut lock = Lock::new();
    for ts in all {
        for src in &ts.sources {
            let entry = lock.entry(src.url.clone()).or_insert_with(|| {
                let prior = existing.and_then(|l| l.get(&src.url));
                LockEntry {
                    cited_by: Vec::new(),
                    fetched_at: prior.and_then(|p| p.fetched_at.clone()),
                    status: prior.and_then(|p| p.status),
                    content_sha256: prior.and_then(|p| p.content_sha256.clone()),
                    quotes: BTreeMap::new(),
                    archive: prior.and_then(|p| p.archive.clone()),
                }
            });
            if !entry.cited_by.contains(&ts.term) {
                entry.cited_by.push(ts.term.clone());
            }
            // A pin written in the node outranks whatever a fetch recorded.
            if src.archive.is_some() {
                entry.archive = src.archive.clone();
            }
            entry.quotes.entry(ts.term.clone()).or_insert_with(|| {
                existing
                    .and_then(|l| l.get(&src.url))
                    .and_then(|p| p.quotes.get(&ts.term).cloned())
                    .unwrap_or_else(|| UNCHECKED.to_string())
            });
        }
    }
    for entry in lock.values_mut() {
        entry.cited_by.sort();
    }
    lock
}

/// Read a lockfile; `Ok(None)` when it does not exist yet.
pub fn read_lock(path: &Path) -> Result<Option<Lock>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("Cannot parse {}: {e}", path.display()))
}

/// Write a lockfile, creating its directory.
pub fn write_lock(path: &Path, lock: &Lock) -> Result<(), String> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    }
    let mut text =
        serde_json::to_string_pretty(lock).map_err(|e| format!("JSON serialization error: {e}"))?;
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

/// (anchor count, quote count) across all terms.
fn totals(all: &[TermSources]) -> (usize, usize) {
    all.iter().fold((0, 0), |(a, q), ts| {
        (
            a + ts.sources.len(),
            q + ts.sources.iter().map(|s| s.quotes.len()).sum::<usize>(),
        )
    })
}

/// Human-readable listing.
pub fn to_text(all: &[TermSources]) -> String {
    let mut out = String::new();
    for ts in all {
        if ts.sources.is_empty() {
            continue;
        }
        out.push_str(&format!("{} ({} source(s))\n", ts.term, ts.sources.len()));
        for src in &ts.sources {
            let label = if src.label.is_empty() {
                String::new()
            } else {
                format!(" — {}", src.label)
            };
            out.push_str(&format!(
                "  {}{} — {} quote(s)\n",
                src.url,
                label,
                src.quotes.len()
            ));
        }
    }
    let (anchors, quotes) = totals(all);
    let cited = all.iter().filter(|t| !t.sources.is_empty()).count();
    out.push_str(&format!(
        "{anchors} anchor(s) in {cited} of {} term(s), {quotes} quote(s)\n",
        all.len()
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const NODE: &str = r#"# Scope

## [Ontology](./ontology.md)

> A quoted lay definition with no source above it is not a quote.

Since the scope is wide, the resolution starts low. <a href="http://cosmicscale.appspot.com/index.html" target="_blank">
The Cosmic Scale</a> is a demonstration of scope.

## [Epistemology](./epistemology.md)

### [Cultural](./culture.md) Definition

<a href="http://en.wiktionary.org/wiki/scope" target="_blank">scope (wiktionary)</a>

> ### Noun

> The breadth, depth or reach of a subject; a domain.

>     (computing) The region of program source in which an identifier is meaningful.

> (slang) Shortened form of periscope.

<a href="https://www.youtube.com/watch?v=abc" target="_blank">A keynote</a>

Abstraction is:

* to take a distance from the physical world

> <a href="https://en.wikipedia.org/wiki/Scope" target="_blank">Wikipedia</a>: Scope is the extent of the area or subject matter.

> <a href="https://www.etymonline.com/word/scope" target="_blank">Etymonline</a>: from Italian scopo "aim, purpose".

### [Pattern](./pattern.md) Expression

Scope is a pattern.
"#;

    #[test]
    fn extracts_both_shapes_and_ends_blocks_correctly() {
        let refs = extract_sources(NODE);
        let urls: Vec<&str> = refs.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "http://cosmicscale.appspot.com/index.html",
                "http://en.wiktionary.org/wiki/scope",
                "https://www.youtube.com/watch?v=abc",
                "https://en.wikipedia.org/wiki/Scope",
                "https://www.etymonline.com/word/scope",
            ]
        );
        // Mid-paragraph anchor with a wrapped label: label recovered, no quotes.
        assert_eq!(refs[0].label, "The Cosmic Scale");
        assert!(refs[0].quotes.is_empty());
        // Standalone: heading line dropped, indented sub-sense kept, blank
        // lines do not end the block.
        assert_eq!(refs[1].label, "scope (wiktionary)");
        assert_eq!(
            refs[1].quotes,
            [
                "The breadth, depth or reach of a subject; a domain.",
                "(computing) The region of program source in which an identifier is meaningful.",
                "(slang) Shortened form of periscope.",
            ]
        );
        // Anchor followed by prose and a list: no quotes.
        assert_eq!(refs[2].label, "A keynote");
        assert!(refs[2].quotes.is_empty());
        // Inline: passage after `</a>:`.
        assert_eq!(refs[3].label, "Wikipedia");
        assert_eq!(
            refs[3].quotes,
            ["Scope is the extent of the area or subject matter."]
        );
        assert_eq!(refs[4].quotes, ["from Italian scopo \"aim, purpose\"."]);
    }

    #[test]
    fn an_archive_anchor_beside_its_source_pins_it_instead_of_being_a_source() {
        let refs = extract_sources(
            "# T\n\n<a href=\"http://x.org/a\" target=\"_blank\">A</a> <a href=\"https://web.archive.org/web/20150301000000/http://x.org/a\" target=\"_blank\">(archived 2015-03-01)</a>\n\n> kept\n\n> <a href=\"https://y.org/b\">B</a> <a href=\"http://127.0.0.1:9/archive/20150301000000id_/https://y.org/b\">(archived)</a>: inline passage\n\n<a href=\"https://web.archive.org/web/20100101000000/http://z.org/c\">C, a dead link already swapped</a>\n\n> c\n",
        );
        assert_eq!(refs.len(), 3, "{refs:?}");
        assert_eq!(refs[0].url, "http://x.org/a");
        assert_eq!(
            refs[0].archive.as_deref(),
            Some("https://web.archive.org/web/20150301000000/http://x.org/a")
        );
        assert_eq!(refs[0].quotes, vec!["kept"]);
        assert_eq!(refs[1].url, "https://y.org/b");
        assert!(refs[1].archive.is_some());
        assert_eq!(refs[1].quotes, vec!["inline passage"]);
        // With no live anchor before it, an archive URL is a source of its own.
        assert!(refs[2].url.starts_with("https://web.archive.org/"));
        assert!(refs[2].archive.is_none());
        assert_eq!(
            archived_original("https://web.archive.org/web/2015id_/http://x.org/a"),
            Some("http://x.org/a")
        );
        assert_eq!(archived_original("http://x.org/a"), None);

        let all = vec![TermSources {
            term: "t".into(),
            sources: refs,
        }];
        let lock = build_lock(&all, None);
        assert_eq!(lock.len(), 3);
        assert!(lock["http://x.org/a"].archive.is_some());
        assert!(lock["https://y.org/b"].archive.is_some());
    }

    #[test]
    fn a_quote_before_any_anchor_is_ignored() {
        let refs =
            extract_sources("# T\n\n> orphan\n\n<a href=\"https://x.org/a\">A</a>\n\n> kept\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].quotes, ["kept"]);
    }

    fn setup(tmp: &Path) {
        let src = tmp.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("entity.md"),
            "# Entity\n\n## [Ontology](./ontology.md)\n\nx\n\n## [Epistemology](./epistemology.md)\n\n<a href=\"https://en.wikipedia.org/wiki/Entity\" target=\"_blank\">Entity (Wikipedia)</a>\n\n> An entity is something that exists.\n\n<a href=\"https://en.wikipedia.org/wiki/Entity\" target=\"_blank\">again</a>\n\n> Second citation of the same page.\n",
        )
        .unwrap();
        fs::write(
            src.join("being.md"),
            "# Being\n\n## [Epistemology](./epistemology.md)\n\n> <a href=\"https://en.wikipedia.org/wiki/Entity\" target=\"_blank\">Wikipedia</a>: cited inline.\n\n> <a href=\"https://en.wiktionary.org/wiki/being\" target=\"_blank\">Wiktionary</a>: existence.\n",
        )
        .unwrap();
        fs::write(
            src.join("bare.md"),
            "# Bare\n\n## [Ontology](./ontology.md)\n\nNo sources.\n",
        )
        .unwrap();
    }

    #[test]
    fn build_lists_terms_in_order_and_filters_by_term() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let all = build(tmp.path(), None).unwrap();
        let terms: Vec<&str> = all.iter().map(|t| t.term.as_str()).collect();
        assert_eq!(terms, ["bare", "being", "entity"]);
        assert_eq!(all[2].sources.len(), 2);
        assert_eq!(
            all[2].sources[1].quotes,
            ["Second citation of the same page."]
        );
        let one = build(tmp.path(), Some("being")).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].sources.len(), 2);
        assert!(
            build(tmp.path(), Some("ghost"))
                .unwrap_err()
                .contains("not found")
        );
    }

    #[test]
    fn lock_aggregates_by_url_and_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let all = build(tmp.path(), None).unwrap();
        let lock = build_lock(&all, None);
        assert_eq!(lock.len(), 2);
        let entity = &lock["https://en.wikipedia.org/wiki/Entity"];
        assert_eq!(entity.cited_by, ["being", "entity"]);
        assert_eq!(entity.fetched_at, None);
        assert_eq!(entity.status, None);
        assert_eq!(entity.quotes["being"], UNCHECKED);
        assert_eq!(entity.quotes["entity"], UNCHECKED);

        let path = tmp.path().join(DEFAULT_LOCK);
        assert_eq!(read_lock(&path).unwrap(), None);
        write_lock(&path, &lock).unwrap();
        assert_eq!(read_lock(&path).unwrap(), Some(lock.clone()));

        let text = fs::read_to_string(&path).unwrap();
        let expected = r#"{
  "https://en.wikipedia.org/wiki/Entity": {
    "cited_by": [
      "being",
      "entity"
    ],
    "fetched_at": null,
    "status": null,
    "content_sha256": null,
    "quotes": {
      "being": "unchecked",
      "entity": "unchecked"
    },
    "archive": null
  },
  "https://en.wiktionary.org/wiki/being": {
    "cited_by": [
      "being"
    ],
    "fetched_at": null,
    "status": null,
    "content_sha256": null,
    "quotes": {
      "being": "unchecked"
    },
    "archive": null
  }
}
"#;
        assert_eq!(text, expected);
    }

    #[test]
    fn rewriting_the_lock_keeps_fetch_results_and_drops_stale_entries() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let all = build(tmp.path(), None).unwrap();
        let mut existing = Lock::new();
        existing.insert(
            "https://en.wikipedia.org/wiki/Entity".into(),
            LockEntry {
                cited_by: vec!["entity".into(), "gone".into()],
                fetched_at: Some("2026-09-11T00:00:00Z".into()),
                status: Some(200),
                content_sha256: Some("abc".into()),
                quotes: BTreeMap::from([
                    ("entity".to_string(), "present".to_string()),
                    ("gone".to_string(), "missing".to_string()),
                ]),
                archive: Some("https://web.archive.org/web/2026/x".into()),
            },
        );
        existing.insert(
            "https://dead.example/".into(),
            LockEntry {
                cited_by: vec!["entity".into()],
                fetched_at: Some("2026-09-11T00:00:00Z".into()),
                status: Some(404),
                content_sha256: None,
                quotes: BTreeMap::new(),
                archive: None,
            },
        );
        let lock = build_lock(&all, Some(&existing));
        assert!(!lock.contains_key("https://dead.example/"));
        let entity = &lock["https://en.wikipedia.org/wiki/Entity"];
        assert_eq!(entity.cited_by, ["being", "entity"]);
        assert_eq!(entity.fetched_at.as_deref(), Some("2026-09-11T00:00:00Z"));
        assert_eq!(entity.status, Some(200));
        assert_eq!(entity.content_sha256.as_deref(), Some("abc"));
        assert_eq!(
            entity.archive.as_deref(),
            Some("https://web.archive.org/web/2026/x")
        );
        assert_eq!(entity.quotes["entity"], "present");
        assert_eq!(entity.quotes["being"], UNCHECKED);
        assert!(!entity.quotes.contains_key("gone"));
    }

    #[test]
    fn run_rejects_lock_with_a_term_and_writes_relative_to_the_ontology() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let err = run(tmp.path(), Some("entity"), false, Some(Path::new("x.json"))).unwrap_err();
        assert!(err.contains("--lock covers the whole ontology"));
        run(tmp.path(), None, false, Some(Path::new(DEFAULT_LOCK))).unwrap();
        assert!(tmp.path().join(DEFAULT_LOCK).is_file());
    }

    #[test]
    fn text_and_json_shapes() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let all = build(tmp.path(), Some("being")).unwrap();
        assert_eq!(
            to_text(&all),
            "being (2 source(s))\n  https://en.wikipedia.org/wiki/Entity — Wikipedia — 1 quote(s)\n  https://en.wiktionary.org/wiki/being — Wiktionary — 1 quote(s)\n2 anchor(s) in 1 of 1 term(s), 2 quote(s)\n"
        );
        let json = serde_json::to_value(&all).unwrap();
        assert_eq!(
            json,
            serde_json::json!([{
                "term": "being",
                "sources": [
                    {"url": "https://en.wikipedia.org/wiki/Entity", "label": "Wikipedia", "quotes": ["cited inline."]},
                    {"url": "https://en.wiktionary.org/wiki/being", "label": "Wiktionary", "quotes": ["existence."]}
                ]
            }])
        );
    }
}
