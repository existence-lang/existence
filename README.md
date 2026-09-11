# existence

CLI for the [Existence](https://github.com/existence-lang/ontology) ontology framework.

Navigate, validate, and visualize ontology term definitions organized in scoping rings.

## Install

Prebuilt binaries for Linux (x86_64, aarch64), macOS (Intel, Apple Silicon),
and Windows (x86_64) ship with every release. One line, no toolchain:

```bash
# Linux, macOS, and Git Bash / MSYS2 on Windows
curl -fsSL https://raw.githubusercontent.com/existence-lang/existence/main/install.sh | sh
```

```powershell
# Windows PowerShell
irm https://raw.githubusercontent.com/existence-lang/existence/main/install.ps1 | iex
```

The installer puts `existence` (and its alias `xist`) in `~/.local/bin`
(`%LOCALAPPDATA%\existence\bin` on Windows) and prints a PATH hint if needed.
Pin a version with `EXISTENCE_VERSION=v0.5.0`, or change the target directory
with `EXISTENCE_INSTALL_DIR`. The same line works in CI:

```yaml
- run: curl -fsSL https://raw.githubusercontent.com/existence-lang/existence/main/install.sh | EXISTENCE_VERSION=v0.5.0 sh
- run: ~/.local/bin/existence --ontology ontology lint
```

With a Rust toolchain, `cargo install existence` works too. Or build from source:

```bash
git clone https://github.com/existence-lang/existence
cd existence
cargo install --path .
```

## Usage

All commands auto-detect the ontology directory. If you're inside an ontology repo (one with `existence.toml`), it just works. Otherwise, use `--ontology <path>` or run `existence fetch` first.

### Lookup a term

```bash
# Print the full markdown definition
existence lookup existence

# Output as structured JSON (title, ontology, axiology, epistemology sections)
existence lookup existence --json
```

### Navigate scoping rings

```bash
# List all rings and their terms
existence scope

# List only Ring 0 (kernel) terms
existence scope 0

# List Ring 1 (software) terms
existence scope 1
```

### Lint ontology nodes

```bash
# Validate all nodes in src/
existence lint

# Validate a specific directory or file
existence lint src/existence.md
```

Checks:
- Title (`# Term`) is present
- Required sections: `## [Ontology]`, `## [Axiology]`, `## [Epistemology]`
- Broken links: `[term](./term.md)` references where `src/term.md` doesn't exist
- Typed links (warning): a link title outside `broader` | `narrower` | `related`,
  or one target typed both broader and narrower

Warnings (advisory — never fail the run; `###` subsections are non-normative):
- `## Ontology` subsections outside `Pattern` | `Senses` (the pattern-node shape)
- `## Epistemology` subsections outside `Cultural Definition` | `Sources`, `Pattern Expression` | `Examples`

Exit code 0 if clean, 1 if errors found.

### Term index

```bash
# Markdown index grouped by ring: one bullet per term with its lay definition
existence toc

# Write it to the repo root; term links point at src/<term>.md
existence toc -o TERMS.md

# Written elsewhere, links are rebased to the relative path back to src/;
# --base overrides that (also for stdout, where it defaults to `src`)
existence toc -o docs/terms.md
existence toc --base https://example.org/ontology/src

# One ring, or JSON for a site generator / context pack
existence toc --ring 0
existence toc --format json

# Fail when the index would degrade: an Ontology section that does not open
# with a plain sentence, a ring term with no node file, a node in no ring
existence toc --check
```

Each bullet is `- [**Title**](path) — lay definition` (SPEC rule 2: the first
line of the Ontology section), under a `## Ring N — name` heading with the
ring's description from `existence.toml`. Node links inside a definition
(`[return](./return.md)`) are rewritten to `<base>/return.md` so they stay
live. Terms in `src/` that no ring declares are listed under `## Unringed`,
and ring terms without a file are marked missing, so the index doubles as a
manifest check. JSON carries both the slug (`term`, what `lookup` takes) and
the `title`, plus `definition` (markdown) and `definition_text` (plain).

### Generate relationship graph

```bash
# DOT format (pipe to graphviz)
existence graph | dot -Tsvg -o ontology.svg

# Filter to a specific ring
existence graph 0 | dot -Tpng -o kernel.png

# JSON adjacency list
existence graph --format json
```

### Export as RDF (SKOS)

```bash
# Turtle (default) — one skos:Concept per term, one skos:Collection per ring
existence export > ontology.ttl

# JSON-LD with a prefix @context and a flat @graph
existence export --format jsonld > ontology.jsonld

# Only Ring 0, under your own base IRI
existence export 0 --base-iri https://example.org/ontology/
```

Mapping: title → `skos:prefLabel`; the lay definition (first Ontology line) →
`skos:definition`; the Ontology / Axiology / Ethics / Epistemology sections →
`xl:ontology` / `xl:axiology` / `xl:ethics` / `xl:epistemology` literals;
`[term](./term.md "broader")` / `"narrower"` links → `skos:broader` /
`skos:narrower` (the inverse is emitted on the target); untitled
`[term](./term.md)` links → `skos:related`; external `href` /
`[text](https://…)` references → `rdfs:seeAlso`; rings → `skos:Collection` with `skos:member` and `xl:ring`;
Ring 0 terms → `skos:hasTopConcept` of the scheme. Term IRIs are
`{base_iri}{term}` — set `meta.base_iri` (and optionally `meta.vocab_iri`) in
`existence.toml` or pass `--base-iri`. The Turtle output parses with
`rapper -i turtle -c ontology.ttl` and loads into any SPARQL store.

### Query with SPARQL

SPARQL support is an opt-in cargo feature so the default binary stays small:

```bash
cargo install existence --features sparql
```

```bash
# SELECT / ASK print as a text table by default; --format json|csv|tsv|xml
existence sparql 'SELECT ?t WHERE { ?t a skos:Concept } ORDER BY ?t'
existence sparql 'ASK { :entity skos:broader :existence }'

# Multi-line queries from stdin
existence sparql - --format json <<'Q'
SELECT ?t (COUNT(?r) AS ?links) WHERE { ?t a skos:Concept . ?t skos:related ?r }
GROUP BY ?t ORDER BY DESC(?links) LIMIT 10
Q

# Concepts nothing links to
existence sparql 'SELECT ?t WHERE { ?t a skos:Concept FILTER NOT EXISTS { ?x skos:related ?t } }'
```

`skos:`, `rdfs:`, `dcterms:`, `xsd:`, `xl:`, and `:` (the base IRI, so `:entity`)
are pre-declared unless the query declares them itself. The graph queried is
exactly what `existence export` prints; CONSTRUCT / DESCRIBE results come back
as Turtle.

`existence serve` exposes the same store as a
[SPARQL Protocol](https://www.w3.org/TR/sparql11-protocol/) endpoint on
localhost (`--port`, default 3030): `GET /sparql?query=…`, `POST /sparql` with
an `application/sparql-query` body or a form `query=`, and `Accept` selecting
JSON (default), `text/csv`, `text/tab-separated-values`, or
`application/sparql-results+xml`.

### Fetch an ontology

```bash
# Clone from GitHub
existence fetch github:existence-lang/ontology

# Pull all sources defined in existence.toml
existence fetch
```

Sources are stored in `~/.existence/sources/{org}/{repo}/`.

## Configuration

Ontologies are configured via `existence.toml`:

```toml
[meta]
name = "existence-lang/ontology"
description = "Reference existential ontology"
# Optional: RDF export identity (defaults shown)
base_iri = "https://existence-lang.github.io/ontology/"
vocab_iri = "https://existence-lang.github.io/ontology/vocab#"

[rings.0]
name = "kernel"
description = "14 universal terms, always loaded"
terms = ["existence", "entity", "abstraction", "scope", "context", ...]

[rings.1]
name = "software"
description = "The DDD bridge"
terms = ["project", "model", "algorithm", ...]

[sources]
upstream = "github:existence-lang/ontology"
```

## Commands (v0.3.0)

| Command | Description | Status |
|---------|-------------|--------|
| `lookup <term>` | Read a node's full definition | Implemented |
| `scope [ring]` | List terms at a ring level | Implemented |
| `lint [path]` | Validate nodes against SPEC.md rules | Implemented |
| `graph [ring]` | Generate term relationship graph (DOT/JSON) | Implemented |
| `toc` | Term index with lay definitions, grouped by ring (Markdown/JSON) | Implemented |
| `export [ring]` | Export the ontology as SKOS RDF (Turtle/JSON-LD) | Implemented |
| `sparql <query>` | Run a SPARQL query over the exported ontology | Implemented (`--features sparql`) |
| `serve` | SPARQL Protocol endpoint on localhost | Implemented (`--features sparql`) |
| `fetch [source]` | Clone or pull ontology from GitHub | Implemented |
| `install` | Set up ~/.claude integration | Planned |
| `build-site` | Generate static site + JSON API | Planned |
| `context <domain>` | Suggest relevant terms for a domain | Planned |

## License

Apache-2.0
