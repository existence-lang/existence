use crate::config::{self, Config};
use std::path::Path;
use std::process::Command;

/// Clone or pull an ontology from a GitHub repo.
///
/// Source format: `github:org/repo`
/// Stores in `~/.existence/sources/{org}/{repo}/`
///
/// If no source given, reads from `existence.toml` `[sources]` section.
pub fn run(ontology_dir: &Path, source: Option<&str>) -> Result<(), String> {
    let sources = match source {
        Some(s) => vec![("cli".to_string(), s.to_string())],
        None => {
            let config_path = ontology_dir.join("existence.toml");
            if config_path.exists() {
                let config = Config::load(&config_path)?;
                if config.sources.is_empty() {
                    return Err(
                        "No source specified and no [sources] section in existence.toml"
                            .to_string(),
                    );
                }
                config.sources.into_iter().collect()
            } else {
                return Err(
                    "No source specified and no existence.toml found. Usage: existence fetch github:org/repo"
                        .to_string(),
                );
            }
        }
    };

    for (name, source_spec) in &sources {
        fetch_source(name, source_spec)?;
    }

    Ok(())
}

fn fetch_source(name: &str, source: &str) -> Result<(), String> {
    let (org, repo) = parse_github_source(source)?;
    let url = format!("https://github.com/{org}/{repo}.git");

    let home = config::home_dir()?;
    let dest = home
        .join(".existence")
        .join("sources")
        .join(&org)
        .join(&repo);

    if dest.exists() && dest.join(".git").is_dir() {
        // Pull
        println!("[{name}] Updating {org}/{repo}...");
        let output = Command::new("git")
            .args(["pull", "--ff-only"])
            .current_dir(&dest)
            .output()
            .map_err(|e| format!("Failed to run git pull: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git pull failed for {org}/{repo}: {stderr}"));
        }
        println!("{}", String::from_utf8_lossy(&output.stdout).trim());
    } else {
        // Clone
        println!("[{name}] Cloning {org}/{repo}...");

        // Ensure parent dir exists
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
        }

        let output = Command::new("git")
            .args(["clone", &url, &dest.to_string_lossy()])
            .output()
            .map_err(|e| format!("Failed to run git clone: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git clone failed for {org}/{repo}: {stderr}"));
        }
    }

    println!("[{name}] Source ready at {}", dest.display());
    Ok(())
}

/// Parse `github:org/repo` into `(org, repo)`.
fn parse_github_source(source: &str) -> Result<(String, String), String> {
    let rest = source.strip_prefix("github:").ok_or(format!(
        "Invalid source format '{source}'. Expected: github:org/repo"
    ))?;

    let parts: Vec<&str> = rest.splitn(2, '/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(format!(
            "Invalid source format '{source}'. Expected: github:org/repo"
        ));
    }

    Ok((parts[0].to_string(), parts[1].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_github_source() {
        let (org, repo) = parse_github_source("github:existential-kernel/ontology").unwrap();
        assert_eq!(org, "existential-kernel");
        assert_eq!(repo, "ontology");
    }

    #[test]
    fn test_parse_github_source_invalid() {
        assert!(parse_github_source("gitlab:foo/bar").is_err());
        assert!(parse_github_source("github:").is_err());
        assert!(parse_github_source("github:/repo").is_err());
        assert!(parse_github_source("github:org/").is_err());
    }
}
