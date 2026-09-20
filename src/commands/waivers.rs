//! `[[audit.waiver]]` — findings a person reviewed and decided to live with.
//!
//! Some findings are not defects waiting on work but decisions already made. A
//! citation whose page is gone from the live web, from the Wayback Machine and
//! from every mirror is not going to become reachable; whether to drop the line
//! or keep it and accept the standing warning is the author's call, and once it
//! is made the audit has nothing left to tell anyone. With nowhere to record
//! it, the decision lives only in whatever conversation produced it while the
//! weekly report re-raises the same warning forever — which trains the reader
//! to skim the class on the one week it says something new.
//!
//! This is `[[audit.keep_unlinked]]` one level up. That one accepts a specific
//! unlinked mention; this accepts any finding any class raises, and records the
//! same thing: which finding, why, and — because a decision about somebody
//! else's website is a claim about a world that keeps moving — when.
//!
//! Two things a waiver deliberately does NOT do. It does not hide the finding:
//! an accepted finding still prints, because a decision the reader cannot see
//! is indistinguishable from a check that quietly stopped running. And it does
//! not change what `--fix` applies: a waiver is a decision about reporting, not
//! about the ontology's content, so a safe fix available before the waiver is
//! still available after it.
//!
//! A waiver that matches nothing is reported as `stale_waiver`, exactly as an
//! unused keep is reported as `stale_keep`, and for the same reason: a list
//! nobody prunes is a way to silence the check rather than a record of
//! decisions.

use crate::commands::audit::Finding;
use crate::config::Waiver;

/// The severity a waived finding carries: neither error nor warning, and
/// counted as neither.
pub const ACCEPTED: &str = "accepted";

/// Whether `waiver` is about `finding`.
fn matches(waiver: &Waiver, finding: &Finding) -> bool {
    if finding.term != waiver.term || finding.check != waiver.check {
        return false;
    }
    match &waiver.source {
        Some(source) => finding.message.contains(source.as_str()),
        None => true,
    }
}

/// How the acceptance reads in the report.
fn note(waiver: &Waiver) -> String {
    format!(" — accepted {}: {}", waiver.decided_on, waiver.reason)
}

/// How an unused waiver names itself.
fn describe(waiver: &Waiver) -> String {
    match &waiver.source {
        Some(source) => format!("{} / {} ({source})", waiver.term, waiver.check),
        None => format!("{} / {}", waiver.term, waiver.check),
    }
}

/// Demote every finding a waiver accepts; return the `stale_waiver` findings
/// for the waivers that matched nothing.
///
/// Applied once over the whole report rather than inside one class: "the author
/// has decided to live with this" is the same decision whichever check raised
/// it.
pub fn apply(findings: &mut [Finding], waivers: &[Waiver]) -> Vec<Finding> {
    let mut used = vec![false; waivers.len()];
    for finding in findings.iter_mut() {
        // The FIRST matching waiver wins, and the rest are left to report
        // themselves as stale. Applying every match instead would stack their
        // notes onto one message and make two overlapping decisions read as
        // one; a second waiver for a finding already accepted is a duplicate,
        // and the honest thing to do with a duplicate is name it.
        if let Some((i, waiver)) = waivers
            .iter()
            .enumerate()
            .find(|(_, w)| matches(w, finding))
        {
            used[i] = true;
            finding.severity = ACCEPTED.to_string();
            finding.message.push_str(&note(waiver));
        }
    }
    waivers
        .iter()
        .zip(used)
        .filter(|(_, used)| !used)
        .map(|(w, _)| Finding {
            // Not a class of check but a finding about the audit's own record
            // of decisions, the way `stale_keep` is about the keep list.
            class: "audit".to_string(),
            check: "stale_waiver".to_string(),
            severity: "warning".to_string(),
            term: w.term.clone(),
            message: format!(
                "audit.waiver accepts {} ({}), but nothing raised it this run — it is either resolved or stale, so drop the entry",
                describe(w),
                w.reason
            ),
            fix: None,
            fixed: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(term: &str, check: &str, severity: &str, message: &str) -> Finding {
        Finding {
            class: "sources".to_string(),
            check: check.to_string(),
            severity: severity.to_string(),
            term: term.to_string(),
            message: message.to_string(),
            fix: None,
            fixed: false,
        }
    }

    fn waiver(term: &str, check: &str, source: Option<&str>) -> Waiver {
        Waiver {
            term: term.to_string(),
            check: check.to_string(),
            source: source.map(str::to_string),
            reason: "the page is unrecoverable".to_string(),
            decided_on: "2026-09-20".to_string(),
        }
    }

    #[test]
    fn a_matching_waiver_demotes_the_finding_and_carries_the_reason() {
        let mut findings = vec![finding(
            "god",
            "unreachable_host",
            "warning",
            "host theunboundedspirit.com is unreachable (too many redirects)",
        )];
        let stale = apply(&mut findings, &[waiver("god", "unreachable_host", None)]);
        assert_eq!(findings[0].severity, ACCEPTED);
        assert!(findings[0].message.contains("accepted 2026-09-20"));
        assert!(findings[0].message.contains("the page is unrecoverable"));
        // Still present: a decision the reader cannot see reads like a check
        // that stopped running.
        assert!(
            findings[0]
                .message
                .starts_with("host theunboundedspirit.com")
        );
        assert!(stale.is_empty());
    }

    #[test]
    fn a_waiver_for_another_term_or_check_leaves_the_finding_alone() {
        let original = finding(
            "god",
            "unreachable_host",
            "warning",
            "host x is unreachable",
        );
        for w in [
            waiver("mind", "unreachable_host", None),
            waiver("god", "dead_link", None),
        ] {
            let mut findings = vec![original.clone()];
            let stale = apply(&mut findings, &[w]);
            assert_eq!(findings[0], original);
            assert_eq!(stale.len(), 1, "a waiver matching nothing is reported");
        }
    }

    #[test]
    fn source_narrows_a_waiver_to_one_of_a_terms_citations() {
        let mut findings = vec![
            finding(
                "god",
                "dead_link",
                "error",
                "source http://gone.example/a returned HTTP 404",
            ),
            finding(
                "god",
                "dead_link",
                "error",
                "source http://other.example/b returned HTTP 404",
            ),
        ];
        let stale = apply(
            &mut findings,
            &[waiver("god", "dead_link", Some("gone.example"))],
        );
        assert_eq!(findings[0].severity, ACCEPTED);
        assert_eq!(
            findings[1].severity, "error",
            "the other citation is untouched"
        );
        assert!(stale.is_empty());
    }

    #[test]
    fn an_unused_waiver_is_reported_rather_than_silently_doing_nothing() {
        let mut findings = vec![finding(
            "god",
            "dead_link",
            "error",
            "source x returned HTTP 404",
        )];
        let stale = apply(&mut findings, &[waiver("god", "unreachable_host", None)]);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].check, "stale_waiver");
        assert_eq!(stale[0].severity, "warning");
        assert_eq!(stale[0].term, "god");
        assert!(stale[0].message.contains("god / unreachable_host"));
        // The reason travels with the stale report so the reader can judge
        // whether to drop the entry without opening existence.toml.
        assert!(stale[0].message.contains("the page is unrecoverable"));
    }

    #[test]
    fn an_unused_waiver_with_a_source_names_it() {
        let mut findings = Vec::new();
        let stale = apply(
            &mut findings,
            &[waiver("god", "dead_link", Some("gone.example"))],
        );
        assert!(stale[0].message.contains("god / dead_link (gone.example)"));
    }

    #[test]
    fn two_waivers_matching_one_finding_do_not_stack_their_notes() {
        let mut findings = vec![finding(
            "god",
            "dead_link",
            "error",
            "source x returned HTTP 404",
        )];
        let stale = apply(
            &mut findings,
            &[
                waiver("god", "dead_link", None),
                waiver("god", "dead_link", None),
            ],
        );
        assert_eq!(findings[0].message.matches("accepted").count(), 1);
        // The second never applied, so it reports itself rather than passing
        // for a live decision.
        assert_eq!(stale.len(), 1);
    }
}
