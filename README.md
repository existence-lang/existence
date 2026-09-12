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

### Sources

```bash
# Every external source each node cites, with the passages it quotes
existence sources
existence sources entity
existence sources --json

# Derive the audit lockfile (audit/sources.lock.json, relative to the ontology)
existence sources --lock
existence sources --lock path/to/sources.lock.json
```

Sources are derived from the markdown SPEC rules 7 and 8 already mandate: an
`<a href="…" target="_blank">` anchor followed by `>` blockquotes of the
passage relied on, or the inline `> <a href="…">Label</a>: passage` form. One
entry per anchor, in document order, with its label and quoted lines; a
blockquote block ends at the next anchor, heading, or prose line, so an anchor
with nothing quoted under it (a keynote video, a mid-paragraph reference) is
listed with no quotes.

`--lock` aggregates the same data by URL into the lockfile the audit will
maintain: `cited_by` (terms citing the URL), `fetched_at`, `status`,
`content_sha256`, `archives` (the pinned Wayback snapshots), and `quotes`, a
per-term status. Listing never touches the network, so a new entry carries
`fetched_at: null` and `quotes: { term: "unchecked" }`; rewriting the lock
keeps the fetch results already recorded for URLs and terms that still cite,
and drops entries nothing cites any more.

### Audit

```bash
# Structure and manifest checks, as text; exit 0 clean, 1 findings, 2 could not run
existence audit --structure

# The report JSON is the interface; text is a view of it
existence audit --format json
existence audit --format json -o audit/report.json

# Apply the safe resolutions first (today: append `.md` to a suffix-less link
# whose target node exists), then report what is left
existence audit --fix
```

`--structure` wraps `lint` (errors and warnings) and `toc --check` (lay
definition opens with a plain sentence, ring terms with no node file, nodes
in no ring), and adds two checks neither of those sees: node links that miss
the `.md` suffix (`[environment](./environment)`), which lint does not count
as links and the toc rebaser does not rewrite; and near-duplicate slugs
(`signal`/`signals`, `agree`/`agreement`), paired by shared stem and reported
only when their lay definitions also overlap by at least 0.25 Jaccard, with
that score in the message so the pair reads as merge or keep. A shared stem
alone is not a duplicate: a philosophy ontology separates a verb from its
noun on purpose (`exist`/`existence`, `redefine`/`redefinition`,
`abstract`/`abstraction` all score 0.00-0.03), so those stay silent.

`--contradictions` is report only: cycles in the broader graph after
`"narrower"` links are inverted the way `export` does (`A` broader `B` and
`B` broader `A`, or longer; an error, since the hierarchy cannot be
published); lay definitions that define each other (2- and 3-cycles in the
graph of links that appear in first sentences only, reported with each
sentence side by side); and known terms named in a lay definition that
never links them.

`--sources` is the network class and never runs unless asked for. It
re-fetches every URL in `audit/sources.lock.json` (one request per second,
`--rate-ms` to change it, with a `User-Agent`), records the status, a SHA-256
of the page's text, and per citing term whether each quoted passage is still
there: `present`, `moved` (a window of the page matches at ≥ 0.9
similarity), or `missing`. A dead link or a missing quote is an error, a
moved quote a warning, a changed page with its quotes intact is only
recorded. A host that cannot be reached is skipped with one warning, its
URLs keeping their previous lock values. Dead links get a Wayback snapshot
pinned through the availability API (`--wayback` to point at another
endpoint), and `--fix` rewrites the citing node's `href` to that copy.

A quote that has left a page that is still live is looked for in the past
instead: the Wayback CDX index (`--cdx`) lists the page's snapshots, the six
nearest `--archive-around` (default `20150101`, when most nodes were written)
are fetched first, and a snapshot carrying every passage the node has lost is
pinned in the lock. `--fix` writes it beside the anchor as a second link on
the same line:

```html
<a href="http://en.wiktionary.org/wiki/scope" target="_blank">scope (wiktionary)</a> <a href="https://web.archive.org/web/20150221114818/http://en.wiktionary.org/wiki/scope" target="_blank">(archived 2015-02-21)</a>
```

`sources` reads that pair as one source with a pin, not two sources; from
then on a passage missing from the live page but present in a pinned copy
is recorded as `archived` and not reported, and only a passage absent from
the live page and every pin is an error. Each passage is verified on its
own, so a node whose quotes from one page were taken in different years
carries one pin per year: when no snapshot in the nearest window carries
every lost passage, the ones covering the most are pinned, then one snapshot
per calendar year over the rest of the archive is tried for what is left,
up to twenty fetches per search. Each node's line gets only the pins its own
quotes need. A passage no snapshot carries is reported as `missing` with no
fix, for a person to requote or replace.

`--mirrors` checks the surfaces that copy the ontology, declared in
`existence.toml`:

```toml
[[mirrors]]
path = "../philosophy/src"   # node-per-file copy: lay definitions compared per term
kind = "nodes"
optional = true              # skip silently when the path is absent (another machine)

[[mirrors]]
path = "terms.md"            # generated by `existence toc`; --fix regenerates it
kind = "toc"

[[mirrors]]
path = "~/.claude/CLAUDE.md" # a `| **Term** | summary |` table; rows scored against the node
kind = "table"
optional = true
```

A `nodes` mirror is reported as a per-term diff of lay definitions with the
markdown flattened to prose, so a `"broader"` link title, a link target
spelling, or a missing Axiology section is not drift. A `toc` mirror is regenerated and compared
byte for byte; `--fix` rewrites it. A `table` mirror is report only: a row
whose term has no node, or whose summary shares too few words with the
node's lay definition, is listed with both texts.

`--semantic` hands an LLM each node's lay definition together with the lay
definitions of the nodes its Ontology section links, and asks for a
contradiction verdict with the two conflicting sentences quoted. Report
only; it costs money and needs `ANTHROPIC_API_KEY` (or `OPENAI_API_KEY`
with `--provider openai`), so it never runs on the PR path and is not part
of `--all`. Verdicts are cached in `audit/semantic.cache.json` under a hash
of the provider, model, prompt, and every definition involved, so a rerun on
an unchanged ontology makes zero model calls and a weekly run re-judges
only the nodes whose neighbourhood changed. `--model` picks the model
(default `claude-opus-5`), `--limit N` caps the number of uncached nodes
judged in one run, `--endpoint` points at another API host.

```bash
existence audit --semantic                    # judges every node once
existence audit --semantic                    # zero model calls: all cached
existence audit --semantic --limit 10         # budget a first pass
existence audit --semantic --provider openai --model gpt-5
```

With no class flag the offline classes run; `--all` adds the network
pass.

Each finding carries `class`, `check`, `severity` (`error` fails the audit,
`warning` never does), `term`, `message`, `fix` (the safe resolution, when
one exists), and `fixed`. `--fix` applies only the mechanical resolutions;
manifest membership, duplicates, and lint errors stay reported as decisions.

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

## Commands (v0.7.0)

| Command | Description | Status |
|---------|-------------|--------|
| `lookup <term>` | Read a node's full definition | Implemented |
| `scope [ring]` | List terms at a ring level | Implemented |
| `lint [path]` | Validate nodes against SPEC.md rules | Implemented |
| `graph [ring]` | Generate term relationship graph (DOT/JSON) | Implemented |
| `toc` | Term index with lay definitions, grouped by ring (Markdown/JSON) | Implemented |
| `sources [term]` | External sources and quoted passages per node; `--lock` writes `audit/sources.lock.json` | Implemented |
| `audit` | Structure, contradiction, source, mirror, and LLM-semantic audit as text or JSON; `--fix` applies safe resolutions; exit 0/1/2 | Implemented (`--structure`, `--contradictions`, `--sources`, `--mirrors`, `--semantic`) |
| `export [ring]` | Export the ontology as SKOS RDF (Turtle/JSON-LD) | Implemented |
| `sparql <query>` | Run a SPARQL query over the exported ontology | Implemented (`--features sparql`) |
| `serve` | SPARQL Protocol endpoint on localhost | Implemented (`--features sparql`) |
| `fetch [source]` | Clone or pull ontology from GitHub | Implemented |
| `install` | Set up ~/.claude integration | Planned |
| `build-site` | Generate static site + JSON API | Planned |
| `context <domain>` | Suggest relevant terms for a domain | Planned |

## License

Apache-2.0
