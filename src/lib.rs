pub mod config;
pub mod markdown;

/// Command implementations exposed as library functions.
///
/// Note: `completions` is excluded from the library as it depends on
/// the CLI `Cli` struct defined in main.rs.
pub mod commands {
    pub mod fetch;
    pub mod graph;
    pub mod lint;
    pub mod lookup;
    pub mod new;
    pub mod scope;
    pub mod search;
}
