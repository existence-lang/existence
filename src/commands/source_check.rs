//! `existence audit --sources` — re-fetch every cited URL and compare it with
//! the lockfile `existence sources --lock` derives.
//!
//! For each URL in the lock (one request per second by default, with a
//! `User-Agent`): reachability, the HTTP status, a SHA-256 of the page's
//! normalised text, and, per citing term, whether the passages the node
//! quotes are still on the page: `present` (exact after whitespace and
//! punctuation normalisation), `moved` (a window of the page matches the
//! quote at ≥ 0.9 normalised Levenshtein similarity), or `missing`.
//!
//! A dead link (status ≥ 400) or a missing quote is source drift and an
//! error; a moved quote is a warning; a changed page whose quotes are still
//! present is noise and only recorded in the lock. A host that cannot be
//! reached is skipped, not failed: one warning per host, its URLs keep their
//! previous lock values.
//!
//! Dead links get a Wayback snapshot pinned through the availability API
//! (`https://archive.org/wayback/available?url=…`, overridable for tests);
//! `--fix` rewrites the node's `href` to that snapshot.
//!
//! A quote the live page has lost is looked for in the snapshots pinned
//! beside the anchor (several when the node's quotes come from different
//! years), then in the archive: each quote is verified on its own, against
//! the live page or any pinned copy, so a page whose passages were quoted
//! in different years needs one pin per year, not one snapshot carrying all
//! of them. `--fix` writes the pins a node's line lacks.

use crate::commands::audit::Finding;
use crate::commands::sources::{self, Lock, LockEntry, TermSources};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Wayback availability API.
pub const DEFAULT_WAYBACK: &str = "https://archive.org/wayback/available";
/// Wayback CDX index, listing every snapshot of a URL.
pub const DEFAULT_CDX: &str = "https://web.archive.org/cdx/search/cdx";
/// Wayback snapshot base: `<base>/<timestamp>/<url>` is a copy.
pub const DEFAULT_ARCHIVE_WEB: &str = "https://web.archive.org/web";
/// Snapshots are tried closest to this date first (`YYYYMMDD`).
pub const DEFAULT_ARCHIVE_AROUND: &str = "20150101";
/// Snapshots nearest `--archive-around` tried first, hoping one carries every
/// lost quote; the best partial covers among them are pinned otherwise.
const PIN_CANDIDATES: usize = 6;
/// Snapshot fetches allowed per search, the nearest window included, before
/// the quotes still lost stay unpinned.
const PIN_FETCH_BUDGET: usize = 20;

/// Fuzzy threshold for a quote to count as moved rather than missing.
pub const MOVED_THRESHOLD: f64 = 0.9;

/// How the source pass runs.
#[derive(Debug, Clone)]
pub struct SourceOptions {
    /// Lockfile path, relative to the ontology directory unless absolute.
    pub lock: PathBuf,
    /// Minimum gap between requests.
    pub rate: Duration,
    /// Wayback availability endpoint (`?url=` is appended).
    pub wayback: String,
    /// Wayback CDX endpoint used to list snapshots when a quote has drifted.
    pub cdx: String,
    /// Snapshot base URL: `<base>/<timestamp>/<url>`.
    pub archive_web: String,
    /// `YYYYMMDD`; snapshots nearest this date are tried first.
    pub archive_around: String,
    /// Per-request timeout.
    pub timeout: Duration,
}

impl Default for SourceOptions {
    fn default() -> Self {
        SourceOptions {
            lock: PathBuf::from(sources::DEFAULT_LOCK),
            rate: Duration::from_secs(1),
            wayback: DEFAULT_WAYBACK.to_string(),
            cdx: DEFAULT_CDX.to_string(),
            archive_web: DEFAULT_ARCHIVE_WEB.to_string(),
            archive_around: DEFAULT_ARCHIVE_AROUND.to_string(),
            timeout: Duration::from_secs(20),
        }
    }
}

/// What one fetch produced.
enum Fetch {
    Response { status: u16, body: String },
    Unreachable(String),
}

/// A rate-limited HTTP client.
struct Client {
    agent: ureq::Agent,
    rate: Duration,
    last: Option<Instant>,
    /// Hosts that failed, and the reason, so every source on one is handled
    /// the same way without being fetched again.
    unreachable: BTreeMap<String, String>,
    /// Snapshots whose own fetch failed, by raw snapshot URL. A snapshot that
    /// was never read cannot be said to lack a passage.
    pin_unreachable: BTreeMap<String, String>,
    /// Normalised snapshot pages by raw snapshot URL, fetched once per run.
    pages: BTreeMap<String, Option<String>>,
    /// Candidate snapshot timestamps per page, in the order they are tried.
    stamps: BTreeMap<String, Vec<String>>,
}

impl Client {
    fn new(opts: &SourceOptions) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(opts.timeout))
            .user_agent(format!(
                "existence-audit/{} (+https://github.com/existence-lang/existence)",
                env!("CARGO_PKG_VERSION")
            ))
            .build();
        Client {
            agent: config.new_agent(),
            rate: opts.rate,
            last: None,
            unreachable: BTreeMap::new(),
            pin_unreachable: BTreeMap::new(),
            pages: BTreeMap::new(),
            stamps: BTreeMap::new(),
        }
    }

    /// The normalised text of a pinned snapshot, if it can be fetched;
    /// fetched once per run.
    fn snapshot_page(&mut self, archive: &str) -> Option<String> {
        let raw = raw_snapshot_url(archive);
        if let Some(page) = self.pages.get(&raw) {
            return page.clone();
        }
        let page = match self.get(&raw) {
            Fetch::Response { status, body } if status < 400 => {
                Some(normalise(&html_to_text(&body)))
            }
            Fetch::Unreachable(reason) => {
                self.pin_unreachable.insert(raw.clone(), reason);
                None
            }
            _ => None,
        };
        self.pages.insert(raw, page.clone());
        page
    }

    /// Why a pinned snapshot could not be fetched this run, if it could not.
    /// A pin that was never read says nothing about the passage it holds.
    fn pin_fetch_failed(&self, archive: &str) -> Option<&String> {
        self.pin_unreachable.get(&raw_snapshot_url(archive))
    }

    /// The snapshots of `url` in the order they are tried, from one CDX
    /// query per page per run.
    fn snapshot_stamps(&mut self, opts: &SourceOptions, url: &str) -> Vec<String> {
        if let Some(stamps) = self.stamps.get(url) {
            return stamps.clone();
        }
        let query = format!(
            "{}?url={}&output=json&filter=statuscode:200&collapse=timestamp:6&limit=400",
            opts.cdx,
            percent_encode(url)
        );
        let rows: Vec<Vec<String>> = match self.get(&query) {
            Fetch::Response { status, body } if status < 400 => {
                serde_json::from_str(&body).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let stamps = candidate_order(&rows, &opts.archive_around);
        self.stamps.insert(url.to_string(), stamps.clone());
        stamps
    }

    fn get(&mut self, url: &str) -> Fetch {
        if let Some(last) = self.last {
            let elapsed = last.elapsed();
            if elapsed < self.rate {
                std::thread::sleep(self.rate - elapsed);
            }
        }
        self.last = Some(Instant::now());
        // A transport error is retried once after one rate interval: a reset
        // connection must not write off a whole host (or a snapshot) for the run.
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.agent.get(url).call() {
                Ok(mut resp) => {
                    let status = resp.status().as_u16();
                    let body = resp.body_mut().read_to_string().unwrap_or_default();
                    return Fetch::Response { status, body };
                }
                Err(e) if attempt < 2 => {
                    std::thread::sleep(self.rate);
                    self.last = Some(Instant::now());
                    let _ = e;
                }
                Err(e) => return Fetch::Unreachable(e.to_string()),
            }
        }
    }
}

/// Run the source pass: refresh the lock, return the findings, and apply
/// dead-link fixes when `fix` is set.
pub fn check(ontology_dir: &Path, opts: &SourceOptions, fix: bool) -> Result<Vec<Finding>, String> {
    let all = sources::build(ontology_dir, None)?;
    let lock_path = if opts.lock.is_absolute() {
        opts.lock.clone()
    } else {
        ontology_dir.join(&opts.lock)
    };
    let existing = sources::read_lock(&lock_path)?;
    let mut lock = sources::build_lock(&all, existing.as_ref());

    let mut client = Client::new(opts);
    let mut findings = Vec::new();
    let now = rfc3339_now();
    // Pins a node's line does not carry yet, for `--fix` to write.
    let mut to_write: Vec<PinWrite> = Vec::new();

    for (url, entry) in lock.iter_mut() {
        let Some(host) = host_of(url) else { continue };
        // A host that already failed is not fetched again, but its sources are
        // still handled rather than skipped outright: an unreachable source
        // host says nothing about the archive host, so a pinned copy can still
        // answer for the citation.
        let fetched = match client.unreachable.get(&host) {
            Some(reason) => Fetch::Unreachable(reason.clone()),
            None => client.get(url),
        };
        match fetched {
            Fetch::Unreachable(reason) => {
                client
                    .unreachable
                    .entry(host.clone())
                    .or_insert_with(|| reason.clone());
                // The same archive lookup a 4xx dead link gets. Without it an
                // unreachable host could never resolve: it warned every run
                // with no fix to apply, while a dead link pinned itself and
                // cleared.
                if entry.archives.is_empty()
                    && let Some(archive) = wayback_lookup(&mut client, &opts.wayback, url)
                {
                    entry.archives.push(archive);
                }
                for term in entry.cited_by.clone() {
                    let own = pins_for(&all, &term, url);
                    // A pin already beside the link that still fetches keeps
                    // the citation verifiable; there is nothing to report.
                    if own.iter().any(|p| client.snapshot_page(p).is_some()) {
                        continue;
                    }
                    let mut offer = None;
                    for pin in entry.archives.clone() {
                        if !own.contains(&pin) && client.snapshot_page(&pin).is_some() {
                            offer = Some(pin);
                            break;
                        }
                    }
                    match offer {
                        Some(pin) => {
                            findings.push(source_finding(
                                "unreachable_host",
                                "warning",
                                &term,
                                format!(
                                    "host {host} is unreachable ({reason}); the archived copy {pin} still carries this source"
                                ),
                                Some(format!("pin the archived copy {pin} beside the link")),
                            ));
                            to_write.push(PinWrite {
                                term: term.clone(),
                                url: url.clone(),
                                pins: vec![pin],
                            });
                        }
                        None => findings.push(source_finding(
                            "unreachable_host",
                            "warning",
                            &term,
                            format!(
                                "host {host} is unreachable ({reason}); its sources were skipped"
                            ),
                            None,
                        )),
                    }
                }
            }
            Fetch::Response { status, body } => {
                entry.fetched_at = Some(now.clone());
                entry.status = Some(status);
                if status >= 400 {
                    if entry.archives.is_empty()
                        && let Some(archive) = wayback_lookup(&mut client, &opts.wayback, url)
                    {
                        entry.archives.push(archive);
                    }
                    for term in &entry.cited_by {
                        findings.push(source_finding(
                            "dead_link",
                            "error",
                            term,
                            format!("source {url} returned HTTP {status}"),
                            entry
                                .archives
                                .first()
                                .map(|a| format!("replace the link with the archived copy {a}")),
                        ));
                    }
                    continue;
                }
                let text = html_to_text(&body);
                entry.content_sha256 = Some(sha256_hex(&text));
                let page = normalise(&text);
                for term in entry.cited_by.clone() {
                    let quotes = quotes_for(&all, &term, url);
                    // Each quote is verified against the live page first, then
                    // the pins on the node's own line, then the URL's other pins
                    // (other nodes, the lock), and only then is the archive
                    // searched for what is still missing.
                    let own = pins_for(&all, &term, url);
                    let mut verdicts = verify(&page, &quotes);
                    let mut lost = still_missing(&verdicts);
                    let known: Vec<String> = own
                        .iter()
                        .chain(entry.archives.iter().filter(|a| !own.contains(a)))
                        .cloned()
                        .collect();
                    for pin in &known {
                        if lost.is_empty() {
                            break;
                        }
                        if let Some(archived) = client.snapshot_page(pin) {
                            cover(&mut verdicts, &archived, pin);
                            lost = still_missing(&verdicts);
                        }
                    }
                    if !lost.is_empty() {
                        for (pin, archived) in find_pins(&mut client, opts, url, &lost) {
                            cover(&mut verdicts, &archived, &pin);
                            if !entry.archives.contains(&pin) {
                                entry.archives.push(pin);
                            }
                        }
                        lost = still_missing(&verdicts);
                    }
                    entry
                        .quotes
                        .insert(term.clone(), worst_word(&verdicts).to_string());
                    // Pins that carried a quote but are not on the node's line yet.
                    let mut needed: Vec<String> = Vec::new();
                    let mut example = String::new();
                    for v in &verdicts {
                        if let Status::Archived(pin) = &v.status
                            && !own.contains(pin)
                            && !needed.contains(pin)
                        {
                            if needed.is_empty() {
                                example = v.quote.clone();
                            }
                            needed.push(pin.clone());
                        }
                    }
                    if !needed.is_empty() {
                        let pins = needed.join(", ");
                        findings.push(source_finding(
                            "quote_missing",
                            "error",
                            &term,
                            format!(
                                "quoted passage no longer found on {url} but is in the snapshot {pins}: \u{201c}{}\u{201d}",
                                clip(&example)
                            ),
                            Some(format!("pin the archived copy {pins} beside the link")),
                        ));
                        to_write.push(PinWrite {
                            term: term.clone(),
                            url: url.clone(),
                            pins: needed,
                        });
                    }
                    if let Some(first) = lost.first() {
                        let more = match lost.len() {
                            1 => String::new(),
                            n => format!(" (+{} more)", n - 1),
                        };
                        let pins: Vec<&str> = own
                            .iter()
                            .chain(entry.archives.iter().filter(|a| !own.contains(a)))
                            .map(String::as_str)
                            .collect();
                        // A pin whose own fetch failed was never read, so it
                        // cannot be reported as a pin that does not carry the
                        // passage. That escalated an unreachable archive into
                        // an absent-quote error against a snapshot nobody had
                        // looked at.
                        let unread: Vec<String> = known
                            .iter()
                            .filter_map(|p| {
                                client.pin_fetch_failed(p).map(|why| format!("{p} ({why})"))
                            })
                            .collect();
                        if unread.is_empty() {
                            let message = if pins.is_empty() {
                                format!(
                                    "quoted passage no longer found on {url}: \u{201c}{}\u{201d}{more}",
                                    clip(first)
                                )
                            } else {
                                format!(
                                    "quoted passage found neither on {url} nor in its pinned copy {}: \u{201c}{}\u{201d}{more}",
                                    pins.join(", "),
                                    clip(first)
                                )
                            };
                            findings.push(source_finding(
                                "quote_missing",
                                "error",
                                &term,
                                message,
                                None,
                            ));
                        } else {
                            findings.push(source_finding(
                                "pin_unreachable",
                                "warning",
                                &term,
                                format!(
                                    "quoted passage is not on {url} and its pinned copy {} could not be fetched, so the pin was not checked: \u{201c}{}\u{201d}{more}",
                                    unread.join(", "),
                                    clip(first)
                                ),
                                None,
                            ));
                        }
                    } else {
                        // A passage the live page still carries but has
                        // drifted from. The pin is the citation of record: if
                        // one beside the link carries it verbatim, nothing has
                        // been lost and there is nothing to report. Otherwise
                        // look for a snapshot that does and offer it -- the
                        // resolution path `quote_moved` never had, which is why
                        // these warnings accumulated run after run with nothing
                        // anyone could do about them.
                        let drifted: Vec<String> = verdicts
                            .iter()
                            .filter(|v| v.status == Status::Live("moved"))
                            .map(|v| v.quote.clone())
                            .collect();
                        let mut fresh: Vec<String> = Vec::new();
                        let mut anchored = String::new();
                        let mut adrift: Option<String> = None;
                        let mut pinned: Vec<(String, String)> = Vec::new();
                        for quote in drifted {
                            let mut found = None;
                            for pin in &own {
                                if client
                                    .snapshot_page(pin)
                                    .is_some_and(|page| match_quote(&page, &quote) == "present")
                                {
                                    found = Some(pin.clone());
                                    break;
                                }
                            }
                            let found = match found {
                                Some(pin) => Some(pin),
                                None => {
                                    find_exact_pin(&mut client, opts, url, &quote).inspect(|pin| {
                                        if !entry.archives.contains(pin) {
                                            entry.archives.push(pin.clone());
                                        }
                                        if !fresh.contains(pin) {
                                            fresh.push(pin.clone());
                                            anchored = quote.clone();
                                        }
                                    })
                                }
                            };
                            match found {
                                Some(pin) => pinned.push((quote, pin)),
                                None => adrift = adrift.or(Some(quote)),
                            }
                        }
                        for v in verdicts.iter_mut() {
                            if let Some((_, pin)) = pinned.iter().find(|(q, _)| *q == v.quote) {
                                v.status = Status::Pinned(pin.clone());
                            }
                        }
                        entry
                            .quotes
                            .insert(term.clone(), worst_word(&verdicts).to_string());
                        if !fresh.is_empty() {
                            let pins = fresh.join(", ");
                            findings.push(source_finding(
                                "quote_moved",
                                "warning",
                                &term,
                                format!(
                                    "quoted passage on {url} has changed but still matches; the snapshot {pins} carries it verbatim: \u{201c}{}\u{201d}",
                                    clip(&anchored)
                                ),
                                Some(format!("pin the archived copy {pins} beside the link")),
                            ));
                            to_write.push(PinWrite {
                                term: term.clone(),
                                url: url.clone(),
                                pins: fresh,
                            });
                        }
                        if let Some(quote) = adrift {
                            findings.push(source_finding(
                                "quote_moved",
                                "warning",
                                &term,
                                format!(
                                    "quoted passage on {url} has changed but still matches, and no snapshot carries it verbatim: \u{201c}{}\u{201d}",
                                    clip(&quote)
                                ),
                                None,
                            ));
                        }
                    }
                }
            }
        }
    }

    if fix {
        apply_dead_link_fixes(ontology_dir, &lock, &mut findings)?;
        apply_pin_fixes(ontology_dir, &to_write, &mut findings)?;
    }
    sources::write_lock(&lock_path, &lock)?;
    Ok(findings)
}

/// Where one quote was found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// On the live page: `present` or `moved`.
    Live(&'static str),
    /// Not on the live page, but in this pinned snapshot.
    Archived(String),
    /// On the live page but drifted, and this pinned snapshot carries it
    /// verbatim. Distinct from `Archived`, which asks `--fix` to write a pin
    /// for a passage the live page has lost.
    Pinned(String),
    /// Nowhere yet.
    Missing,
}

/// One quote of a term on a page and where it was found.
#[derive(Debug, Clone)]
struct Verdict {
    quote: String,
    status: Status,
}

/// Pins to write beside one node's anchor of `url`.
struct PinWrite {
    term: String,
    url: String,
    pins: Vec<String>,
}

/// Each quote against the live page.
fn verify(page: &str, quotes: &[String]) -> Vec<Verdict> {
    quotes
        .iter()
        .map(|quote| Verdict {
            quote: quote.clone(),
            status: match match_quote(page, quote) {
                "missing" => Status::Missing,
                live => Status::Live(live),
            },
        })
        .collect()
}

/// Mark every quote still missing that the snapshot `pin` carries.
fn cover(verdicts: &mut [Verdict], archived: &str, pin: &str) {
    for v in verdicts.iter_mut() {
        if v.status == Status::Missing && match_quote(archived, &v.quote) != "missing" {
            v.status = Status::Archived(pin.to_string());
        }
    }
}

fn still_missing(verdicts: &[Verdict]) -> Vec<String> {
    verdicts
        .iter()
        .filter(|v| v.status == Status::Missing)
        .map(|v| v.quote.clone())
        .collect()
}

/// The status word recorded for a term: its worst quote, `missing` over
/// `moved` over `archived` over `present`; no quotes leaves it `unchecked`.
fn worst_word(verdicts: &[Verdict]) -> &'static str {
    let rank = |v: &&Verdict| match &v.status {
        Status::Missing => 4,
        Status::Live("moved") => 3,
        Status::Archived(_) | Status::Pinned(_) => 2,
        Status::Live(_) => 1,
    };
    verdicts
        .iter()
        .max_by_key(rank)
        .map(|v| match &v.status {
            Status::Missing => "missing",
            Status::Archived(_) | Status::Pinned(_) => "archived",
            Status::Live(word) => word,
        })
        .unwrap_or(sources::UNCHECKED)
}

fn source_finding(
    check: &str,
    severity: &str,
    term: &str,
    message: String,
    fix: Option<String>,
) -> Finding {
    Finding {
        class: "sources".into(),
        check: check.into(),
        severity: severity.into(),
        term: term.into(),
        message,
        fix,
        fixed: false,
    }
}

/// Rewrite `href="<dead url>"` to the archived copy in every citing node.
fn apply_dead_link_fixes(
    ontology_dir: &Path,
    lock: &Lock,
    findings: &mut [Finding],
) -> Result<(), String> {
    let src_dir = ontology_dir.join("src");
    let dead: Vec<(&String, &LockEntry)> = lock
        .iter()
        .filter(|(_, e)| e.status.is_some_and(|s| s >= 400) && !e.archives.is_empty())
        .collect();
    for (url, entry) in dead {
        let archive = &entry.archives[0];
        for term in &entry.cited_by {
            let path = src_dir.join(format!("{term}.md"));
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
            let needle = format!("href=\"{url}\"");
            if !content.contains(&needle) {
                continue;
            }
            let rewritten = content.replace(&needle, &format!("href=\"{archive}\""));
            std::fs::write(&path, rewritten)
                .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
            for f in findings
                .iter_mut()
                .filter(|f| f.check == "dead_link" && f.term == *term && f.message.contains(url))
            {
                f.fixed = true;
            }
        }
    }
    Ok(())
}

/// Write ` <a href="<snapshot>" target="_blank">(archived YYYY-MM-DD)</a>`
/// after the anchor of `url` in each node that needs the pin, following any
/// pins the line already carries.
fn apply_pin_fixes(
    ontology_dir: &Path,
    to_write: &[PinWrite],
    findings: &mut [Finding],
) -> Result<(), String> {
    let src_dir = ontology_dir.join("src");
    for w in to_write {
        let path = src_dir.join(format!("{}.md", w.term));
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        let needle = format!("href=\"{}\"", w.url);
        let mut changed = false;
        let mut out = String::with_capacity(content.len() + 128);
        for line in content.split_inclusive('\n') {
            let Some(at) = line.find(&needle) else {
                out.push_str(line);
                continue;
            };
            let Some(close) = line[at..].find("</a>") else {
                out.push_str(line);
                continue;
            };
            let cut = end_of_pins(line, at + close + "</a>".len(), &w.url);
            let missing: Vec<&String> = w
                .pins
                .iter()
                .filter(|p| !line.contains(p.as_str()))
                .collect();
            if missing.is_empty() {
                out.push_str(line);
                continue;
            }
            out.push_str(&line[..cut]);
            for pin in missing {
                let label = match snapshot_date(pin) {
                    Some(date) => format!("(archived {date})"),
                    None => "(archived)".to_string(),
                };
                out.push_str(&format!(" <a href=\"{pin}\" target=\"_blank\">{label}</a>"));
            }
            out.push_str(&line[cut..]);
            changed = true;
        }
        if !changed {
            continue;
        }
        std::fs::write(&path, out)
            .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
        // Every check whose remedy is a pin beside the link: a passage the
        // live page lost, one it still carries but has drifted from, and a
        // source whose host no longer answers at all.
        for f in findings.iter_mut().filter(|f| {
            matches!(
                f.check.as_str(),
                "quote_missing" | "quote_moved" | "unreachable_host"
            ) && f.term == w.term
                && f.fix
                    .as_deref()
                    .is_some_and(|fix| w.pins.iter().all(|p| fix.contains(p.as_str())))
        }) {
            f.fixed = true;
        }
    }
    Ok(())
}

/// The offset just past every archive anchor of `url` that follows `from`
/// on the line, so a new pin lands after the ones already there.
fn end_of_pins(line: &str, from: usize, url: &str) -> usize {
    let anchor = Regex::new(r#"^\s*<a\s+href="([^"\s]+)"[^>]*>[^<]*</a>"#).unwrap();
    let mut cut = from;
    while let Some(cap) = anchor.captures(&line[cut..]) {
        let Some(original) = sources::archived_original(&cap[1]) else {
            break;
        };
        if !sources::same_page(url, original) {
            break;
        }
        cut += cap.get(0).unwrap().end();
    }
    cut
}
/// `YYYY-MM-DD` of a snapshot URL's timestamp segment.
fn snapshot_date(archive: &str) -> Option<String> {
    let re = regex::Regex::new(r"/(\d{8})\d{0,6}(?:id_)?/https?://").ok()?;
    let ts = re.captures(archive)?.get(1)?.as_str().to_string();
    Some(format!("{}-{}-{}", &ts[0..4], &ts[4..6], &ts[6..8]))
}

/// The raw (`id_`) form of a snapshot URL, without the Wayback toolbar.
fn raw_snapshot_url(archive: &str) -> String {
    let re = regex::Regex::new(r"(/\d{4,14})/(https?://)").unwrap();
    if archive.contains("id_/") {
        archive.to_string()
    } else {
        re.replace(archive, "${1}id_/${2}").into_owned()
    }
}

/// Search the archive for snapshots of `url` that carry the `lost` quotes.
/// The `PIN_CANDIDATES` snapshots nearest `archive_around` are tried first,
/// hoping one carries them all; failing that, the best partial covers among
/// them are pinned, and one snapshot per calendar year over the rest of the
/// archive is tried for whatever is left, nearest year first, until
/// `PIN_FETCH_BUDGET` fetches. Each pin returned carries at least one quote
/// no earlier pin did; quotes no snapshot carries stay missing.
fn find_pins(
    client: &mut Client,
    opts: &SourceOptions,
    url: &str,
    lost: &[String],
) -> Vec<(String, String)> {
    let stamps: Vec<String> = client
        .snapshot_stamps(opts, url)
        .into_iter()
        .take(PIN_FETCH_BUDGET)
        .collect();
    let mut remaining: Vec<String> = lost.to_vec();
    let mut pins: Vec<(String, String)> = Vec::new();
    let mut window: Vec<(String, String)> = Vec::new();
    for (i, ts) in stamps.iter().enumerate() {
        if remaining.is_empty() {
            break;
        }
        let pin = format!("{}/{ts}/{url}", opts.archive_web);
        let Some(page) = client.snapshot_page(&pin) else {
            continue;
        };
        let covered = covered_by(&page, &remaining);
        if covered.len() == remaining.len() {
            remaining.clear();
            pins.push((pin, page));
            break;
        }
        if i < PIN_CANDIDATES {
            window.push((pin, page));
            if i + 1 == PIN_CANDIDATES || i + 1 == stamps.len() {
                take_best_covers(&mut window, &mut remaining, &mut pins);
            }
            continue;
        }
        if !covered.is_empty() {
            remaining.retain(|q| !covered.contains(q));
            pins.push((pin, page));
        }
    }
    pins
}

/// The nearest snapshot of `url` carrying `quote` verbatim, from the window
/// `find_pins` tries first.
///
/// A drifted passage is still on the live page, so only the nearest window is
/// searched: enough to anchor a citation, not a whole-archive sweep for every
/// warning. `find_pins` accepts a snapshot where the quote has also drifted,
/// which would resolve nothing here -- anchoring a drifted quote to a copy it
/// has also drifted from leaves the same warning next run.
fn find_exact_pin(
    client: &mut Client,
    opts: &SourceOptions,
    url: &str,
    quote: &str,
) -> Option<String> {
    let stamps: Vec<String> = client
        .snapshot_stamps(opts, url)
        .into_iter()
        .take(PIN_CANDIDATES)
        .collect();
    for ts in stamps {
        let pin = format!("{}/{ts}/{url}", opts.archive_web);
        if let Some(page) = client.snapshot_page(&pin)
            && match_quote(&page, quote) == "present"
        {
            return Some(pin);
        }
    }
    None
}

/// Pin, from the nearest window, the snapshot covering the most remaining
/// quotes (nearest first on a tie), and repeat while any still adds one.
fn take_best_covers(
    window: &mut Vec<(String, String)>,
    remaining: &mut Vec<String>,
    pins: &mut Vec<(String, String)>,
) {
    loop {
        let best = window
            .iter()
            .enumerate()
            .map(|(i, (_, page))| (covered_by(page, remaining).len(), i))
            .filter(|(n, _)| *n > 0)
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let Some((_, i)) = best else { break };
        let (pin, page) = window.remove(i);
        let covered = covered_by(&page, remaining);
        remaining.retain(|q| !covered.contains(q));
        pins.push((pin, page));
    }
}

/// The quotes a snapshot page carries (present or moved).
fn covered_by(page: &str, quotes: &[String]) -> Vec<String> {
    quotes
        .iter()
        .filter(|q| match_quote(page, q) != "missing")
        .cloned()
        .collect()
}

/// CDX rows in the order snapshots are tried: the `PIN_CANDIDATES` nearest
/// `around`, then one per calendar year (the snapshot nearest that year's
/// start) over the rest of the archive, nearest year first.
fn candidate_order(rows: &[Vec<String>], around: &str) -> Vec<String> {
    let Some(around_day) = around.get(0..8).and_then(|s| s.parse::<i64>().ok()) else {
        return Vec::new();
    };
    let mut by_distance: Vec<(i64, String)> = rows
        .iter()
        .skip(1)
        .filter_map(|row| {
            let ts = row.get(1)?;
            let day: i64 = ts.get(0..8)?.parse().ok()?;
            Some(((day - around_day).abs(), ts.clone()))
        })
        .collect();
    by_distance.sort();
    by_distance.dedup();
    let mut order: Vec<String> = by_distance
        .iter()
        .take(PIN_CANDIDATES)
        .map(|(_, ts)| ts.clone())
        .collect();
    let mut years: BTreeMap<i64, (i64, String)> = BTreeMap::new();
    for (_, ts) in &by_distance {
        let day: i64 = ts[0..8].parse().unwrap_or(0);
        let year = day / 10_000;
        let from_start = (day - year * 10_000 - 101).abs();
        let slot = years.entry(year).or_insert((from_start, ts.clone()));
        if from_start < slot.0 {
            *slot = (from_start, ts.clone());
        }
    }
    let around_year = around_day / 10_000;
    let mut sweep: Vec<(i64, i64, String)> = years
        .into_iter()
        .map(|(year, (_, ts))| ((year - around_year).abs(), year, ts))
        .collect();
    sweep.sort();
    for (_, _, ts) in sweep {
        if !order.contains(&ts) {
            order.push(ts);
        }
    }
    order
}

/// The closest Wayback snapshot of `url`, if the availability API has one.
fn wayback_lookup(client: &mut Client, endpoint: &str, url: &str) -> Option<String> {
    let query = format!("{endpoint}?url={}", percent_encode(url));
    let Fetch::Response { status, body } = client.get(&query) else {
        return None;
    };
    if status >= 400 {
        return None;
    }
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let closest = &json["archived_snapshots"]["closest"];
    if closest["available"].as_bool() != Some(true) {
        return None;
    }
    closest["url"].as_str().map(str::to_string)
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    Some(rest.split(['/', '?', '#']).next()?.to_string())
}

fn quotes_for(all: &[TermSources], term: &str, url: &str) -> Vec<String> {
    all.iter()
        .filter(|t| t.term == term)
        .flat_map(|t| t.sources.iter())
        .filter(|s| s.url == url)
        .flat_map(|s| s.quotes.iter().cloned())
        .collect()
}

/// The pins written on a term's own anchors of `url`, in document order.
fn pins_for(all: &[TermSources], term: &str, url: &str) -> Vec<String> {
    let mut pins: Vec<String> = Vec::new();
    for pin in all
        .iter()
        .filter(|t| t.term == term)
        .flat_map(|t| t.sources.iter())
        .filter(|s| s.url == url)
        .flat_map(|s| s.archives.iter())
    {
        if !pins.contains(pin) {
            pins.push(pin.clone());
        }
    }
    pins
}

/// `present`, `moved`, or `missing` for one quote against a normalised page.
pub fn match_quote(page: &str, quote: &str) -> &'static str {
    let q = normalise(quote);
    if q.is_empty() {
        return "present";
    }
    if page.contains(&q) {
        return "present";
    }
    if best_window_similarity(page, &q) >= MOVED_THRESHOLD {
        "moved"
    } else {
        "missing"
    }
}

/// Best normalised Levenshtein similarity between the quote and any window
/// of the page with the same word count give or take one, after a cheap
/// bag-of-words filter.
fn best_window_similarity(page: &str, quote: &str) -> f64 {
    let words: Vec<&str> = page.split(' ').filter(|w| !w.is_empty()).collect();
    let qwords: Vec<&str> = quote.split(' ').filter(|w| !w.is_empty()).collect();
    let n = qwords.len();
    if n == 0 || words.len() < n {
        return 0.0;
    }
    let qset: BTreeSet<&str> = qwords.iter().copied().collect();
    let mut best: f64 = 0.0;
    // Windows one word shorter and longer than the quote absorb a single
    // inserted or dropped word.
    for size in n.saturating_sub(1).max(1)..=n + 1 {
        if words.len() < size {
            continue;
        }
        for window in words.windows(size) {
            let overlap = window.iter().filter(|w| qset.contains(*w)).count();
            if (overlap as f64) < 0.6 * n as f64 {
                continue;
            }
            let candidate = window.join(" ");
            best = best.max(strsim::normalized_levenshtein(&candidate, quote));
            if best >= 0.999 {
                return best;
            }
        }
    }
    best
}

/// Lowercase, letters and digits only, single spaces. Wikipedia-style
/// reference markers (`[1]`, `[a]`, `[note 2]`, `[citation needed]`) are
/// dropped first, on the page and on the quote alike: a marker sits inside
/// a sentence, so leaving it in makes a passage that is otherwise verbatim
/// look absent, and a quote copied with its markers never matches a copy
/// whose markers differ.
pub fn normalise(text: &str) -> String {
    let marker = Regex::new(
        r"(?i)\[\s*(?:\d{1,3}|[a-z]{1,2}|note \d{1,3}|citation needed|clarification needed|dubious|verification needed|(?:when|who|which|where|why)\?)\s*\]",
    )
    .unwrap();
    let text = marker.replace_all(text, " ");
    let mut out = String::with_capacity(text.len());
    let mut space = true;
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_string()
}

/// Strip scripts, styles, and tags; decode the common entities; collapse
/// whitespace.
pub fn html_to_text(html: &str) -> String {
    let block =
        Regex::new(r"(?is)<(script|style|noscript)[^>]*>.*?</(script|style|noscript)\s*>").unwrap();
    // MediaWiki renders a citation as `<sup class="reference">…</sup>`
    // inside the sentence it annotates; it is not part of the passage.
    let reference =
        Regex::new(r#"(?is)<sup[^>]*class="[^"]*\breference\b[^"]*"[^>]*>.*?</sup\s*>"#).unwrap();
    let tag = Regex::new(r"(?s)<[^>]+>").unwrap();
    let text = block.replace_all(html, " ");
    let text = reference.replace_all(&text, " ");
    let text = tag.replace_all(&text, " ");
    let text = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#160;", " ");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn clip(s: &str) -> String {
    let mut out: String = s.chars().take(80).collect();
    if s.chars().count() > 80 {
        out.push('…');
    }
    out
}

/// The current UTC time as RFC 3339 with second precision.
pub fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    rfc3339_from_unix(secs)
}

/// Civil date from days since the epoch (Howard Hinnant's algorithm).
pub fn rfc3339_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Mutex};

    #[test]
    fn text_normalisation_and_quote_matching() {
        let html = "<html><head><style>p{}</style><script>x()</script></head><body><h1>Scope</h1><p>The breadth, depth &amp; reach of a subject; a&nbsp;domain.</p></body></html>";
        assert_eq!(
            html_to_text(html),
            "Scope The breadth, depth & reach of a subject; a domain."
        );
        let page = normalise(&html_to_text(html));
        assert_eq!(
            match_quote(&page, "The breadth, depth & reach of a subject; a domain."),
            "present"
        );
        assert_eq!(
            match_quote(
                &page,
                "The breadth, depth and reach of a subject; a domain."
            ),
            "moved"
        );
        assert_eq!(
            match_quote(&page, "Something else entirely, about focus."),
            "missing"
        );
        assert_eq!(match_quote(&page, ""), "present");
    }

    /// A citation marker inside the sentence — rendered as a reference
    /// superscript, or copied into the quote as `[1]` — is not part of the
    /// passage: a quote that is otherwise verbatim still counts as present.
    #[test]
    fn citation_markers_do_not_hide_a_verbatim_passage() {
        let html = r##"<p>Technology (from Greek <i>techne</i>, "art"; and -logia<sup id="cite_ref-1" class="reference"><a href="#cite_note-1">[1]</a></sup>) is the collection of tools used by humans.<sup class="noprint Inline-Template">[<i>citation needed</i>]</sup> It occurs in eukaryotes.[2][3] Prokaryotes reproduce asexually.[note 4][a]</p>"##;
        let page = normalise(&html_to_text(html));
        assert_eq!(
            page,
            "technology from greek techne art and logia is the collection of tools used by humans it occurs in eukaryotes prokaryotes reproduce asexually"
        );
        assert_eq!(
            match_quote(
                &page,
                r#"Technology (from Greek techne, "art"; and -logia) is the collection of tools used by humans."#
            ),
            "present"
        );
        assert_eq!(
            match_quote(
                &page,
                "It occurs in eukaryotes.[1][2] Prokaryotes reproduce asexually."
            ),
            "present"
        );
        // A bracketed year or a long bracketed phrase is text, not a marker.
        assert_eq!(
            normalise("Born [1964] in [the same town]"),
            "born 1964 in the same town"
        );
    }

    #[test]
    fn rfc3339_dates() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_from_unix(1_788_998_400), "2026-09-10T00:00:00Z");
    }

    /// A local server: `/live` carries the quote, `/changed` a near-miss,
    /// `/gone` is 404, `/wayback/available` answers the availability API.
    fn serve(log: Arc<Mutex<Vec<String>>>) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let archive = format!("{base}/archive/20260101000000/gone");
        let base_in = base.clone();
        std::thread::spawn(move || {
            let base = base_in;
            for req in server.incoming_requests() {
                let url = req.url().to_string();
                log.lock().unwrap().push(url.clone());
                let (code, body) = if url == "/adrift" {
                    // Drifted from its quote, and no snapshot carries it.
                    (
                        200,
                        "<p>A sentence that has since been lightly reworded here.</p>".to_string(),
                    )
                } else if url == "/lostquote" {
                    (
                        200,
                        "<p>Nothing of the passage survives here.</p>".to_string(),
                    )
                } else if url.starts_with("/archive/") && url.ends_with("/relic") {
                    (
                        200,
                        "<p>The relic, as it read before the host went away.</p>".to_string(),
                    )
                } else if url == "/live" {
                    (200, "<p>Anything in Existence that can be distinguished from anything else.</p>".to_string())
                } else if url == "/drifted" {
                    (
                        200,
                        "<p>A page rewritten since it was quoted.</p>".to_string(),
                    )
                } else if url.starts_with("/cdx?") && url.contains("eras") {
                    // Six snapshots around 2015 (one carries the first
                    // sentence) and one in 2018 (carries the second).
                    let rows: Vec<String> = [
                        "20141001", "20141101", "20141201", "20150101", "20150201", "20150301",
                        "20180101",
                    ]
                    .iter()
                    .map(|ts| {
                        format!(r#"["k","{ts}000000","{base}/eras","text/html","200","{ts}","1"]"#)
                    })
                    .collect();
                    (
                        200,
                        format!(
                            r#"[["urlkey","timestamp","original","mimetype","statuscode","digest","length"],{}]"#,
                            rows.join(",")
                        ),
                    )
                } else if url.starts_with("/cdx?") {
                    (
                        200,
                        format!(
                            r#"[["urlkey","timestamp","original","mimetype","statuscode","digest","length"],["k","20150201000000","{base}/drifted","text/html","200","a","1"],["k","20150301000000","{base}/drifted","text/html","200","b","1"],["k","20140101000000","{base}/drifted","text/html","200","c","1"]]"#
                        ),
                    )
                } else if url == "/eras" {
                    (
                        200,
                        "<p>Rewritten twice since it was quoted.</p>".to_string(),
                    )
                } else if url.starts_with("/archive/20150201000000id_/") && url.ends_with("/eras") {
                    (200, "<p>First sentence, from the old page.</p>".to_string())
                } else if url.starts_with("/archive/20180101000000id_/") && url.ends_with("/eras") {
                    (200, "<p>Second sentence, added later.</p>".to_string())
                } else if url.starts_with("/archive/") && url.ends_with("/eras") {
                    (
                        200,
                        "<p>Filler that carries neither sentence.</p>".to_string(),
                    )
                } else if url.starts_with("/archive/20150201000000id_/") {
                    (
                        200,
                        "<p>Closest in time, but the sentence is not here.</p>".to_string(),
                    )
                } else if url.starts_with("/archive/20150301000000id_/") {
                    (200, "<p>Old page: Anything in Existence that can be distinguished from anything else.</p>".to_string())
                } else if url == "/changed" {
                    (200, "<p>Anything in Existence which can be distinguished from anything else.</p>".to_string())
                } else if url.starts_with("/wayback/available") {
                    // The relic's snapshot is one this server actually serves,
                    // so an unreachable host can be offered a working pin.
                    let (snap, ts) = if url.contains("relic") {
                        // A snapshot OF the requested URL, the way the real
                        // availability API answers -- the pin has to carry the
                        // source URL or it is not a pin of that source.
                        (
                            format!("{base}/archive/20150301000000/http://127.0.0.1:1/relic"),
                            "20150301000000",
                        )
                    } else {
                        (archive.clone(), "20260101000000")
                    };
                    (
                        200,
                        format!(
                            r#"{{"archived_snapshots":{{"closest":{{"available":true,"url":"{snap}","timestamp":"{ts}"}}}}}}"#
                        ),
                    )
                } else {
                    (404, "gone".to_string())
                };
                let resp = tiny_http::Response::from_string(body).with_status_code(code);
                let _ = req.respond(resp);
            }
        });
        base
    }

    fn node(title: &str, url: &str, quote: &str) -> String {
        format!(
            "# {title}\n\n## Ontology\n\nx\n\n## Axiology\n\nx\n\n## Epistemology\n\n<a href=\"{url}\" target=\"_blank\">{title} (source)</a>\n\n> {quote}\n"
        )
    }

    fn setup(tmp: &Path, base: &str) {
        let src = tmp.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            tmp.join("existence.toml"),
            "[meta]\nname = \"t\"\ndescription = \"d\"\n\n[rings.0]\nname = \"k\"\ndescription = \"c\"\nterms = [\"entity\", \"being\", \"soul\", \"ghost\"]\n",
        )
        .unwrap();
        let quote = "Anything in Existence that can be distinguished from anything else.";
        fs::write(
            src.join("entity.md"),
            node("Entity", &format!("{base}/live"), quote),
        )
        .unwrap();
        fs::write(
            src.join("being.md"),
            node("Being", &format!("{base}/changed"), quote),
        )
        .unwrap();
        fs::write(
            src.join("soul.md"),
            node("Soul", &format!("{base}/gone"), "Whatever it said."),
        )
        .unwrap();
        // Two URLs on a host with nothing listening: skipped after one attempt.
        fs::write(
            src.join("ghost.md"),
            format!(
                "# Ghost\n\n## Ontology\n\nx\n\n## Axiology\n\nx\n\n## Epistemology\n\n<a href=\"http://127.0.0.1:1/a\">a</a>\n\n> q\n\n<a href=\"http://127.0.0.1:1/b\">b</a>\n\n> q\n"
            ),
        )
        .unwrap();
    }

    fn opts(base: &str, rate_ms: u64) -> SourceOptions {
        SourceOptions {
            lock: PathBuf::from("audit/sources.lock.json"),
            rate: Duration::from_millis(rate_ms),
            wayback: format!("{base}/wayback/available"),
            cdx: format!("{base}/cdx"),
            archive_web: format!("{base}/archive"),
            archive_around: "20150101".into(),
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn fix_pins_a_verified_snapshot_beside_a_drifted_quote() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let base = serve(log.clone());
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path(), &base);
        let quote = "Anything in Existence that can be distinguished from anything else.";
        fs::write(
            tmp.path().join("src/mind.md"),
            node("Mind", &format!("{base}/drifted"), quote),
        )
        .unwrap();

        let findings = check(tmp.path(), &opts(&base, 10), true).unwrap();
        let pinned: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.check == "quote_missing" && f.term == "mind")
            .collect();
        assert_eq!(pinned.len(), 1, "{findings:?}");
        let pin = format!("{base}/archive/20150301000000/{base}/drifted");
        assert_eq!(
            pinned[0].fix.as_deref(),
            Some(format!("pin the archived copy {pin} beside the link").as_str())
        );
        assert!(pinned[0].fixed);
        let mind = fs::read_to_string(tmp.path().join("src/mind.md")).unwrap();
        assert!(
            mind.contains(&format!(
                "<a href=\"{base}/drifted\" target=\"_blank\">Mind (source)</a> <a href=\"{pin}\" target=\"_blank\">(archived 2015-03-01)</a>\n"
            )),
            "{mind}"
        );
        // The closest snapshot lacked the sentence and was skipped; the
        // farthest was never needed.
        let asked: Vec<String> = log.lock().unwrap().clone();
        assert!(
            asked
                .iter()
                .any(|u| u.starts_with("/archive/20150201000000id_/"))
        );
        assert!(
            !asked
                .iter()
                .any(|u| u.starts_with("/archive/20140101000000id_/"))
        );
        let lock = sources::read_lock(&tmp.path().join("audit/sources.lock.json"))
            .unwrap()
            .unwrap();
        let entry = &lock[&format!("{base}/drifted")];
        assert_eq!(entry.archives, [pin.clone()]);
        assert_eq!(entry.quotes["mind"], "archived");

        // Second run: the pin in the node is read back, the snapshot is
        // checked instead of searched for, and nothing is reported.
        log.lock().unwrap().clear();
        let again = check(tmp.path(), &opts(&base, 10), false).unwrap();
        assert!(again.iter().all(|f| f.term != "mind"), "{again:?}");
        let asked: Vec<String> = log.lock().unwrap().clone();
        assert!(!asked.iter().any(|u| u.starts_with("/cdx")));
        assert!(
            asked
                .iter()
                .any(|u| u.starts_with("/archive/20150301000000id_/"))
        );
        let mind_again = fs::read_to_string(tmp.path().join("src/mind.md")).unwrap();
        assert_eq!(mind, mind_again, "the pin is written once");
        assert_eq!(
            snapshot_date("http://web.archive.org/web/20150623010707/http://a.org/x").as_deref(),
            Some("2015-06-23")
        );
        assert_eq!(
            raw_snapshot_url("https://web.archive.org/web/20150623010707/http://a.org/x"),
            "https://web.archive.org/web/20150623010707id_/http://a.org/x"
        );
    }

    /// Quotes taken from a page in different years need one pin per year:
    /// each quote is verified on its own against the live page or any pin,
    /// the nearest window is tried first, the yearly sweep finds the rest,
    /// and each node's line gets only the pins its own quotes need.
    #[test]
    fn fix_pins_one_snapshot_per_era_and_only_the_pins_each_node_needs() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let base = serve(log.clone());
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path(), &base);
        let first = "First sentence, from the old page.";
        let second = "Second sentence, added later.";
        let url = format!("{base}/eras");
        fs::write(
            tmp.path().join("src/eras.md"),
            format!(
                "# Eras\n\n## Ontology\n\nx\n\n## Axiology\n\nx\n\n## Epistemology\n\n<a href=\"{url}\" target=\"_blank\">Eras (source)</a>\n\n> {first}\n\n> {second}\n"
            ),
        )
        .unwrap();
        fs::write(
            tmp.path().join("src/accord.md"),
            node("Accord", &url, second),
        )
        .unwrap();

        let findings = check(tmp.path(), &opts(&base, 10), true).unwrap();
        let pin_2015 = format!("{base}/archive/20150201000000/{url}");
        let pin_2018 = format!("{base}/archive/20180101000000/{url}");
        let pinned: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.check == "quote_missing" && f.fix.is_some())
            .collect();
        assert_eq!(pinned.len(), 2, "{findings:?}");
        assert!(pinned.iter().all(|f| f.fixed), "{pinned:?}");
        assert!(
            findings
                .iter()
                .all(|f| f.check != "quote_missing" || f.fix.is_some()),
            "every quote was found in some snapshot: {findings:?}"
        );
        let eras = fs::read_to_string(tmp.path().join("src/eras.md")).unwrap();
        assert!(
            eras.contains(&format!(
                "<a href=\"{url}\" target=\"_blank\">Eras (source)</a> <a href=\"{pin_2015}\" target=\"_blank\">(archived 2015-02-01)</a> <a href=\"{pin_2018}\" target=\"_blank\">(archived 2018-01-01)</a>\n"
            )),
            "{eras}"
        );
        let accord = fs::read_to_string(tmp.path().join("src/accord.md")).unwrap();
        assert!(
            accord.contains(&format!("</a> <a href=\"{pin_2018}\"")),
            "{accord}"
        );
        assert!(
            !accord.contains(&pin_2015),
            "a pin its quotes do not need: {accord}"
        );
        let asked: Vec<String> = log.lock().unwrap().clone();
        assert_eq!(
            asked
                .iter()
                .filter(|u| u.starts_with("/cdx?") && u.contains("eras"))
                .count(),
            1,
            "one CDX query per page per run"
        );
        assert_eq!(
            asked
                .iter()
                .filter(|u| u.starts_with("/archive/20180101000000id_/"))
                .count(),
            1,
            "each snapshot is fetched once per run"
        );
        let lock = sources::read_lock(&tmp.path().join("audit/sources.lock.json"))
            .unwrap()
            .unwrap();
        let entry = &lock[&url];
        assert_eq!(entry.quotes["eras"], "archived");
        assert_eq!(entry.quotes["accord"], "archived");
        assert!(entry.archives.contains(&pin_2015) && entry.archives.contains(&pin_2018));

        // Second run: both pins are read back from the nodes, no search.
        log.lock().unwrap().clear();
        let again = check(tmp.path(), &opts(&base, 10), false).unwrap();
        assert!(
            again.iter().all(|f| f.term != "eras" && f.term != "accord"),
            "{again:?}"
        );
        let asked: Vec<String> = log.lock().unwrap().clone();
        assert!(!asked.iter().any(|u| u.starts_with("/cdx")));
        assert_eq!(
            eras,
            fs::read_to_string(tmp.path().join("src/eras.md")).unwrap()
        );

        // The order snapshots are tried: the nearest window, then one per year.
        let rows: Vec<Vec<String>> = [
            "20141001", "20141101", "20141201", "20150101", "20150201", "20150301", "20180101",
            "20180601", "20120301",
        ]
        .iter()
        .map(|ts| vec!["k".into(), format!("{ts}000000")])
        .collect();
        let order = candidate_order(
            &[
                vec![vec!["urlkey".to_string(), "timestamp".to_string()]],
                rows,
            ]
            .concat(),
            "20150101",
        );
        assert_eq!(
            order,
            [
                "20150101000000",
                "20150201000000",
                "20150301000000",
                "20141201000000",
                "20141101000000",
                "20141001000000",
                "20120301000000",
                "20180101000000"
            ]
        );
    }

    #[test]
    fn source_pass_records_status_quotes_archive_and_skips_unreachable_hosts() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let base = serve(log.clone());
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path(), &base);

        let started = Instant::now();
        let findings = check(tmp.path(), &opts(&base, 150), false).unwrap();
        // live, changed, gone, wayback, one attempt at the dead host: 5 gaps
        // of ≥150ms between the first and last request.
        assert!(
            started.elapsed() >= Duration::from_millis(4 * 150),
            "rate limit not honoured"
        );

        let lock = sources::read_lock(&tmp.path().join("audit/sources.lock.json"))
            .unwrap()
            .unwrap();
        let live = &lock[&format!("{base}/live")];
        assert_eq!(live.status, Some(200));
        assert_eq!(live.quotes["entity"], "present");
        assert!(live.fetched_at.as_deref().unwrap().ends_with('Z'));
        assert_eq!(live.content_sha256.as_deref().map(str::len), Some(64));
        // A drifted quote the archive can anchor is recorded as archived, not
        // left as `moved` with nothing anyone could do about it.
        assert_eq!(lock[&format!("{base}/changed")].quotes["being"], "archived");
        let gone = &lock[&format!("{base}/gone")];
        assert_eq!(gone.status, Some(404));
        assert_eq!(
            gone.archives,
            [format!("{base}/archive/20260101000000/gone")]
        );
        assert_eq!(gone.quotes["soul"], "unchecked");
        let dead_a = &lock["http://127.0.0.1:1/a"];
        assert_eq!((dead_a.status, dead_a.fetched_at.as_deref()), (None, None));

        let by_check =
            |c: &str| -> Vec<&Finding> { findings.iter().filter(|f| f.check == c).collect() };
        assert_eq!(by_check("quote_missing").len(), 0);
        let moved = by_check("quote_moved");
        assert_eq!(moved.len(), 1);
        assert_eq!(
            (moved[0].term.as_str(), moved[0].severity.as_str()),
            ("being", "warning")
        );
        // It now carries a remedy: without one it could only be re-reported.
        assert!(
            moved[0]
                .fix
                .as_deref()
                .is_some_and(|f| f.contains("/archive/20150301000000/")),
            "{moved:?}"
        );
        let dead = by_check("dead_link");
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].term, "soul");
        assert_eq!(dead[0].severity, "error");
        assert!(dead[0].message.contains("HTTP 404"));
        assert!(
            dead[0]
                .fix
                .as_deref()
                .unwrap()
                .contains("/archive/20260101000000/gone")
        );
        assert!(!dead[0].fixed);
        // Every source on the dead host is reported, not just the first: each
        // one has its own pin to look for, and the host is fetched only once.
        let skipped = by_check("unreachable_host");
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert!(skipped.iter().all(|f| f.term == "ghost"));
        assert!(skipped.iter().all(|f| f.message.contains("127.0.0.1:1")));

        // The archive was asked about the dead link and about each source on
        // the unreachable host; the host itself was attempted once.
        let requests = log.lock().unwrap().clone();
        assert_eq!(
            requests
                .iter()
                .filter(|u| u.starts_with("/wayback"))
                .count(),
            3
        );
        assert!(requests.iter().any(|u| u.contains("gone")));
    }

    /// A node with a source anchor and one pin already beside it.
    fn pinned_node(title: &str, url: &str, pin: &str, quote: &str) -> String {
        format!(
            "# {title}\n\n## Ontology\n\nx\n\n## Axiology\n\nx\n\n## Epistemology\n\n<a href=\"{url}\" target=\"_blank\">{title} (source)</a> <a href=\"{pin}\" target=\"_blank\">(archived)</a>\n\n> {quote}\n"
        )
    }

    const DRIFTED: &str = "Anything in Existence that can be distinguished from anything else.";

    /// A passage the live page has drifted from is not a finding when a pin
    /// beside the link still carries it word for word: the pin is the citation
    /// of record, so nothing has been lost and there is nothing to report.
    #[test]
    fn a_drifted_quote_its_own_pin_carries_verbatim_is_not_reported() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let base = serve(log.clone());
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path(), &base);
        let url = format!("{base}/changed");
        let pin = format!("{base}/archive/20150301000000/{url}");
        fs::write(
            tmp.path().join("src/anchor.md"),
            pinned_node("Anchor", &url, &pin, DRIFTED),
        )
        .unwrap();
        let before = fs::read_to_string(tmp.path().join("src/anchor.md")).unwrap();

        let findings = check(tmp.path(), &opts(&base, 10), true).unwrap();
        assert!(findings.iter().all(|f| f.term != "anchor"), "{findings:?}");
        let lock = sources::read_lock(&tmp.path().join("audit/sources.lock.json"))
            .unwrap()
            .unwrap();
        assert_eq!(lock[&url].quotes["anchor"], "archived");
        assert_eq!(
            before,
            fs::read_to_string(tmp.path().join("src/anchor.md")).unwrap(),
            "the pin already there answered; nothing was rewritten"
        );
    }

    /// The resolution path `quote_moved` did not have: search the nearest
    /// snapshots for one that carries the passage verbatim, offer it as the
    /// fix, and write it. Without this the warning could only be re-reported
    /// every run, which is how sixty-one of them accumulated.
    #[test]
    fn fix_pins_a_snapshot_that_carries_a_drifted_quote_verbatim() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let base = serve(log.clone());
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path(), &base);
        let url = format!("{base}/changed");
        let pin = format!("{base}/archive/20150301000000/{url}");

        let findings = check(tmp.path(), &opts(&base, 10), true).unwrap();
        let moved: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.check == "quote_moved" && f.term == "being")
            .collect();
        assert_eq!(moved.len(), 1, "{findings:?}");
        assert_eq!(
            moved[0].fix.as_deref(),
            Some(format!("pin the archived copy {pin} beside the link").as_str())
        );
        assert!(moved[0].fixed);
        let being = fs::read_to_string(tmp.path().join("src/being.md")).unwrap();
        assert!(being.contains(&format!("<a href=\"{pin}\"")), "{being}");

        // Second run: the pin is read back and the warning is gone for good.
        let again = check(tmp.path(), &opts(&base, 10), false).unwrap();
        assert!(again.iter().all(|f| f.term != "being"), "{again:?}");
        assert_eq!(
            being,
            fs::read_to_string(tmp.path().join("src/being.md")).unwrap(),
            "the pin is written once"
        );
    }

    /// When no snapshot carries the passage verbatim the warning stands, with
    /// no fix — there is genuinely nothing to apply.
    #[test]
    fn a_drifted_quote_no_snapshot_carries_is_reported_without_a_fix() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let base = serve(log.clone());
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path(), &base);
        fs::write(
            tmp.path().join("src/adrift.md"),
            node(
                "Adrift",
                &format!("{base}/adrift"),
                "A sentence that has since been lightly reworded there.",
            ),
        )
        .unwrap();

        let findings = check(tmp.path(), &opts(&base, 10), true).unwrap();
        let mine: Vec<&Finding> = findings.iter().filter(|f| f.term == "adrift").collect();
        assert_eq!(mine.len(), 1, "{findings:?}");
        assert_eq!(mine[0].check, "quote_moved");
        assert_eq!(mine[0].severity, "warning");
        assert_eq!(mine[0].fix, None);
        assert!(
            mine[0].message.contains("no snapshot carries it verbatim"),
            "{:?}",
            mine[0]
        );
    }

    /// A pin whose own fetch failed was never read, so it cannot be reported
    /// as a pin that does not carry the passage. Saying otherwise turned an
    /// unreachable archive into an absent-quote error against a snapshot
    /// nobody had looked at.
    #[test]
    fn a_pin_that_could_not_be_fetched_is_not_an_absent_quote() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let base = serve(log.clone());
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path(), &base);
        let url = format!("{base}/lostquote");
        let dead_pin = format!("http://127.0.0.1:1/archive/20150101000000/{url}");
        fs::write(
            tmp.path().join("src/vault.md"),
            pinned_node(
                "Vault",
                &url,
                &dead_pin,
                "A passage that vanished from the page.",
            ),
        )
        .unwrap();

        let findings = check(tmp.path(), &opts(&base, 10), false).unwrap();
        let mine: Vec<&Finding> = findings.iter().filter(|f| f.term == "vault").collect();
        assert_eq!(mine.len(), 1, "{findings:?}");
        assert_eq!(
            (mine[0].check.as_str(), mine[0].severity.as_str()),
            ("pin_unreachable", "warning"),
            "{:?}",
            mine[0]
        );
        assert!(mine[0].message.contains("127.0.0.1:1"), "{:?}", mine[0]);
        assert!(
            findings
                .iter()
                .all(|f| f.check != "quote_missing" || f.term != "vault"),
            "{findings:?}"
        );
    }

    /// An unreachable source host says nothing about the archive host, so the
    /// archive is still searched and the pin still offered. Without this a
    /// dead host warned every run with no fix to apply, while a 4xx dead link
    /// pinned itself and cleared.
    #[test]
    fn an_unreachable_host_is_offered_and_keeps_its_archived_copy() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let base = serve(log.clone());
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path(), &base);
        fs::write(
            tmp.path().join("src/relic.md"),
            node("Relic", "http://127.0.0.1:1/relic", "Whatever it said."),
        )
        .unwrap();
        let pin = format!("{base}/archive/20150301000000/http://127.0.0.1:1/relic");

        let findings = check(tmp.path(), &opts(&base, 10), true).unwrap();
        let mine: Vec<&Finding> = findings.iter().filter(|f| f.term == "relic").collect();
        assert_eq!(mine.len(), 1, "{findings:?}");
        assert_eq!(mine[0].check, "unreachable_host");
        assert_eq!(
            mine[0].fix.as_deref(),
            Some(format!("pin the archived copy {pin} beside the link").as_str())
        );
        assert!(mine[0].fixed);
        let relic = fs::read_to_string(tmp.path().join("src/relic.md")).unwrap();
        assert!(relic.contains(&format!("<a href=\"{pin}\"")), "{relic}");

        // Second run: the pin answers for the citation and the host's
        // unreachability is no longer worth reporting.
        let again = check(tmp.path(), &opts(&base, 10), false).unwrap();
        assert!(again.iter().all(|f| f.term != "relic"), "{again:?}");
    }

    #[test]
    fn fix_swaps_the_dead_link_for_the_archived_copy() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let base = serve(log);
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path(), &base);
        let findings = check(tmp.path(), &opts(&base, 10), true).unwrap();
        let dead: Vec<&Finding> = findings.iter().filter(|f| f.check == "dead_link").collect();
        assert!(dead[0].fixed);
        let soul = fs::read_to_string(tmp.path().join("src/soul.md")).unwrap();
        assert!(soul.contains(&format!("href=\"{base}/archive/20260101000000/gone\"")));
        assert!(!soul.contains(&format!("href=\"{base}/gone\"")));
        // A live link is untouched.
        let entity = fs::read_to_string(tmp.path().join("src/entity.md")).unwrap();
        assert!(entity.contains(&format!("href=\"{base}/live\"")));
    }
}
