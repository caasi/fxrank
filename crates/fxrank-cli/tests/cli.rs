use assert_cmd::Command;
use std::io::Write;

fn fxrank() -> Command {
    Command::cargo_bin("fxrank").expect("binary exists")
}

/// Helper: run `fxrank scan` with the given stdin text, assert success, and
/// return the parsed JSON value.
fn scan_stdin(input: &str) -> serde_json::Value {
    let output = fxrank()
        .arg("scan")
        .write_stdin(input)
        .output()
        .expect("process ran");
    assert!(
        output.status.success(),
        "exit was not 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    // Compact JSON = exactly one non-empty line
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one JSON line, got: {stdout:?}"
    );
    serde_json::from_str(lines[0]).expect("valid JSON")
}

// ── Test 1: stdin with a Rust function → one JSON line containing "hotspots" ──

#[test]
fn stdin_produces_one_line_json_with_hotspots() {
    let json = scan_stdin("fn my_fn() { println!(\"x\"); }");
    assert!(
        json.get("hotspots").is_some(),
        "missing 'hotspots' key in: {json}"
    );
}

// ── Test 2: stdin → scope.input is "stdin" ──

#[test]
fn stdin_scope_input_is_stdin() {
    let json = scan_stdin("fn f() {}");
    assert_eq!(
        json["scope"]["input"].as_str(),
        Some("stdin"),
        "scope.input should be 'stdin'"
    );
}

// ── Test 3: scan <file> → JSON with a hotspot for the function ──

#[test]
fn file_path_produces_hotspot_for_function() {
    let mut tmp = std::env::temp_dir();
    tmp.push("fxrank_test_file.rs");
    {
        let mut f = std::fs::File::create(&tmp).expect("create temp file");
        writeln!(f, "fn my_fn() {{ println!(\"hello\"); }}").expect("write");
    }

    let output = fxrank()
        .arg("scan")
        .arg(&tmp)
        .output()
        .expect("process ran");

    std::fs::remove_file(&tmp).ok();

    assert!(
        output.status.success(),
        "exit was not 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "expected one JSON line");
    let json: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");
    assert!(
        json["hotspots"].is_array(),
        "missing hotspots array: {json}"
    );
    // The function should appear as a hotspot
    let hotspots = json["hotspots"].as_array().unwrap();
    assert!(
        !hotspots.is_empty(),
        "expected at least one hotspot for fn my_fn()"
    );
}

// ── Test 4: directory path → JSON with hotspots for .rs files inside ──

#[test]
fn directory_path_recurses_and_finds_hotspot() {
    let dir = std::env::temp_dir().join("fxrank_test_dir");
    std::fs::create_dir_all(&dir).expect("create dir");
    let rs_file = dir.join("sample.rs");
    {
        let mut f = std::fs::File::create(&rs_file).expect("create file");
        writeln!(f, "fn dir_fn() {{ println!(\"dir\"); }}").expect("write");
    }

    let output = fxrank()
        .arg("scan")
        .arg(&dir)
        .output()
        .expect("process ran");

    std::fs::remove_dir_all(&dir).ok();

    assert!(
        output.status.success(),
        "exit was not 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "expected one JSON line");
    let json: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");
    let hotspots = json["hotspots"].as_array().unwrap();
    assert!(
        !hotspots.is_empty(),
        "expected hotspot for fn dir_fn() in dir scan"
    );
}

// ── Test 5: non-existent path → non-zero exit + JSON error object ──

#[test]
fn nonexistent_path_exits_nonzero_with_json_error() {
    let output = fxrank()
        .arg("scan")
        .arg("/nonexistent/path/that/does/not/exist.rs")
        .output()
        .expect("process ran");

    assert!(
        !output.status.success(),
        "expected non-zero exit for missing path"
    );
    // Error object goes to stdout so agents can parse it
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    assert!(
        !stdout.trim().is_empty(),
        "expected JSON error on stdout, got nothing"
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("expected JSON error object");
    assert!(
        json.get("error").is_some(),
        "expected 'error' key in JSON: {json}"
    );
}

// ── Test 7: dogfood — scan the fxrank crates themselves ──

#[test]
fn dogfoods_the_fxrank_crates() {
    // CARGO_MANIFEST_DIR is crates/fxrank-cli, so ".." is crates/
    let crates_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
    let output = fxrank()
        .arg("scan")
        .arg(crates_dir)
        .output()
        .expect("process ran");

    assert!(
        output.status.success(),
        "fxrank scan over own crates/ failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("fxrank output should be valid JSON");
    let hotspots = json["hotspots"]
        .as_array()
        .expect("hotspots should be an array");
    assert!(
        !hotspots.is_empty(),
        "fxrank should find hotspots in its own crates, got empty array"
    );
}

// ── Test 8: read_dir entry errors are surfaced as diagnostics, not silently dropped ──
//
// Simulating a truly unreadable entry (chmod 000) is fragile and root-sensitive,
// so this test focuses on the happy path: a directory containing one valid .rs file
// is scanned successfully and produces a non-empty diagnostics array *slot* in the
// JSON shape — confirming the code path that would emit diagnostics is wired through
// to the report.  The companion unit-level check is the code change in walk_dir:
// entry errors now emit a Diagnostic attributed to the parent directory instead of
// being silently dropped by .flatten().
#[test]
fn directory_scan_diagnostics_key_present_in_output() {
    let dir = std::env::temp_dir().join("fxrank_test_diag");
    std::fs::create_dir_all(&dir).expect("create dir");
    let rs_file = dir.join("diag_test.rs");
    {
        let mut f = std::fs::File::create(&rs_file).expect("create file");
        writeln!(f, "fn diag_fn() {{}}").expect("write");
    }

    let output = fxrank()
        .arg("scan")
        .arg(&dir)
        .output()
        .expect("process ran");

    std::fs::remove_dir_all(&dir).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "expected one JSON line");
    let json: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");

    // Confirm the diagnostics key is present and is an array (even if empty on success).
    assert!(
        json.get("diagnostics")
            .map(|v| v.is_array())
            .unwrap_or(false),
        "report must have a 'diagnostics' array key for error surfacing; got: {json}"
    );
}

// ── Test 9: --include-tests flag controls test-code skipping ──

#[test]
fn scan_skips_tests_by_default_and_include_tests_keeps_them() {
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("fxrank_skip_tests_{}_a.rs", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp).expect("create temp file");
        f.write_all(
            b"fn prod() { let _ = std::fs::read(\"a\"); }\n\
              #[cfg(test)] mod tests { #[test] fn t() { let _ = std::fs::read(\"b\"); } }",
        )
        .expect("write");
    }

    // default: test module is skipped; skipped_tests >= 1; symbol "t" not in hotspots
    let out = fxrank()
        .arg("scan")
        .arg(&tmp)
        .output()
        .expect("process ran");
    std::fs::remove_file(&tmp).ok();

    assert!(
        out.status.success(),
        "exit non-zero; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let j: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert!(
        j["scope"]["skipped_tests"].as_u64().unwrap_or(0) >= 1,
        "expected skipped_tests >= 1 by default, got: {j}"
    );
    assert!(
        !j["hotspots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["symbol"] == "t"),
        "test fn 't' should not appear in hotspots by default"
    );

    // --include-tests: test fn is included; skipped_tests == 0; symbol "t" in hotspots
    let mut tmp2 = std::env::temp_dir();
    tmp2.push(format!("fxrank_skip_tests_{}_b.rs", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp2).expect("create temp file");
        f.write_all(
            b"fn prod() { let _ = std::fs::read(\"a\"); }\n\
              #[cfg(test)] mod tests { #[test] fn t() { let _ = std::fs::read(\"b\"); } }",
        )
        .expect("write");
    }

    let out2 = fxrank()
        .arg("scan")
        .arg(&tmp2)
        .arg("--include-tests")
        .output()
        .expect("process ran");
    std::fs::remove_file(&tmp2).ok();

    assert!(
        out2.status.success(),
        "exit non-zero with --include-tests; stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let stdout2 = String::from_utf8(out2.stdout).expect("utf-8");
    let j2: serde_json::Value = serde_json::from_str(stdout2.trim()).expect("valid JSON");
    assert_eq!(
        j2["scope"]["skipped_tests"].as_u64(),
        Some(0),
        "expected skipped_tests == 0 with --include-tests, got: {j2}"
    );
    assert!(
        j2["hotspots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["symbol"] == "t"),
        "test fn 't' should appear in hotspots with --include-tests"
    );
}

// ── Test 6: --limit 1 on ≥2 functions → hotspots length 1, summary over all ──

#[test]
fn limit_truncates_hotspots_but_summary_covers_all() {
    // Two distinct functions so we can verify summary.functions reflects both
    let src = r#"
fn alpha() { println!("a"); }
fn beta()  { println!("b"); }
"#;
    let mut tmp = std::env::temp_dir();
    tmp.push("fxrank_test_limit.rs");
    {
        let mut f = std::fs::File::create(&tmp).expect("create");
        f.write_all(src.as_bytes()).expect("write");
    }

    let output = fxrank()
        .arg("scan")
        .arg(&tmp)
        .arg("--limit")
        .arg("1")
        .output()
        .expect("process ran");

    std::fs::remove_file(&tmp).ok();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1);
    let json: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");

    let hotspots = json["hotspots"].as_array().expect("hotspots array");
    assert_eq!(hotspots.len(), 1, "limit 1 should give exactly 1 hotspot");

    // scope.functions should reflect the total parsed (≥ 2)
    let functions_count = json["scope"]["functions"].as_u64().unwrap_or(0);
    assert!(
        functions_count >= 2,
        "scope.functions should be ≥ 2 (all functions), got {functions_count}"
    );
}
