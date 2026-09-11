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

use crate::commands::audit::Finding;
use crate::commands::sources::{self, Lock, LockEntry, TermSources};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
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
/// How many snapshots are tried before a drifted quote stays unpinned.
const PIN_CANDIDATES: usize = 6;

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
    unreachable: BTreeSet<String>,
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
            unreachable: BTreeSet::new(),
        }
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
    // URLs whose snapshot was found this run, for `--fix` to write beside the anchor.
    let mut pinned_now: Vec<String> = Vec::new();

    for (url, entry) in lock.iter_mut() {
        let Some(host) = host_of(url) else { continue };
        if client.unreachable.contains(&host) {
            continue;
        }
        match client.get(url) {
            Fetch::Unreachable(reason) => {
                client.unreachable.insert(host.clone());
                findings.push(source_finding(
                    "unreachable_host",
                    "warning",
                    &entry.cited_by[0],
                    format!("host {host} is unreachable ({reason}); its sources were skipped"),
                    None,
                ));
            }
            Fetch::Response { status, body } => {
                entry.fetched_at = Some(now.clone());
                entry.status = Some(status);
                if status >= 400 {
                    if entry.archive.is_none() {
                        entry.archive = wayback_lookup(&mut client, &opts.wayback, url);
                    }
                    for term in &entry.cited_by {
                        findings.push(source_finding(
                            "dead_link",
                            "error",
                            term,
                            format!("source {url} returned HTTP {status}"),
                            entry
                                .archive
                                .as_ref()
                                .map(|a| format!("replace the link with the archived copy {a}")),
                        ));
                    }
                    continue;
                }
                let text = html_to_text(&body);
                entry.content_sha256 = Some(sha256_hex(&text));
                let page = normalise(&text);
                let live: Vec<(String, Vec<String>, &'static str, Option<String>)> = entry
                    .cited_by
                    .iter()
                    .map(|term| {
                        let quotes = quotes_for(&all, term, url);
                        let (status_word, worst) = quote_status(&page, &quotes);
                        (term.clone(), quotes, status_word, worst)
                    })
                    .collect();
                // A quote the live page has lost is looked for in the pinned
                // snapshot, or a snapshot is searched for and pinned.
                let mut archived_page: Option<String> = None;
                if live.iter().any(|(_, _, s, _)| *s == "missing") {
                    let all_quotes: Vec<String> = live
                        .iter()
                        .flat_map(|(_, q, _, _)| q.iter().cloned())
                        .collect();
                    match &entry.archive {
                        Some(pin) => archived_page = fetch_archived_page(&mut client, pin),
                        None => {
                            if let Some((pin, text)) = find_pin(&mut client, opts, url, &all_quotes)
                            {
                                entry.archive = Some(pin);
                                archived_page = Some(text);
                                pinned_now.push(url.clone());
                            }
                        }
                    }
                }
                for (term, quotes, status_word, worst) in live {
                    let term = &term;
                    if status_word == "missing"
                        && let Some(archived) = &archived_page
                        && quote_status(archived, &quotes).0 != "missing"
                    {
                        entry.quotes.insert(term.clone(), "archived".to_string());
                        if pinned_now.contains(url) {
                            let pin = entry.archive.clone().unwrap_or_default();
                            findings.push(source_finding(
                                "quote_missing",
                                "error",
                                term,
                                format!(
                                    "quoted passage no longer found on {url} but is in the snapshot {pin}: \u{201c}{}\u{201d}",
                                    clip(&worst.unwrap_or_default())
                                ),
                                Some(format!("pin the archived copy {pin} beside the link")),
                            ));
                        }
                        continue;
                    }
                    entry.quotes.insert(term.clone(), status_word.to_string());
                    match status_word {
                        "missing" => findings.push(source_finding(
                            "quote_missing",
                            "error",
                            term,
                            match &entry.archive {
                                Some(pin) => format!(
                                    "quoted passage found neither on {url} nor in its pinned copy {pin}: \u{201c}{}\u{201d}",
                                    clip(&worst.unwrap_or_default())
                                ),
                                None => format!(
                                    "quoted passage no longer found on {url}: \u{201c}{}\u{201d}",
                                    clip(&worst.unwrap_or_default())
                                ),
                            },
                            None,
                        )),
                        "moved" => findings.push(source_finding(
                            "quote_moved",
                            "warning",
                            term,
                            format!(
                                "quoted passage on {url} has changed but still matches: \u{201c}{}\u{201d}",
                                clip(&worst.unwrap_or_default())
                            ),
                            None,
                        )),
                        _ => {}
                    }
                }
            }
        }
    }

    if fix {
        apply_dead_link_fixes(ontology_dir, &lock, &mut findings)?;
        apply_pin_fixes(ontology_dir, &lock, &pinned_now, &mut findings)?;
    }
    sources::write_lock(&lock_path, &lock)?;
    Ok(findings)
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
        .filter(|(_, e)| e.status.is_some_and(|s| s >= 400) && e.archive.is_some())
        .collect();
    for (url, entry) in dead {
        let archive = entry.archive.as_ref().unwrap();
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
/// after the anchor of every URL pinned this run, in every citing node.
fn apply_pin_fixes(
    ontology_dir: &Path,
    lock: &Lock,
    pinned_now: &[String],
    findings: &mut [Finding],
) -> Result<(), String> {
    let src_dir = ontology_dir.join("src");
    for url in pinned_now {
        let Some(entry) = lock.get(url) else { continue };
        let Some(archive) = entry.archive.as_ref() else {
            continue;
        };
        let label = match snapshot_date(archive) {
            Some(date) => format!("(archived {date})"),
            None => "(archived)".to_string(),
        };
        let pin = format!(" <a href=\"{archive}\" target=\"_blank\">{label}</a>");
        let needle = format!("href=\"{url}\"");
        for term in &entry.cited_by {
            let path = src_dir.join(format!("{term}.md"));
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
            let mut changed = false;
            let mut out = String::with_capacity(content.len() + 128);
            for line in content.split_inclusive('\n') {
                if let Some(at) = line.find(&needle)
                    && !line.contains(archive.as_str())
                    && let Some(close) = line[at..].find("</a>")
                {
                    let cut = at + close + "</a>".len();
                    out.push_str(&line[..cut]);
                    out.push_str(&pin);
                    out.push_str(&line[cut..]);
                    changed = true;
                } else {
                    out.push_str(line);
                }
            }
            if !changed {
                continue;
            }
            std::fs::write(&path, out)
                .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
            for f in findings.iter_mut().filter(|f| {
                f.check == "quote_missing" && f.term == *term && f.message.contains(url.as_str())
            }) {
                f.fixed = true;
            }
        }
    }
    Ok(())
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

/// The normalised text of a pinned snapshot, if it can be fetched.
fn fetch_archived_page(client: &mut Client, archive: &str) -> Option<String> {
    match client.get(&raw_snapshot_url(archive)) {
        Fetch::Response { status, body } if status < 400 => Some(normalise(&html_to_text(&body))),
        _ => None,
    }
}

/// Search the CDX index for a snapshot of `url` that still carries every
/// quote, nearest `archive_around` first; at most `PIN_CANDIDATES` fetches.
fn find_pin(
    client: &mut Client,
    opts: &SourceOptions,
    url: &str,
    quotes: &[String],
) -> Option<(String, String)> {
    let query = format!(
        "{}?url={}&output=json&filter=statuscode:200&collapse=timestamp:6&limit=200",
        opts.cdx,
        percent_encode(url)
    );
    let Fetch::Response { status, body } = client.get(&query) else {
        return None;
    };
    if status >= 400 {
        return None;
    }
    let rows: Vec<Vec<String>> = serde_json::from_str(&body).ok()?;
    let around: i64 = opts.archive_around.get(0..8)?.parse().ok()?;
    let mut stamps: Vec<(i64, String)> = rows
        .iter()
        .skip(1)
        .filter_map(|row| {
            let ts = row.get(1)?;
            let day: i64 = ts.get(0..8)?.parse().ok()?;
            Some(((day - around).abs(), ts.clone()))
        })
        .collect();
    stamps.sort();
    stamps.dedup();
    for (_, ts) in stamps.into_iter().take(PIN_CANDIDATES) {
        let raw = format!("{}/{ts}id_/{url}", opts.archive_web);
        let Fetch::Response { status, body } = client.get(&raw) else {
            continue;
        };
        if status >= 400 {
            continue;
        }
        let page = normalise(&html_to_text(&body));
        if quote_status(&page, quotes).0 != "missing" {
            return Some((format!("{}/{ts}/{url}", opts.archive_web), page));
        }
    }
    None
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

/// The worst status over a term's quotes on a page, with the quote that
/// produced it. No quotes leaves the term `unchecked`.
fn quote_status(page: &str, quotes: &[String]) -> (&'static str, Option<String>) {
    let mut worst: (&'static str, Option<String>) = ("unchecked", None);
    let rank = |s: &str| match s {
        "missing" => 3,
        "moved" => 2,
        "present" => 1,
        _ => 0,
    };
    for quote in quotes {
        let status = match_quote(page, quote);
        if rank(status) > rank(worst.0) {
            worst = (status, Some(quote.clone()));
        }
    }
    worst
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

/// Lowercase, letters and digits only, single spaces.
pub fn normalise(text: &str) -> String {
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
    let tag = Regex::new(r"(?s)<[^>]+>").unwrap();
    let text = block.replace_all(html, " ");
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
                let (code, body) = if url == "/live" {
                    (200, "<p>Anything in Existence that can be distinguished from anything else.</p>".to_string())
                } else if url == "/drifted" {
                    (
                        200,
                        "<p>A page rewritten since it was quoted.</p>".to_string(),
                    )
                } else if url.starts_with("/cdx?") {
                    (
                        200,
                        format!(
                            r#"[["urlkey","timestamp","original","mimetype","statuscode","digest","length"],["k","20150201000000","{base}/drifted","text/html","200","a","1"],["k","20150301000000","{base}/drifted","text/html","200","b","1"],["k","20140101000000","{base}/drifted","text/html","200","c","1"]]"#
                        ),
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
                    (
                        200,
                        format!(
                            r#"{{"archived_snapshots":{{"closest":{{"available":true,"url":"{archive}","timestamp":"20260101000000"}}}}}}"#
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
        assert_eq!(entry.archive.as_deref(), Some(pin.as_str()));
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
        assert_eq!(lock[&format!("{base}/changed")].quotes["being"], "moved");
        let gone = &lock[&format!("{base}/gone")];
        assert_eq!(gone.status, Some(404));
        assert_eq!(
            gone.archive.as_deref(),
            Some(format!("{base}/archive/20260101000000/gone").as_str())
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
        let skipped = by_check("unreachable_host");
        assert_eq!(skipped.len(), 1, "one warning per host, not per URL");
        assert_eq!(skipped[0].term, "ghost");
        assert!(skipped[0].message.contains("127.0.0.1:1"));

        // The wayback endpoint was asked once, for the dead link only.
        let requests = log.lock().unwrap().clone();
        assert_eq!(
            requests
                .iter()
                .filter(|u| u.starts_with("/wayback"))
                .count(),
            1
        );
        assert!(requests.iter().any(|u| u.contains("gone")));
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
