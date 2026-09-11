mod commands;
mod config;
mod markdown;

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;
use std::process;

#[derive(Parser)]
#[command(
    name = "existence",
    version,
    about = "CLI for the Existence ontology framework"
)]
pub struct Cli {
    /// Path to the ontology directory (overrides auto-detection)
    #[arg(long, global = true)]
    ontology: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Read a node's full definition
    Lookup {
        /// Term name (e.g. "existence", "entity")
        term: String,

        /// Output parsed sections as JSON
        #[arg(long)]
        json: bool,
    },

    /// Search across term files by relevance
    Search {
        /// Search query (matches term names, definitions, and content)
        query: String,

        /// Output results as JSON
        #[arg(long)]
        json: bool,

        /// Maximum number of results (default 10)
        #[arg(long, default_value = "10")]
        limit: usize,
    },

    /// Navigate the scoping chain — list terms at a ring level
    Scope {
        /// Ring level (0, 1, ...). Omit to list all rings.
        ring: Option<u32>,
    },

    /// Validate ontology nodes against SPEC.md rules
    Lint {
        /// Path to lint (directory or single file). Defaults to src/
        path: Option<String>,
    },

    /// Generate term relationship graph
    Graph {
        /// Filter to a specific ring level
        ring: Option<u32>,

        /// Output format: "dot" (default) or "json"
        #[arg(long, default_value = "dot")]
        format: String,
    },

    /// Export the ontology as RDF (SKOS) in Turtle or JSON-LD
    Export {
        /// Filter to a specific ring level
        ring: Option<u32>,

        /// Output format: "turtle" (default) or "jsonld"
        #[arg(long, default_value = "turtle")]
        format: String,

        /// Base IRI for term identity (overrides `meta.base_iri` in existence.toml)
        #[arg(long)]
        base_iri: Option<String>,
    },

    /// Emit a term index with lay definitions, grouped by ring
    Toc {
        /// Output format: "markdown" (default) or "json"
        #[arg(long, default_value = "markdown")]
        format: String,

        /// Filter to a specific ring level
        #[arg(long)]
        ring: Option<u32>,

        /// Path prefix for term links (rewrites `./term.md` to `<base>/term.md`).
        /// Defaults to the path from the output file's directory to src/, or
        /// `src` when printing to stdout
        #[arg(long)]
        base: Option<String>,

        /// Write to this file instead of stdout
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Emit nothing; fail when a node's Ontology section does not open
        /// with a plain sentence, a ring lists a term with no node file, or
        /// a node is in no ring
        #[arg(long)]
        check: bool,
    },

    /// List the external sources each node cites and the passages it quotes,
    /// or derive the audit lockfile from them
    Sources {
        /// Restrict the listing to one term
        term: Option<String>,

        /// Emit JSON instead of text
        #[arg(long)]
        json: bool,

        /// Write the lockfile (default `audit/sources.lock.json`, relative to
        /// the ontology) instead of listing; keeps fetch results already
        /// recorded for URLs and terms that still cite
        #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = commands::sources::DEFAULT_LOCK)]
        lock: Option<PathBuf>,
    },

    /// Run a SPARQL query over the exported ontology (needs the `sparql` feature)
    Sparql {
        /// The query text, or `-` to read it from stdin. skos:, rdfs:, dcterms:,
        /// xsd:, xl:, and `:` (the base IRI) are pre-declared
        query: String,

        /// SELECT/ASK result format: text (default), json, csv, tsv, or xml.
        /// CONSTRUCT/DESCRIBE always return Turtle
        #[arg(long, default_value = "text")]
        format: String,

        /// Base IRI for term identity (overrides `meta.base_iri` in existence.toml)
        #[arg(long)]
        base_iri: Option<String>,
    },

    /// Clone or pull an ontology from a GitHub repo
    Fetch {
        /// Source in format github:org/repo. If omitted, reads existence.toml
        source: Option<String>,
    },

    /// Create a new ontology node from template
    New {
        /// Term name (lowercase, hyphens for multi-word, e.g. "domain-model")
        term: String,

        /// Add to ring N in existence.toml (0, 1, 2)
        #[arg(long)]
        ring: Option<u32>,

        /// Don't open in $EDITOR after creating
        #[arg(long)]
        no_edit: bool,

        /// Pre-fill the ontology section's first line
        #[arg(long)]
        description: Option<String>,
    },

    /// Generate shell completion scripts
    Completions {
        /// Shell to generate completions for (bash, zsh, fish, elvish, powershell)
        shell: Shell,
    },

    /// Set up ~/.claude integration (not yet implemented)
    Install,

    /// Serve a SPARQL endpoint at http://127.0.0.1:<port>/sparql (needs the `sparql` feature)
    Serve {
        /// TCP port to listen on
        #[arg(long, default_value = "3030")]
        port: u16,

        /// Base IRI for term identity (overrides `meta.base_iri` in existence.toml)
        #[arg(long)]
        base_iri: Option<String>,
    },

    /// Generate static site + JSON API (not yet implemented)
    BuildSite,

    /// Suggest relevant terms for a domain (not yet implemented)
    Context {
        /// Domain name
        domain: String,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Lookup { ref term, json } => {
            let ontology_dir = resolve_or_exit(cli.ontology.as_deref());
            commands::lookup::run(&ontology_dir, term, json)
        }
        Commands::Search {
            ref query,
            json,
            limit,
        } => {
            let ontology_dir = resolve_or_exit(cli.ontology.as_deref());
            commands::search::run(&ontology_dir, query, json, limit)
        }
        Commands::Scope { ring } => {
            let ontology_dir = resolve_or_exit(cli.ontology.as_deref());
            commands::scope::run(&ontology_dir, ring)
        }
        Commands::Lint { ref path } => {
            let ontology_dir = resolve_or_exit(cli.ontology.as_deref());
            commands::lint::run(&ontology_dir, path.as_deref())
        }
        Commands::Graph { ring, ref format } => {
            let ontology_dir = resolve_or_exit(cli.ontology.as_deref());
            commands::graph::run(&ontology_dir, ring, format)
        }
        Commands::Export {
            ring,
            ref format,
            ref base_iri,
        } => {
            let ontology_dir = resolve_or_exit(cli.ontology.as_deref());
            commands::export::run(&ontology_dir, ring, format, base_iri.as_deref())
        }
        Commands::Toc {
            ref format,
            ring,
            ref base,
            ref output,
            check,
        } => {
            let ontology_dir = resolve_or_exit(cli.ontology.as_deref());
            commands::toc::run(
                &ontology_dir,
                ring,
                format,
                base.as_deref(),
                output.as_deref(),
                check,
            )
        }
        Commands::New {
            ref term,
            ring,
            no_edit,
            ref description,
        } => {
            let ontology_dir = resolve_or_exit(cli.ontology.as_deref());
            commands::new::run(&ontology_dir, term, ring, no_edit, description.as_deref())
        }
        Commands::Sources {
            ref term,
            json,
            ref lock,
        } => {
            let ontology_dir = resolve_or_exit(cli.ontology.as_deref());
            commands::sources::run(&ontology_dir, term.as_deref(), json, lock.as_deref())
        }
        Commands::Sparql {
            ref query,
            ref format,
            ref base_iri,
        } => {
            let ontology_dir = resolve_or_exit(cli.ontology.as_deref());
            commands::sparql::run(&ontology_dir, query, format, base_iri.as_deref())
        }
        Commands::Fetch { ref source } => {
            let ontology_dir = config::resolve_ontology_dir(cli.ontology.as_deref())
                .unwrap_or_else(|_| PathBuf::from("."));
            commands::fetch::run(&ontology_dir, source.as_deref())
        }
        Commands::Completions { shell } => {
            commands::completions::run(shell);
            Ok(())
        }
        Commands::Install => {
            println!("existence install: not yet implemented");
            println!("Will set up ~/.claude integration with ontology terms.");
            Ok(())
        }
        Commands::Serve { port, ref base_iri } => {
            let ontology_dir = resolve_or_exit(cli.ontology.as_deref());
            commands::sparql::serve(&ontology_dir, port, base_iri.as_deref())
        }
        Commands::BuildSite => {
            println!("existence build-site: not yet implemented");
            println!("Will generate a static site + JSON API from the ontology.");
            Ok(())
        }
        Commands::Context { ref domain } => {
            println!("existence context: not yet implemented");
            println!("Will suggest relevant ontology terms for the '{domain}' domain.");
            Ok(())
        }
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

fn resolve_or_exit(explicit: Option<&std::path::Path>) -> PathBuf {
    config::resolve_ontology_dir(explicit).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(1);
    })
}
