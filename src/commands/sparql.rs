//! `existence sparql` and the SPARQL endpoint of `existence serve`.
//!
//! Both load the ontology through `export` (so the graph is exactly what
//! `existence export` prints) into an in-memory Oxigraph store. They are
//! compiled only with the `sparql` cargo feature so the default binary stays
//! small; without it the commands explain how to enable them.

// Without the feature the protocol helpers are only reached from tests.
#![cfg_attr(not(feature = "sparql"), allow(dead_code))]

#[cfg(not(feature = "sparql"))]
use std::path::Path;

/// Output formats for SELECT / ASK results. CONSTRUCT / DESCRIBE always
/// produce Turtle.
pub const FORMATS: &[&str] = &["text", "json", "csv", "tsv", "xml"];

#[cfg(not(feature = "sparql"))]
const UNAVAILABLE: &str = "SPARQL support is not compiled into this binary. Rebuild with \
`cargo install existence --features sparql` (or `cargo build --features sparql`).";

/// Decode a percent-encoded query-string value (`+` is a space).
pub fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = &value[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Pull one parameter out of an `application/x-www-form-urlencoded` body or
/// query string.
pub fn form_param(encoded: &str, name: &str) -> Option<String> {
    encoded
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| url_decode(value))
}

/// Map an HTTP `Accept` header to a result format name.
pub fn format_for_accept(accept: Option<&str>) -> &'static str {
    let accept = accept.unwrap_or("").to_ascii_lowercase();
    if accept.contains("text/csv") {
        "csv"
    } else if accept.contains("text/tab-separated-values") {
        "tsv"
    } else if accept.contains("sparql-results+xml") || accept.contains("application/xml") {
        "xml"
    } else {
        "json"
    }
}

/// The SPARQL Protocol request shapes `serve` accepts, reduced to the query
/// text: `GET /sparql?query=…`, `POST` with `application/sparql-query`, or
/// `POST` with a form body carrying `query=`.
pub fn query_from_request(
    method: &str,
    url: &str,
    content_type: Option<&str>,
    body: &str,
) -> Result<String, &'static str> {
    let (path, query_string) = url.split_once('?').unwrap_or((url, ""));
    if path != "/sparql" && path != "/" {
        return Err("not found: the endpoint is /sparql");
    }
    let query = match method {
        "GET" => form_param(query_string, "query"),
        "POST" => {
            let content_type = content_type.unwrap_or("").to_ascii_lowercase();
            if content_type.starts_with("application/x-www-form-urlencoded") {
                form_param(body, "query")
            } else {
                Some(body.to_string())
            }
        }
        _ => return Err("method not allowed: use GET or POST"),
    };
    match query {
        Some(q) if !q.trim().is_empty() => Ok(q),
        _ => Err("bad request: no SPARQL query (GET ?query=… or POST the query)"),
    }
}

#[cfg(not(feature = "sparql"))]
pub fn run(
    _ontology_dir: &Path,
    _query: &str,
    _format: &str,
    _base_iri: Option<&str>,
) -> Result<(), String> {
    Err(UNAVAILABLE.to_string())
}

#[cfg(not(feature = "sparql"))]
pub fn serve(_ontology_dir: &Path, _port: u16, _base_iri: Option<&str>) -> Result<(), String> {
    Err(UNAVAILABLE.to_string())
}

#[cfg(feature = "sparql")]
pub use imp::{run, serve};

#[cfg(feature = "sparql")]
mod imp {
    use super::{FORMATS, format_for_accept, query_from_request};
    use crate::commands::export;
    use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
    use oxigraph::sparql::results::{QueryResultsFormat, QueryResultsSerializer};
    use oxigraph::sparql::{QueryResults, SparqlEvaluator};
    use oxigraph::store::Store;
    use std::io::{Read, Write};
    use std::path::Path;

    const SKOS: &str = "http://www.w3.org/2004/02/skos/core#";
    const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
    const DCTERMS: &str = "http://purl.org/dc/terms/";
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

    /// An in-memory store holding the exported ontology.
    pub struct Engine {
        store: Store,
        /// `(prefix, iri)` pairs declared for every query unless the query
        /// declares the prefix itself. `""` is the base IRI (`:entity`).
        prefixes: Vec<(String, String)>,
    }

    impl Engine {
        /// Export the ontology and load it.
        pub fn load(ontology_dir: &Path, base_iri: Option<&str>) -> Result<Engine, String> {
            let graph = export::build(ontology_dir, None, base_iri)?;
            let turtle = export::to_turtle(&graph);
            let store = Store::new().map_err(|e| format!("cannot create in-memory store: {e}"))?;
            store
                .load_from_slice(RdfParser::from_format(RdfFormat::Turtle), turtle.as_bytes())
                .map_err(|e| format!("failed to load the exported Turtle: {e}"))?;
            let prefixes = vec![
                ("skos".to_string(), SKOS.to_string()),
                ("rdfs".to_string(), RDFS.to_string()),
                ("dcterms".to_string(), DCTERMS.to_string()),
                ("xsd".to_string(), XSD.to_string()),
                ("xl".to_string(), graph.vocab_iri.clone()),
                (String::new(), graph.base_iri.clone()),
            ];
            Ok(Engine { store, prefixes })
        }

        /// Prepend `PREFIX` declarations the query does not already make.
        pub fn with_prefixes(&self, query: &str) -> String {
            let lower = query.to_ascii_lowercase();
            let mut out = String::new();
            for (name, iri) in &self.prefixes {
                if !lower.contains(&format!("prefix {name}:")) {
                    out.push_str(&format!("PREFIX {name}: <{iri}>\n"));
                }
            }
            out.push_str(query);
            out
        }

        /// Run a query and write the results. Returns the media type written.
        pub fn query(
            &self,
            query: &str,
            format: &str,
            out: &mut dyn Write,
        ) -> Result<&'static str, String> {
            if !FORMATS.contains(&format) {
                return Err(format!(
                    "Unknown result format '{format}' (expected one of {})",
                    FORMATS.join(", ")
                ));
            }
            let prepared = SparqlEvaluator::new()
                .parse_query(&self.with_prefixes(query))
                .map_err(|e| format!("SPARQL syntax error: {e}"))?;
            let results = prepared
                .on_store(&self.store)
                .execute()
                .map_err(|e| format!("query failed: {e}"))?;
            let io = |e: std::io::Error| format!("write error: {e}");
            match results {
                QueryResults::Boolean(value) => {
                    if format == "text" {
                        writeln!(out, "{value}").map_err(io)?;
                        return Ok("text/plain");
                    }
                    let (results_format, media) = results_format(format);
                    QueryResultsSerializer::from_format(results_format)
                        .serialize_boolean_to_writer(&mut *out, value)
                        .map_err(io)?;
                    Ok(media)
                }
                QueryResults::Solutions(solutions) => {
                    let variables = solutions.variables().to_vec();
                    if format == "text" {
                        let header: Vec<&str> = variables.iter().map(|v| v.as_str()).collect();
                        writeln!(out, "{}", header.join("\t")).map_err(io)?;
                        for solution in solutions {
                            let solution = solution.map_err(|e| format!("query failed: {e}"))?;
                            let row: Vec<String> = solution
                                .values()
                                .iter()
                                .map(|term| {
                                    term.as_ref().map(|t| t.to_string()).unwrap_or_default()
                                })
                                .collect();
                            writeln!(out, "{}", row.join("\t")).map_err(io)?;
                        }
                        return Ok("text/plain");
                    }
                    let (results_format, media) = results_format(format);
                    let mut serializer = QueryResultsSerializer::from_format(results_format)
                        .serialize_solutions_to_writer(&mut *out, variables)
                        .map_err(io)?;
                    for solution in solutions {
                        let solution = solution.map_err(|e| format!("query failed: {e}"))?;
                        serializer.serialize(solution.iter()).map_err(io)?;
                    }
                    serializer.finish().map_err(io)?;
                    Ok(media)
                }
                QueryResults::Graph(triples) => {
                    let mut serializer = RdfSerializer::from_format(RdfFormat::Turtle);
                    for (name, iri) in &self.prefixes {
                        if !name.is_empty() {
                            serializer = serializer
                                .with_prefix(name, iri)
                                .map_err(|e| format!("bad prefix IRI {iri}: {e}"))?;
                        }
                    }
                    let mut writer = serializer.for_writer(&mut *out);
                    for triple in triples {
                        let triple = triple.map_err(|e| format!("query failed: {e}"))?;
                        writer.serialize_triple(&triple).map_err(io)?;
                    }
                    writer.finish().map_err(io)?;
                    Ok("text/turtle")
                }
            }
        }
    }

    fn results_format(format: &str) -> (QueryResultsFormat, &'static str) {
        match format {
            "csv" => (QueryResultsFormat::Csv, "text/csv"),
            "tsv" => (QueryResultsFormat::Tsv, "text/tab-separated-values"),
            "xml" => (QueryResultsFormat::Xml, "application/sparql-results+xml"),
            _ => (QueryResultsFormat::Json, "application/sparql-results+json"),
        }
    }

    /// `existence sparql <query>`: `-` reads the query from stdin.
    pub fn run(
        ontology_dir: &Path,
        query: &str,
        format: &str,
        base_iri: Option<&str>,
    ) -> Result<(), String> {
        let query = if query == "-" {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("cannot read query from stdin: {e}"))?;
            buf
        } else {
            query.to_string()
        };
        let engine = Engine::load(ontology_dir, base_iri)?;
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        engine.query(&query, format, &mut lock)?;
        lock.flush().map_err(|e| format!("write error: {e}"))?;
        Ok(())
    }

    /// `existence serve`: a SPARQL Protocol endpoint at `/sparql` on localhost.
    pub fn serve(ontology_dir: &Path, port: u16, base_iri: Option<&str>) -> Result<(), String> {
        let engine = Engine::load(ontology_dir, base_iri)?;
        let server = tiny_http::Server::http(("127.0.0.1", port))
            .map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
        println!("SPARQL endpoint: http://127.0.0.1:{port}/sparql");
        println!("  GET  /sparql?query=<urlencoded query>");
        println!("  POST /sparql  (application/sparql-query body, or form query=…)");
        println!(
            "  Accept: application/sparql-results+json (default) | text/csv | text/tab-separated-values | application/sparql-results+xml"
        );
        for mut request in server.incoming_requests() {
            let response = respond(&engine, &mut request);
            // A client that hung up is not the server's problem.
            let _ = request.respond(response);
        }
        Ok(())
    }

    fn respond(
        engine: &Engine,
        request: &mut tiny_http::Request,
    ) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
        let content_type = header_value(request, "Content-Type");
        let accept = header_value(request, "Accept");
        let mut body = String::new();
        if request.as_reader().read_to_string(&mut body).is_err() {
            return text_response(400, "bad request: body is not UTF-8");
        }
        let method = request.method().as_str().to_ascii_uppercase();
        let query = match query_from_request(&method, request.url(), content_type.as_deref(), &body)
        {
            Ok(q) => q,
            Err(msg) => {
                let status = if msg.starts_with("not found") {
                    404
                } else if msg.starts_with("method") {
                    405
                } else {
                    400
                };
                return text_response(status, msg);
            }
        };
        let format = format_for_accept(accept.as_deref());
        let mut buf = Vec::new();
        match engine.query(&query, format, &mut buf) {
            Ok(media) => tiny_http::Response::from_data(buf)
                .with_status_code(200)
                .with_header(content_type_header(media)),
            Err(msg) => text_response(400, &msg),
        }
    }

    fn header_value(request: &tiny_http::Request, name: &str) -> Option<String> {
        request
            .headers()
            .iter()
            .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str().to_string())
    }

    fn content_type_header(media: &str) -> tiny_http::Header {
        tiny_http::Header::from_bytes("Content-Type", media)
            .expect("static media types are valid header values")
    }

    fn text_response(status: u16, msg: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
        tiny_http::Response::from_string(msg)
            .with_status_code(status)
            .with_header(content_type_header("text/plain"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_decode_handles_percent_and_plus() {
        assert_eq!(url_decode("a+b%20c%3F"), "a b c?");
        assert_eq!(url_decode("100%"), "100%");
        assert_eq!(url_decode("%zz"), "%zz");
        assert_eq!(
            form_param("format=json&query=SELECT+%2A+WHERE+%7B%7D", "query").as_deref(),
            Some("SELECT * WHERE {}")
        );
        assert_eq!(form_param("a=1", "query"), None);
    }

    #[test]
    fn accept_header_picks_format() {
        assert_eq!(format_for_accept(None), "json");
        assert_eq!(format_for_accept(Some("text/csv")), "csv");
        assert_eq!(format_for_accept(Some("text/tab-separated-values")), "tsv");
        assert_eq!(
            format_for_accept(Some("application/sparql-results+xml")),
            "xml"
        );
        assert_eq!(format_for_accept(Some("*/*")), "json");
    }

    #[test]
    fn request_shapes() {
        assert_eq!(
            query_from_request("GET", "/sparql?query=ASK+%7B%7D", None, "").unwrap(),
            "ASK {}"
        );
        assert_eq!(
            query_from_request(
                "POST",
                "/sparql",
                Some("application/x-www-form-urlencoded"),
                "query=ASK+%7B%7D"
            )
            .unwrap(),
            "ASK {}"
        );
        assert_eq!(
            query_from_request(
                "POST",
                "/sparql",
                Some("application/sparql-query"),
                "ASK {}"
            )
            .unwrap(),
            "ASK {}"
        );
        assert!(
            query_from_request("GET", "/nope?query=x", None, "")
                .unwrap_err()
                .starts_with("not found")
        );
        assert!(
            query_from_request("PUT", "/sparql", None, "")
                .unwrap_err()
                .starts_with("method")
        );
        assert!(
            query_from_request("GET", "/sparql", None, "")
                .unwrap_err()
                .starts_with("bad request")
        );
    }
}

#[cfg(all(test, feature = "sparql"))]
mod engine_tests {
    use super::imp::Engine;

    fn sample_ontology() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            dir.path().join("existence.toml"),
            "[meta]\nname = \"sample\"\ndescription = \"sample\"\n\n[rings.0]\nname = \"kernel\"\ndescription = \"core\"\nterms = [\"existence\", \"entity\"]\n",
        )
        .unwrap();
        std::fs::write(
            src.join("existence.md"),
            "# Existence\n\n## [Ontology](./ontology.md)\n\nEverything.\n\n## [Axiology](./axiology.md)\n\nA\n\n## [Epistemology](./epistemology.md)\n\nE\n",
        )
        .unwrap();
        std::fs::write(
            src.join("entity.md"),
            "# Entity\n\n## [Ontology](./ontology.md)\n\nAny information in [Existence](./existence.md \"broader\").\n\n## [Axiology](./axiology.md)\n\nA\n\n## [Epistemology](./epistemology.md)\n\nE\n",
        )
        .unwrap();
        dir
    }

    fn run(engine: &Engine, query: &str, format: &str) -> (String, &'static str) {
        let mut buf = Vec::new();
        let media = engine.query(query, format, &mut buf).unwrap();
        (String::from_utf8(buf).unwrap(), media)
    }

    #[test]
    fn select_ask_construct_with_implicit_prefixes() {
        let dir = sample_ontology();
        let engine = Engine::load(dir.path(), Some("https://example.org/o/")).unwrap();

        let (text, media) = run(
            &engine,
            "SELECT ?t WHERE { ?t a skos:Concept } ORDER BY ?t",
            "text",
        );
        assert_eq!(media, "text/plain");
        assert_eq!(
            text,
            "t\n<https://example.org/o/entity>\n<https://example.org/o/existence>\n"
        );

        let (json, media) = run(
            &engine,
            "SELECT ?label WHERE { :entity skos:prefLabel ?label }",
            "json",
        );
        assert_eq!(media, "application/sparql-results+json");
        let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(doc["head"]["vars"][0], "label");
        assert_eq!(doc["results"]["bindings"][0]["label"]["value"], "Entity");
        assert_eq!(doc["results"]["bindings"][0]["label"]["xml:lang"], "en");

        let (ask, _) = run(&engine, "ASK { :entity skos:broader :existence }", "text");
        assert_eq!(ask, "true\n");
        let (ask, _) = run(&engine, "ASK { :existence skos:broader :entity }", "text");
        assert_eq!(ask, "false\n");

        let (csv, media) = run(
            &engine,
            "SELECT (COUNT(?c) AS ?n) WHERE { ?c a skos:Concept }",
            "csv",
        );
        assert_eq!(media, "text/csv");
        assert!(csv.trim().ends_with('2'), "{csv}");

        let (ttl, media) = run(
            &engine,
            "CONSTRUCT { ?s skos:narrower ?o } WHERE { ?s skos:narrower ?o }",
            "text",
        );
        assert_eq!(media, "text/turtle");
        assert!(ttl.contains("skos:narrower"), "{ttl}");
        assert!(ttl.contains("existence"), "{ttl}");
    }

    #[test]
    fn explicit_prefix_is_not_redeclared_and_errors_surface() {
        let dir = sample_ontology();
        let engine = Engine::load(dir.path(), None).unwrap();
        let q = "PREFIX skos: <http://example.org/other#> ASK { ?s skos:prefLabel ?o }";
        assert_eq!(engine.with_prefixes(q).matches("PREFIX skos:").count(), 1);
        let (ask, _) = run(&engine, q, "text");
        assert_eq!(ask, "false\n");

        let mut buf = Vec::new();
        let err = engine
            .query("SELECT ?x WHERE {", "text", &mut buf)
            .unwrap_err();
        assert!(err.starts_with("SPARQL syntax error"), "{err}");
        let err = engine.query("ASK {}", "yaml", &mut buf).unwrap_err();
        assert!(err.contains("Unknown result format 'yaml'"), "{err}");
    }
}
