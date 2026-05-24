//! JUnit-XML emitter so GitHub Actions can show per-test results in
//! the Checks tab without a custom action (SPEC-01 F9 hand-off).

use std::fmt::Write;

use crate::outcome::{Report, Status};

pub fn to_junit_xml(report: &Report) -> String {
    let total = report.outcomes.len();
    let failures = report.failed();
    let skipped = report.skipped();
    let mut out = String::new();
    let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        out,
        r#"<testsuite name="horndb-harness" tests="{total}" failures="{failures}" skipped="{skipped}">"#,
    );
    for o in &report.outcomes {
        let escaped_id = xml_escape(&o.test_id);
        let escaped_suite = xml_escape(&o.suite);
        match o.status {
            Status::Passed => {
                let _ = writeln!(
                    out,
                    r#"  <testcase classname="{escaped_suite}" name="{escaped_id}" time="{:.3}"/>"#,
                    o.duration_ms as f64 / 1000.0,
                );
            }
            Status::Failed => {
                let msg = xml_escape(o.reason.as_deref().unwrap_or("failed"));
                let _ = writeln!(
                    out,
                    r#"  <testcase classname="{escaped_suite}" name="{escaped_id}" time="{:.3}">"#,
                    o.duration_ms as f64 / 1000.0,
                );
                let _ = writeln!(out, r#"    <failure message="{msg}"/>"#);
                let _ = writeln!(out, "  </testcase>");
            }
            Status::Skipped => {
                let msg = xml_escape(o.reason.as_deref().unwrap_or("skipped"));
                let _ = writeln!(
                    out,
                    r#"  <testcase classname="{escaped_suite}" name="{escaped_id}">"#,
                );
                let _ = writeln!(out, r#"    <skipped message="{msg}"/>"#);
                let _ = writeln!(out, "  </testcase>");
            }
        }
    }
    let _ = writeln!(out, "</testsuite>");
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::{Outcome, Status};

    #[test]
    fn emits_well_formed_junit_for_mixed_report() {
        let mut r = Report::new();
        r.push(Outcome {
            test_id: "a".into(),
            suite: "owl2".into(),
            status: Status::Passed,
            reason: None,
            duration_ms: 12,
        });
        r.push(Outcome {
            test_id: "b<x>".into(),
            suite: "owl2".into(),
            status: Status::Failed,
            reason: Some("not entailed".into()),
            duration_ms: 5,
        });
        r.push(Outcome {
            test_id: "c".into(),
            suite: "sparql11".into(),
            status: Status::Skipped,
            reason: Some("waived".into()),
            duration_ms: 0,
        });
        let xml = to_junit_xml(&r);
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains(r#"tests="3""#));
        assert!(xml.contains(r#"failures="1""#));
        assert!(xml.contains(r#"skipped="1""#));
        assert!(xml.contains("b&lt;x&gt;"));
        assert!(xml.contains("<failure message=\"not entailed\""));
        assert!(xml.contains("<skipped message=\"waived\""));
        assert!(xml.ends_with("</testsuite>\n"));
    }

    #[test]
    fn xml_escape_handles_metas() {
        assert_eq!(xml_escape("a&<b>\"'"), "a&amp;&lt;b&gt;&quot;&apos;");
    }
}
