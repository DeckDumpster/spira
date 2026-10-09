use spira_sim::jq::jq_r;
use std::io::Write;
use std::process::{Command, Stdio};

const DOC: &str = r#"{"number":7,"state":"MERGED","headRefOid":"abc","mergeable":null,"statusCheckRollup":[{"conclusion":"SUCCESS"},{"conclusion":null},{"conclusion":"FAILURE"}],
"check_runs":[{"name":"gate","conclusion":"success"},{"name":"gate","conclusion":"failure"},{"name":"other","conclusion":"success"}],
"workflow_runs":[{"name":"Publish","id":3},{"name":"Gate","id":4}],"artifacts":[{"name":"release-x"},{"name":"logs"}],"flag":true}"#;

const ARR: &str = r#"[{"databaseId":9,"headSha":"s9","head":{"ref":"spira/publish/x"}},{"databaseId":8,"headSha":"s8","head":{"ref":"b"}}]"#;

fn table() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (DOC, ".number", "7\n"),
        (DOC, ".state", "MERGED\n"),
        (DOC, ".flag", "true\n"),
        (DOC, ".mergeable", "null\n"),
        (DOC, ".missing // \"\"", "\n"),
        (DOC, ".state | length", "6\n"),
        (DOC, ".workflow_runs | length", "2\n"),
        (DOC, "select(.state==\"MERGED\") | .headRefOid", "abc\n"),
        (DOC, "select(.state==\"OPEN\") | .headRefOid", ""),
        (DOC, "[.check_runs[] | select(.name == \"gate\" and .conclusion == \"success\")] | length", "1\n"),
        (DOC, ".workflow_runs[] | select(.name == \"Publish\") | .id", "3\n"),
        (DOC, ".artifacts[] | select(.name | startswith(\"release\")) | .name", "release-x\n"),
        (DOC, "[(.mergeable // \"\"), ((.statusCheckRollup // []) | map(.conclusion // \"\") | join(\",\"))] | join(\"\\t\")", "\tSUCCESS,,FAILURE\n"),
        (DOC, ".state == \"MERGED\" or .flag == false", "true\n"),
        (DOC, ".workflow_runs[].name", "Publish\nGate\n"),
        (ARR, ".[0].databaseId", "9\n"),
        (ARR, ".[0].headSha", "s9\n"),
        (ARR, ".[-1].headSha", "s8\n"),
        (ARR, ".[5].number // \"\"", "\n"),
        (ARR, ".[].databaseId", "9\n8\n"),
        (ARR, ".[0].head.ref // \"\"", "spira/publish/x\n"),
        (ARR, "length", "2\n"),
        ("[]", ".[0].headSha", "null\n"),
    ]
}

#[test]
fn the_expressions_this_tree_calls_gh_with_evaluate() {
    for (doc, expr, want) in table() {
        assert_eq!(jq_r(doc, expr).as_deref(), Ok(want), "{expr}");
    }
}

#[test]
fn it_agrees_with_the_real_jq_where_one_is_installed() {
    let mut checked = 0;
    for (doc, expr, _) in table() {
        let Ok(mut child) = Command::new("jq").args(["-r", expr]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { return };
        child.stdin.take().unwrap().write_all(doc.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), jq_r(doc, expr).unwrap(), "{expr}");
        checked += 1;
    }
    assert_eq!(checked, table().len());
}

#[test]
fn an_unsupported_construct_is_an_error_not_an_empty_answer() {
    for expr in [".a | frobnicate", ".a +", "$x", ".[] as $y | $y"] {
        assert!(jq_r("{}", expr).is_err(), "{expr}");
    }
    assert!(jq_r("not json", ".a").is_err());
}
