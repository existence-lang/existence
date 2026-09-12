# existence

CLI tool for the Existence ontology framework. Single Rust binary using clap (derive API).

## Architecture

```
src/
  main.rs          — CLI entry point, clap arg parsing, command dispatch
  config.rs        — existence.toml parsing (serde + toml), ontology dir resolution
  markdown.rs      — Markdown node parsing, section extraction, link extraction
  commands/
    mod.rs         — Command module declarations
    lookup.rs      — Read and display a term definition (raw or JSON)
    scope.rs       — List terms by ring level from existence.toml
    lint.rs        — Validate nodes against SPEC.md rules
    graph.rs       — Generate DOT or JSON relationship graphs
    fetch.rs       — Clone/pull ontology repos via git
```

## Key patterns

- **Ontology resolution**: `config::resolve_ontology_dir()` checks (1) `--ontology` flag, (2) cwd for `existence.toml`, (3) `~/.existence/sources/`
- **Ring keys**: TOML table keys are strings (`[rings.0]`), stored as `BTreeMap<String, Ring>`, accessed via `Config::get_ring(u32)` and `Config::rings_sorted()`
- **Markdown parsing**: Section extraction matches `## [SectionName]` or `## SectionName` headings; link extraction uses regex for `[term](./term.md)` patterns
- **Error handling**: All commands return `Result<(), String>` — main prints errors to stderr and exits with code 1
- **Stub commands**: `install`, `serve`, `build-site`, `context` are defined in the CLI enum but print "not yet implemented" messages — no command files for these yet

## Development

```bash
cargo clippy -- -D warnings  # Lint
cargo test                    # Run tests
cargo fmt -- --check          # Format check
```

## Testing against the real ontology

```bash
# Point at the ontology repo
existence --ontology /path/to/existence-lang/ontology lookup existence
existence --ontology /path/to/existence-lang/ontology lint
existence --ontology /path/to/existence-lang/ontology graph 0
```

<!-- tsift:code-navigation v=0.1.96 -->
## Code Navigation

Run `tsift status` at session start from the owning repo root. If the task or file lives under a git submodule (for example `src/tsift/...`), switch to that submodule root first so the harness loads the narrower local instructions and repo state instead of the superproject root. `tsift status` repairs the `.tsift/` index state it owns and never rewrites tracked files (`--no-fix` skips even that). If status reports stale or missing instructions, run `tsift init` to refresh the tracked Code Navigation block and runbook; it names every tracked file it rewrites or moves. When the harness cannot perform write commands, ask the user to run the printed `run:` command instead.

Prefer tsift envelopes over raw reads:
- `tsift --envelope search <query>` instead of `grep`/`rg`
- `tsift --envelope source-read <file>` / `tsift --envelope symbol-read <symbol>` instead of raw `cat`/`head`/`tail`/`sed`/`less` source reads
- `tsift --envelope explain <symbol>` and `tsift graph <symbol> --callers` / `--callees` for call graphs
- `tsift diff-digest [path]` (`--pathspec <pathspec>` to preserve scoped reviews) instead of `git diff`, commit-form `git show`, or patch-style `git log`; blob-form `git show <rev>:<path>` stays a raw object read
- `tsift --envelope session-review <path>` / `tsift --envelope context-pack <path>` instead of replaying long session docs or transcripts
- raw-read rewrites route recognized session docs/transcripts to `tsift session-digest --input <path>` and captured logs to `tsift log-digest --input <path>`
- `tsift --envelope digest-runner --kind test|log --path . --shell-command '<command>'` instead of raw test/build output

Command detail lives in [`.agent/runbooks/code-navigation.md`](.agent/runbooks/code-navigation.md) — budgets, `tsift workflow search`, `report.scale_guard` handling, the harness rewrite path for `PreToolUse`-less harnesses, and Codex/OpenCode integration. `tsift init` writes and versions that runbook alongside this block, so it is present in every initialized checkout; read it before broad exploration instead of expanding this block. A repository that also ships a current `.claude/skills/tsift/SKILL.md` should use that skill as the deeper source.

For local verification, run `make check` before committing. After local changes, check the latest GitHub Actions CI run with `gh run list --limit 1` and fix any failing tests before calling the work complete.

Only read full source files when tsift results are insufficient.
<!-- /tsift:code-navigation -->
