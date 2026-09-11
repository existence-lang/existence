//! `existence audit --semantic` — an LLM reads each node's lay definition
//! next to the lay definitions of the nodes it links and says whether any
//! pair contradicts. Report only, never on the PR path, and never run unless
//! asked for: it costs money and needs a key.
//!
//! Verdicts are cached in `audit/semantic.cache.json` under a content hash
//! of (provider, model, prompt version, the term's definition, and every
//! neighbour's definition), so a rerun with no changes makes zero model
//! calls and a weekly run only re-judges nodes whose neighbourhood changed.
//!
//! Providers: `anthropic` (Messages API, `ANTHROPIC_API_KEY`) and `openai`
//! (Chat Completions, `OPENAI_API_KEY`). `--endpoint` overrides the base URL
//! so tests can run against a local server.

use crate::commands::audit::Finding;
use crate::commands::source_check::rfc3339_now;
use crate::markdown;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Default cache location, relative to the ontology directory.
pub const DEFAULT_CACHE: &str = "audit/semantic.cache.json";

/// Bumped whenever the prompt changes, so cached verdicts are re-judged.
const PROMPT_VERSION: &str = "1";

const SYSTEM_PROMPT: &str = "You audit an ontology of terms for contradictions. You are given one term's lay definition and the lay definitions of the terms it links to. Decide whether the term's definition contradicts any linked definition: a claim in one that cannot be true if the other is true. Different emphasis, scope, level of detail, or a definition that merely depends on another are not contradictions. Reply with exactly one JSON object and nothing else: {\"contradiction\": true or false, \"with\": \"<slug of the linked term, or empty>\", \"a\": \"<the sentence from the term's definition, quoted verbatim>\", \"b\": \"<the conflicting sentence from the linked definition, quoted verbatim>\", \"why\": \"<one sentence>\"}. When there is no contradiction, set contradiction to false and leave the other fields empty.";

/// How the semantic pass runs.
#[derive(Debug, Clone)]
pub struct SemanticOptions {
    /// `anthropic` or `openai`.
    pub provider: String,
    /// Model id; `None` picks the provider default.
    pub model: Option<String>,
    /// Cache path, relative to the ontology directory unless absolute.
    pub cache: PathBuf,
    /// Base URL override (tests); `None` uses the provider's API host.
    pub endpoint: Option<String>,
    /// API key; `None` reads `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`.
    pub api_key: Option<String>,
    /// Judge at most this many nodes that are not already cached.
    pub limit: Option<usize>,
    /// Minimum gap between model calls.
    pub rate: Duration,
    pub timeout: Duration,
}

impl Default for SemanticOptions {
    fn default() -> Self {
        SemanticOptions {
            provider: "anthropic".into(),
            model: None,
            cache: PathBuf::from(DEFAULT_CACHE),
            endpoint: None,
            api_key: None,
            limit: None,
            rate: Duration::from_millis(500),
            timeout: Duration::from_secs(120),
        }
    }
}

/// One cached verdict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Verdict {
    pub term: String,
    pub provider: String,
    pub model: String,
    pub judged_at: String,
    pub contradiction: bool,
    #[serde(default)]
    pub with: String,
    #[serde(default)]
    pub a: String,
    #[serde(default)]
    pub b: String,
    #[serde(default)]
    pub why: String,
}

/// The cache: content hash → verdict.
pub type Cache = BTreeMap<String, Verdict>;

/// What one pass did, for the operator.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub judged: usize,
    pub cached: usize,
    pub calls: usize,
    pub skipped: usize,
}

/// One node with its neighbourhood, the unit the model judges.
struct Neighbourhood {
    term: String,
    definition: String,
    neighbours: Vec<(String, String)>,
}

/// Run the pass and return the findings; also prints the call statistics to
/// stderr so a rerun can be seen to make zero calls.
pub fn check(ontology_dir: &Path, opts: &SemanticOptions) -> Result<Vec<Finding>, String> {
    let (findings, stats) = run(ontology_dir, opts)?;
    eprintln!(
        "semantic: {} node(s) judged, {} from cache, {} model call(s), {} skipped (no definition or no links)",
        stats.judged, stats.cached, stats.calls, stats.skipped
    );
    Ok(findings)
}

/// [`check`] with the statistics returned instead of printed.
pub fn run(ontology_dir: &Path, opts: &SemanticOptions) -> Result<(Vec<Finding>, Stats), String> {
    let cache_path = if opts.cache.is_absolute() {
        opts.cache.clone()
    } else {
        ontology_dir.join(&opts.cache)
    };
    let mut cache = read_cache(&cache_path)?;
    let model = opts
        .model
        .clone()
        .unwrap_or_else(|| default_model(&opts.provider).to_string());
    let hoods = neighbourhoods(ontology_dir)?;

    let mut client: Option<Client> = None;
    let mut stats = Stats::default();
    let mut findings = Vec::new();
    let mut budget = opts.limit;

    for hood in &hoods {
        if hood.definition.is_empty() || hood.neighbours.is_empty() {
            stats.skipped += 1;
            continue;
        }
        let key = cache_key(&opts.provider, &model, hood);
        let verdict = if let Some(v) = cache.get(&key) {
            stats.cached += 1;
            v.clone()
        } else {
            if budget == Some(0) {
                stats.skipped += 1;
                continue;
            }
            let c = match client.as_mut() {
                Some(c) => c,
                None => client.insert(Client::new(opts)?),
            };
            let v = c.judge(&opts.provider, &model, hood)?;
            stats.calls += 1;
            if let Some(b) = budget.as_mut() {
                *b -= 1;
            }
            cache.insert(key, v.clone());
            // Persist after every call so an interrupted run keeps its work.
            write_cache(&cache_path, &cache)?;
            v
        };
        stats.judged += 1;
        if verdict.contradiction {
            let with = if verdict.with.is_empty() {
                "a linked term".to_string()
            } else {
                format!("`{}`", verdict.with)
            };
            findings.push(Finding {
                class: "semantic".into(),
                check: "contradiction".into(),
                severity: "warning".into(),
                term: hood.term.clone(),
                message: format!(
                    "lay definition may contradict {with} ({}): \u{201c}{}\u{201d} vs \u{201c}{}\u{201d} — {}",
                    verdict.model, verdict.a, verdict.b, verdict.why
                ),
                fix: None,
                fixed: false,
            });
        }
    }
    if stats.calls == 0 && !cache_path.exists() {
        write_cache(&cache_path, &cache)?;
    }
    Ok((findings, stats))
}

fn default_model(provider: &str) -> &'static str {
    match provider {
        "openai" => "gpt-5",
        _ => "claude-opus-5",
    }
}

/// Every node with its lay definition and its Ontology-section neighbours'
/// lay definitions, in term order.
fn neighbourhoods(ontology_dir: &Path) -> Result<Vec<Neighbourhood>, String> {
    let src_dir = ontology_dir.join("src");
    let terms = markdown::list_terms(&src_dir)?;
    let known: BTreeSet<&String> = terms.iter().collect();
    let mut contents = BTreeMap::new();
    for term in &terms {
        let path = src_dir.join(format!("{term}.md"));
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        contents.insert(term.clone(), content);
    }
    let definition = |term: &str| -> String {
        contents
            .get(term)
            .and_then(|c| markdown::extract_definition(c))
            .map(|d| markdown::plain_text(&d))
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for term in &terms {
        let content = &contents[term];
        let ontology = markdown::Node::parse(content)
            .ok()
            .and_then(|n| n.ontology)
            .unwrap_or_default();
        let neighbours: Vec<(String, String)> = markdown::extract_unique_links(&ontology)
            .into_iter()
            .filter(|t| t != term && known.contains(t))
            .map(|t| {
                let d = definition(&t);
                (t, d)
            })
            .filter(|(_, d)| !d.is_empty())
            .collect();
        out.push(Neighbourhood {
            term: term.clone(),
            definition: definition(term),
            neighbours,
        });
    }
    Ok(out)
}

/// SHA-256 over everything the verdict depends on.
fn cache_key(provider: &str, model: &str, hood: &Neighbourhood) -> String {
    let payload = serde_json::json!({
        "v": PROMPT_VERSION,
        "provider": provider,
        "model": model,
        "term": hood.term,
        "definition": hood.definition,
        "neighbours": hood.neighbours,
    });
    let digest = Sha256::digest(payload.to_string().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn user_prompt(hood: &Neighbourhood) -> String {
    let mut s = format!(
        "Term `{}`: {}\n\nLinked terms:\n",
        hood.term, hood.definition
    );
    for (t, d) in &hood.neighbours {
        s.push_str(&format!("- `{t}`: {d}\n"));
    }
    s
}

/// A rate-limited HTTP client for one provider.
struct Client {
    agent: ureq::Agent,
    base: String,
    key: String,
    rate: Duration,
    last: Option<Instant>,
}

impl Client {
    fn new(opts: &SemanticOptions) -> Result<Self, String> {
        let (env, host) = match opts.provider.as_str() {
            "anthropic" => ("ANTHROPIC_API_KEY", "https://api.anthropic.com"),
            "openai" => ("OPENAI_API_KEY", "https://api.openai.com"),
            other => {
                return Err(format!(
                    "unknown provider '{other}' (expected anthropic or openai)"
                ));
            }
        };
        let key = match &opts.api_key {
            Some(k) => k.clone(),
            None => std::env::var(env).map_err(|_| {
                format!(
                    "{env} is not set; the semantic pass needs a {} key",
                    opts.provider
                )
            })?,
        };
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(opts.timeout))
            .user_agent(format!(
                "existence-audit/{} (+https://github.com/existence-lang/existence)",
                env!("CARGO_PKG_VERSION")
            ))
            .build();
        Ok(Client {
            agent: config.new_agent(),
            base: opts
                .endpoint
                .clone()
                .unwrap_or_else(|| host.to_string())
                .trim_end_matches('/')
                .to_string(),
            key,
            rate: opts.rate,
            last: None,
        })
    }

    fn pace(&mut self) {
        if let Some(last) = self.last {
            let elapsed = last.elapsed();
            if elapsed < self.rate {
                std::thread::sleep(self.rate - elapsed);
            }
        }
        self.last = Some(Instant::now());
    }

    fn judge(
        &mut self,
        provider: &str,
        model: &str,
        hood: &Neighbourhood,
    ) -> Result<Verdict, String> {
        self.pace();
        let text = match provider {
            "anthropic" => self.anthropic(model, hood)?,
            _ => self.openai(model, hood)?,
        };
        let json = extract_json(&text).ok_or_else(|| {
            format!(
                "model reply for `{}` carried no JSON object: {}",
                hood.term,
                clip(&text)
            )
        })?;
        Ok(Verdict {
            term: hood.term.clone(),
            provider: provider.to_string(),
            model: model.to_string(),
            judged_at: rfc3339_now(),
            contradiction: json["contradiction"].as_bool().unwrap_or(false),
            with: json["with"].as_str().unwrap_or("").to_string(),
            a: json["a"].as_str().unwrap_or("").to_string(),
            b: json["b"].as_str().unwrap_or("").to_string(),
            why: json["why"].as_str().unwrap_or("").to_string(),
        })
    }

    /// Messages API: one user turn, the instructions as `system`.
    fn anthropic(&mut self, model: &str, hood: &Neighbourhood) -> Result<String, String> {
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 1024,
            "system": SYSTEM_PROMPT,
            "messages": [{"role": "user", "content": user_prompt(hood)}],
        });
        let url = format!("{}/v1/messages", self.base);
        let mut resp = self
            .agent
            .post(&url)
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|e| format!("anthropic request failed: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        if status >= 400 {
            return Err(format!("anthropic returned HTTP {status}: {}", clip(&text)));
        }
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("anthropic reply is not JSON: {e}"))?;
        if json["stop_reason"].as_str() == Some("refusal") {
            return Err(format!(
                "anthropic declined to judge `{}` ({})",
                hood.term,
                json["stop_details"]["category"]
                    .as_str()
                    .unwrap_or("no category")
            ));
        }
        let out: String = json["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        Ok(out)
    }

    /// Chat Completions: system + user, JSON object mode.
    fn openai(&mut self, model: &str, hood: &Neighbourhood) -> Result<String, String> {
        let body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": user_prompt(hood)},
            ],
            "response_format": {"type": "json_object"},
        });
        let url = format!("{}/v1/chat/completions", self.base);
        let mut resp = self
            .agent
            .post(&url)
            .header("authorization", &format!("Bearer {}", self.key))
            .header("content-type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|e| format!("openai request failed: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        if status >= 400 {
            return Err(format!("openai returned HTTP {status}: {}", clip(&text)));
        }
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("openai reply is not JSON: {e}"))?;
        Ok(json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }
}

/// The first `{ … }` object in a model reply, tolerant of prose around it.
pub fn extract_json(text: &str) -> Option<serde_json::Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

fn clip(s: &str) -> String {
    let mut out: String = s.chars().take(160).collect();
    if s.chars().count() > 160 {
        out.push('…');
    }
    out
}

pub fn read_cache(path: &Path) -> Result<Cache, String> {
    if !path.exists() {
        return Ok(Cache::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("Cannot parse {}: {e}", path.display()))
}

pub fn write_cache(path: &Path, cache: &Cache) -> Result<(), String> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    }
    let mut text = serde_json::to_string_pretty(cache)
        .map_err(|e| format!("JSON serialization error: {e}"))?;
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Mutex};

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
            "[meta]\nname = \"t\"\ndescription = \"d\"\n\n[rings.0]\nname = \"k\"\ndescription = \"c\"\nterms = [\"entity\", \"existence\", \"void\", \"lonely\"]\n",
        )
        .unwrap();
        // Planted contradiction: entity says everything is an entity;
        // void says it is in Existence but is not an entity.
        fs::write(
            src.join("entity.md"),
            node(
                "Entity",
                "Anything in [Existence](./existence.md) is an entity, without exception.",
            ),
        )
        .unwrap();
        fs::write(
            src.join("void.md"),
            node(
                "Void",
                "The void is in [Existence](./existence.md) yet is not an [entity](./entity.md).",
            ),
        )
        .unwrap();
        fs::write(
            src.join("existence.md"),
            node(
                "Existence",
                "Everything that is; the set of all [entities](./entity.md).",
            ),
        )
        .unwrap();
        // No links: skipped without a call.
        fs::write(
            src.join("lonely.md"),
            node("Lonely", "A term that links nothing."),
        )
        .unwrap();
    }

    /// A model stand-in: `/v1/messages` answers in Messages-API shape and
    /// `/v1/chat/completions` in Chat-Completions shape; only the prompt
    /// for `void` gets a contradiction verdict. Records every request body.
    fn serve(log: Arc<Mutex<Vec<String>>>) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        std::thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).unwrap();
                log.lock().unwrap().push(body.clone());
                let verdict = if body.contains("Term `void`") {
                    r#"{"contradiction": true, "with": "entity", "a": "The void is in Existence yet is not an entity.", "b": "Anything in Existence is an entity, without exception.", "why": "One admits a non-entity in Existence, the other forbids it."}"#
                } else {
                    r#"{"contradiction": false, "with": "", "a": "", "b": "", "why": ""}"#
                };
                let url = req.url().to_string();
                let reply = if url == "/v1/messages" {
                    serde_json::json!({"id": "msg_1", "type": "message", "role": "assistant", "model": "test", "stop_reason": "end_turn",
                        "content": [{"type": "text", "text": format!("Here is my verdict:\n{verdict}")}]})
                } else {
                    serde_json::json!({"choices": [{"message": {"role": "assistant", "content": verdict}}]})
                };
                let _ = req.respond(
                    tiny_http::Response::from_string(reply.to_string()).with_status_code(200),
                );
            }
        });
        base
    }

    fn opts(base: &str, provider: &str) -> SemanticOptions {
        SemanticOptions {
            provider: provider.into(),
            model: Some("test-model".into()),
            cache: PathBuf::from(DEFAULT_CACHE),
            endpoint: Some(base.to_string()),
            api_key: Some("test-key".into()),
            limit: None,
            rate: Duration::from_millis(1),
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn planted_contradiction_is_caught_and_a_rerun_makes_zero_calls() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let base = serve(log.clone());
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());

        let (findings, stats) = run(tmp.path(), &opts(&base, "anthropic")).unwrap();
        assert_eq!(
            stats,
            Stats {
                judged: 3,
                cached: 0,
                calls: 3,
                skipped: 1
            }
        );
        assert_eq!(findings.len(), 1, "{findings:#?}");
        let f = &findings[0];
        assert_eq!(
            (
                f.class.as_str(),
                f.check.as_str(),
                f.severity.as_str(),
                f.term.as_str()
            ),
            ("semantic", "contradiction", "warning", "void")
        );
        assert_eq!(
            f.message,
            "lay definition may contradict `entity` (test-model): \u{201c}The void is in Existence yet is not an entity.\u{201d} vs \u{201c}Anything in Existence is an entity, without exception.\u{201d} — One admits a non-entity in Existence, the other forbids it."
        );
        // The request carried the term and its neighbours' definitions.
        let bodies = log.lock().unwrap().clone();
        assert_eq!(bodies.len(), 3);
        let void_req: serde_json::Value =
            serde_json::from_str(bodies.iter().find(|b| b.contains("Term `void`")).unwrap())
                .unwrap();
        assert_eq!(void_req["model"], "test-model");
        assert!(
            void_req["system"]
                .as_str()
                .unwrap()
                .contains("Reply with exactly one JSON object")
        );
        let content = void_req["messages"][0]["content"].as_str().unwrap();
        assert!(content.contains("- `entity`: Anything in Existence is an entity"));
        assert!(content.contains("- `existence`: Everything that is"));

        // Second run: everything from cache, no requests.
        let (again, stats2) = run(tmp.path(), &opts(&base, "anthropic")).unwrap();
        assert_eq!(
            stats2,
            Stats {
                judged: 3,
                cached: 3,
                calls: 0,
                skipped: 1
            }
        );
        assert_eq!(again.len(), 1);
        assert_eq!(
            log.lock().unwrap().len(),
            3,
            "a rerun with no changes must make zero model calls"
        );

        // Editing one node's definition re-judges that node only.
        fs::write(
            tmp.path().join("src/lonely.md"),
            node("Lonely", "Still links nothing."),
        )
        .unwrap();
        fs::write(
            tmp.path().join("src/existence.md"),
            node(
                "Existence",
                "All that is; the set of all [entities](./entity.md).",
            ),
        )
        .unwrap();
        let (_, stats3) = run(tmp.path(), &opts(&base, "anthropic")).unwrap();
        // existence changed, so existence itself and both nodes linking it are re-judged.
        assert_eq!(stats3.calls, 3);
        assert_eq!(stats3.cached, 0);

        let cache = read_cache(&tmp.path().join(DEFAULT_CACHE)).unwrap();
        assert_eq!(cache.len(), 6);
        assert!(cache.values().all(|v| v.provider == "anthropic"
            && v.model == "test-model"
            && v.judged_at.ends_with('Z')));
    }

    #[test]
    fn openai_provider_and_limit() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let base = serve(log.clone());
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let mut o = opts(&base, "openai");
        o.limit = Some(1);
        let (_, stats) = run(tmp.path(), &o).unwrap();
        assert_eq!(stats.calls, 1);
        assert_eq!(stats.skipped, 3, "the limit skips the rest");
        let body: serde_json::Value = serde_json::from_str(&log.lock().unwrap()[0]).unwrap();
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["messages"][0]["role"], "system");
        // A second pass with the limit lifted judges the remaining two.
        o.limit = None;
        let (_, stats2) = run(tmp.path(), &o).unwrap();
        assert_eq!((stats2.calls, stats2.cached), (2, 1));
    }

    #[test]
    fn missing_key_and_unknown_provider_cannot_run() {
        let tmp = tempfile::tempdir().unwrap();
        setup(tmp.path());
        let mut o = SemanticOptions {
            api_key: None,
            ..opts("http://127.0.0.1:1", "anthropic")
        };
        // SAFETY: tests in this module do not otherwise touch the variable.
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        assert!(
            run(tmp.path(), &o)
                .unwrap_err()
                .contains("ANTHROPIC_API_KEY is not set")
        );
        o.provider = "mistral".into();
        o.api_key = Some("k".into());
        assert!(
            run(tmp.path(), &o)
                .unwrap_err()
                .contains("unknown provider")
        );
    }

    #[test]
    fn json_is_extracted_from_prose() {
        let v = extract_json("Sure.\n{\"contradiction\": true, \"with\": \"x\"}\nDone.").unwrap();
        assert_eq!(v["with"], "x");
        assert!(extract_json("no object here").is_none());
    }
}
