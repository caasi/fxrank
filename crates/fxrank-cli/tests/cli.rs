use assert_cmd::Command;
use std::io::Write;
use tempfile::TempDir;

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

// ── Test 10: stdin with --lang ts → TS frontend, one function counted ──

#[test]
fn cli_scans_ts_fragment_from_stdin() {
    use assert_cmd::Command;
    let mut cmd = Command::cargo_bin("fxrank").unwrap();
    let assert = cmd
        .args(["scan", "--lang", "ts", "-"])
        .write_stdin("function f(): void {}\n")
        .assert()
        .success();
    let json: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(json["scope"]["functions"], 1);
}

// ── Test 12 (Task 7): TS async fn with fetch → hotspot with max_class 7 ──

#[test]
fn cli_ts_async_fetch_yields_class7_hotspot() {
    let src = "async function load(): Promise<string> {\n\
               const r = await fetch('https://x');\n\
               console.log('done');\n\
               return r.text();\n\
               }\n";
    let mut cmd = Command::cargo_bin("fxrank").unwrap();
    let assert = cmd
        .args(["scan", "--lang", "ts", "-"])
        .write_stdin(src)
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("valid JSON");
    let hotspots = json["hotspots"].as_array().expect("hotspots array");
    let load = hotspots
        .iter()
        .find(|h| h["symbol"].as_str() == Some("load"))
        .expect("hotspot for 'load' not found");
    assert_eq!(
        load["max_class"].as_u64(),
        Some(7),
        "load() should have max_class 7 (net.fs.db from fetch)"
    );
    assert!(
        load["own_score"].as_f64().unwrap_or(0.0) >= 21.0,
        "own_score should be >= 21.0 (weight_for_class(7) == 21)"
    );
    assert_eq!(
        load["async_boundary"].as_bool(),
        Some(true),
        "load() should be an async boundary"
    );
    let effects = load["effects"].as_array().expect("effects array");
    assert!(
        !effects.is_empty(),
        "load() should have detected effects, got none"
    );
}

// ── Test 11: stdin WITHOUT --lang stays Rust (back-compat) ──

#[test]
fn cli_stdin_without_lang_is_rust() {
    // A Rust fn body parses as Rust; the same text is not valid TS, so if the
    // back-compat default ever flipped to TS this would error or miscount.
    let json = scan_stdin("fn r() { println!(\"x\"); }");
    assert_eq!(
        json["scope"]["functions"], 1,
        "stdin without --lang should parse as Rust"
    );
    assert!(
        json.get("hotspots").is_some(),
        "missing 'hotspots' key in: {json}"
    );
}

// ── Test 13: --lang on a real path → error (only valid for stdin) ──

#[test]
fn lang_flag_on_file_path_is_rejected() {
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("fxrank_lang_on_path_{}.rs", std::process::id()));
    std::fs::write(&tmp, "fn f() {}").expect("write temp file");

    let output = fxrank()
        .args(["scan", "--lang", "ts"])
        .arg(&tmp)
        .output()
        .expect("process ran");

    std::fs::remove_file(&tmp).ok();

    assert!(
        !output.status.success(),
        "expected non-zero exit when --lang is combined with a file path"
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("expected JSON error object on stdout");
    let error_msg = json["error"].as_str().unwrap_or("");
    assert!(
        error_msg.contains("--lang is only valid when reading from stdin"),
        "error message should mention stdin restriction; got: {error_msg:?}"
    );
}

#[test]
fn lang_flag_on_directory_path_is_rejected() {
    let dir = std::env::temp_dir().join(format!("fxrank_lang_on_dir_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create dir");

    let output = fxrank()
        .args(["scan", "--lang", "ts"])
        .arg(&dir)
        .output()
        .expect("process ran");

    std::fs::remove_dir_all(&dir).ok();

    assert!(
        !output.status.success(),
        "expected non-zero exit when --lang is combined with a directory path"
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("expected JSON error object on stdout");
    let error_msg = json["error"].as_str().unwrap_or("");
    assert!(
        error_msg.contains("--lang is only valid when reading from stdin"),
        "error message should mention stdin restriction; got: {error_msg:?}"
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

// ── Task 004: default file-glob excludes + skipped_excluded count ──
#[test]
fn default_excludes_skip_bundles_stories_and_count_them() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("mkdir");
    // one real source file (kept) + three default-excluded files
    std::fs::write(src.join("app.ts"), "export function ok() { return 1; }\n").unwrap();
    std::fs::write(src.join("vendor.min.js"), "function a(){}\n").unwrap();
    std::fs::write(
        src.join("Button.stories.tsx"),
        "export const s = () => 1;\n",
    )
    .unwrap();
    std::fs::write(src.join("jest.setup.js"), "globalThis.x = 1;\n").unwrap();

    let out = fxrank().arg("scan").arg(root).output().expect("ran");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(
        j["scope"]["skipped_excluded"].as_u64(),
        Some(3),
        "three default-excluded files; got: {j}"
    );
    // only app.ts contributed functions
    assert!(j["scope"]["functions"].as_u64().unwrap_or(0) >= 1);
}

// ── Task 004: a wildcard entry must NOT prune a same-named directory ──
#[test]
fn wildcard_default_does_not_prune_matching_directory() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    // directory name matches the default `*.stories.*`
    let d = root.join("x.stories.d");
    std::fs::create_dir_all(&d).expect("mkdir");
    std::fs::write(d.join("keep.ts"), "export function keep() { return 2; }\n").unwrap();

    let out = fxrank().arg("scan").arg(root).output().expect("ran");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    // keep.ts under x.stories.d/ is still scanned (wildcard files-only)
    assert!(
        j["hotspots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["symbol"].as_str() == Some("keep")),
        "x.stories.d/ must not be pruned by the `*.stories.*` default; got: {j}"
    );
    assert_eq!(j["scope"]["skipped_excluded"].as_u64(), Some(0));
}

// ── Task 004: invalid glob → non-zero exit + JSON error ──
#[test]
fn invalid_exclude_glob_is_startup_error() {
    let tmp = TempDir::new().expect("tmp");
    let out = fxrank()
        .arg("scan")
        .arg(tmp.path())
        .arg("--exclude")
        .arg("[")
        .output()
        .expect("ran");
    assert!(!out.status.success(), "expected non-zero exit for bad glob");
    let j: serde_json::Value = serde_json::from_str(String::from_utf8(out.stdout).unwrap().trim())
        .expect("JSON error object");
    assert!(j.get("error").is_some(), "expected error key; got: {j}");
}

// ── Test 14: --exclude flag skips vendor/build dirs ──

/// Build a temp tree:
///   <tmp>/src/app.ts          — has a fetch() call (effect-producing)
///   <tmp>/node_modules/pkg/index.ts — also has an effect
///
/// Default scan must skip node_modules; --exclude src must skip src but include
/// node_modules.
#[test]
fn exclude_skips_default_dirs_and_flag_overrides() {
    let tmp: TempDir = TempDir::new().expect("create temp dir");
    let root = tmp.path();

    // <tmp>/src/app.ts
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    std::fs::write(
        src_dir.join("app.ts"),
        "async function fetchData() { return await fetch('https://example.com'); }\n",
    )
    .expect("write app.ts");

    // <tmp>/node_modules/pkg/index.ts
    let nm_dir = root.join("node_modules").join("pkg");
    std::fs::create_dir_all(&nm_dir).expect("create node_modules dir");
    std::fs::write(
        nm_dir.join("index.ts"),
        "async function nmFetch() { return await fetch('https://cdn.example.com'); }\n",
    )
    .expect("write index.ts");

    // --- Default scan: node_modules is in the default exclude list, src is not ---
    let out_default = fxrank()
        .arg("scan")
        .arg(root)
        .output()
        .expect("process ran");
    assert!(
        out_default.status.success(),
        "default scan failed; stderr: {}",
        String::from_utf8_lossy(&out_default.stderr)
    );
    let stdout_default = String::from_utf8(out_default.stdout).expect("utf-8");
    let json_default: serde_json::Value =
        serde_json::from_str(stdout_default.trim()).expect("valid JSON");
    let fn_count_default = json_default["scope"]["functions"].as_u64().unwrap_or(0);
    // Only src/app.ts is scanned (node_modules excluded by default) → 1 function
    assert_eq!(
        fn_count_default, 1,
        "default scan should find 1 function (node_modules excluded); got {fn_count_default}"
    );

    // --- --exclude src: src dir is now excluded; node_modules is NOT in the list ---
    let out_custom = fxrank()
        .arg("scan")
        .arg(root)
        .arg("--exclude")
        .arg("src")
        .output()
        .expect("process ran");
    assert!(
        out_custom.status.success(),
        "--exclude src scan failed; stderr: {}",
        String::from_utf8_lossy(&out_custom.stderr)
    );
    let stdout_custom = String::from_utf8(out_custom.stdout).expect("utf-8");
    let json_custom: serde_json::Value =
        serde_json::from_str(stdout_custom.trim()).expect("valid JSON");
    let fn_count_custom = json_custom["scope"]["functions"].as_u64().unwrap_or(0);
    // node_modules/pkg/index.ts is scanned; src excluded → 1 function from node_modules
    assert_eq!(
        fn_count_custom, 1,
        "--exclude src should find 1 function (only node_modules scanned); got {fn_count_custom}"
    );
    // The hotspot symbol should be nmFetch (from node_modules), not fetchData (from src)
    let hotspots = json_custom["hotspots"].as_array().expect("hotspots array");
    assert!(
        hotspots
            .iter()
            .any(|h| h["symbol"].as_str() == Some("nmFetch")),
        "expected 'nmFetch' from node_modules in hotspots when src is excluded; got: {json_custom}"
    );
}

// ── Task 004: --exclude replaces the default (not additive) ──
#[test]
fn exclude_replaces_default_so_bundles_reappear() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::write(root.join("a.min.js"), "function a(){ fetch('x'); }\n").unwrap();
    // override with an unrelated pattern → a.min.js is no longer excluded
    let out = fxrank()
        .arg("scan")
        .arg(root)
        .arg("--exclude")
        .arg("*.nope")
        .output()
        .expect("ran");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        j["scope"]["functions"].as_u64().unwrap_or(0) >= 1,
        "a.min.js should be scanned once defaults are replaced; got: {j}"
    );
}

// ── Task 004: --exclude is a no-op for an explicitly named single file ──
#[test]
fn exclude_does_not_apply_to_single_file_target() {
    let tmp = TempDir::new().expect("tmp");
    let f = tmp.path().join("vendor.min.js");
    std::fs::write(&f, "function a(){ fetch('x'); }\n").unwrap();
    // Even though *.min.js is a default exclude, naming the file scans it. The
    // bogus `--exclude '['` ALSO proves the no-op: for a single file the matcher
    // is never built, so the invalid glob can't error (guards against building
    // the matcher at the top of run_scan).
    let out = fxrank()
        .arg("scan")
        .arg(&f)
        .arg("--exclude")
        .arg("[")
        .output()
        .expect("ran");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        j["scope"]["functions"].as_u64().unwrap_or(0) >= 1,
        "explicit single-file target must be honored; got: {j}"
    );
    assert_eq!(j["scope"]["skipped_excluded"].as_u64(), Some(0));
}

// ── Task 004: --include-tests does NOT re-include a *.stories.* file ──
#[test]
fn include_tests_does_not_reinclude_excluded_stories() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::write(root.join("X.stories.tsx"), "export const s = () => 1;\n").unwrap();
    let out = fxrank()
        .arg("scan")
        .arg(root)
        .arg("--include-tests")
        .output()
        .expect("ran");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        j["scope"]["skipped_excluded"].as_u64(),
        Some(1),
        "stories stay excluded under --include-tests (exclude != test mechanism); got: {j}"
    );
}

// ── Task 004: files accounting — excluded files are in neither files nor read_errors ──
#[test]
fn excluded_files_count_in_skipped_not_files() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::write(root.join("app.ts"), "export function ok() { return 1; }\n").unwrap();
    std::fs::write(root.join("vendor.min.js"), "function a(){}\n").unwrap(); // excluded by default
    let out = fxrank().arg("scan").arg(root).output().expect("ran");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // app.ts is the only file read; vendor.min.js is excluded (not counted in files)
    assert_eq!(
        j["scope"]["files"].as_u64(),
        Some(1),
        "files = read files only; got: {j}"
    );
    assert_eq!(j["scope"]["parsed"].as_u64(), Some(1));
    assert_eq!(j["scope"]["skipped_excluded"].as_u64(), Some(1));
}

// ── Task 004 follow-up: mixed entries (glob + literal + path glob); path glob descends & counts ──
#[test]
fn exclude_mixes_glob_literal_and_path_glob() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::write(
        root.join("keep.ts"),
        "export function keep() { return 1; }\n",
    )
    .unwrap();
    std::fs::write(root.join("a.min.js"), "function a(){}\n").unwrap(); // *.min.js (name glob)
    std::fs::write(root.join("vendor.js"), "function v(){}\n").unwrap(); // vendor.js (literal filename)
    let legacy = root.join("legacy");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(
        legacy.join("old.ts"),
        "export function old() { return 2; }\n",
    )
    .unwrap(); // legacy/** (path glob)
    let out = fxrank()
        .arg("scan")
        .arg(root)
        .arg("--exclude")
        .arg("*.min.js,vendor.js,legacy/**")
        .output()
        .expect("ran");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // glob + literal + path-glob each exclude one routable file; legacy/ is descended
    // (path globs are file filters, not prunes) so old.ts is counted, not pruned.
    assert_eq!(
        j["scope"]["skipped_excluded"].as_u64(),
        Some(3),
        "glob + literal + path-glob should each exclude one file; got: {j}"
    );
    assert!(
        j["hotspots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["symbol"].as_str() == Some("keep")),
        "keep.ts should be scanned; got: {j}"
    );
}

// ── Task 021 (Task 3): pyvenv.cfg content-marker prunes arbitrarily-named venv dirs ──

/// A dir containing `pyvenv.cfg` is a venv root regardless of its name.
/// Files inside are NOT walked, NOT counted in `skipped_excluded`.
/// Scanning the venv root *directly* also yields no hotspots.
#[test]
fn pyvenv_cfg_marker_prunes_arbitrarily_named_venv() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    // A normal source file outside the venv (must appear in hotspots)
    std::fs::write(root.join("app.py"), "def real():\n    open('x')\n").unwrap();

    // An arbitrarily-named venv dir (weirdenv/) with pyvenv.cfg inside
    let venv = root.join("weirdenv");
    std::fs::create_dir_all(&venv).unwrap();
    std::fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
    // A routable Python file inside that must NOT appear in output
    std::fs::write(venv.join("hidden.py"), "def shouldnotappear():\n    pass\n").unwrap();

    // --- Scan the parent dir: weirdenv/ must be pruned (not counted in skipped_excluded) ---
    let out = fxrank().arg("scan").arg(root).output().expect("ran");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");

    let hotspots = j["hotspots"].as_array().expect("hotspots array");
    // app.py outside weirdenv/ must be present in hotspots
    assert!(
        hotspots
            .iter()
            .any(|h| h["path"].as_str().is_some_and(|p| p.ends_with("app.py"))),
        "app.py outside weirdenv/ must still be scanned; got: {j}"
    );
    // weirdenv/ and hidden.py inside it must NOT appear in any hotspot path
    assert!(
        hotspots.iter().all(|h| {
            h["path"]
                .as_str()
                .is_none_or(|p| !p.contains("weirdenv") && !p.contains("hidden.py"))
        }),
        "weirdenv/ must be pruned by pyvenv.cfg marker; no venv path must appear; got: {j}"
    );
    // Marker prunes are NOT counted in skipped_excluded
    assert_eq!(
        j["scope"]["skipped_excluded"].as_u64(),
        Some(0),
        "marker prunes must not increment skipped_excluded; got: {j}"
    );

    // --- Scan the venv root directly: no hotspots (the marker check covers the root itself) ---
    let out2 = fxrank().arg("scan").arg(&venv).output().expect("ran");
    assert!(
        out2.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let j2: serde_json::Value = serde_json::from_slice(&out2.stdout).expect("valid JSON");
    assert_eq!(
        j2["scope"]["files"].as_u64(),
        Some(0),
        "scanning weirdenv/ (venv root) directly must walk 0 files; got: {j2}"
    );
    assert_eq!(
        j2["hotspots"].as_array().map(|a| a.len()),
        Some(0),
        "scanning weirdenv/ (venv root) directly must yield no hotspots; got: {j2}"
    );
}

// ── Task 004 follow-up: path glob anchors relative to scan root through nested dirs ──
#[test]
fn path_glob_anchors_through_nested_dirs() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let nested = root.join("pkg").join("ui");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("x.stories.tsx"), "export const s = () => 1;\n").unwrap();
    std::fs::write(
        root.join("keep.ts"),
        "export function keep() { return 1; }\n",
    )
    .unwrap();
    let out = fxrank()
        .arg("scan")
        .arg(root)
        .arg("--exclude")
        .arg("**/*.stories.*")
        .output()
        .expect("ran");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        j["scope"]["skipped_excluded"].as_u64(),
        Some(1),
        "nested pkg/ui/x.stories.tsx matched by **/*.stories.* regardless of depth; got: {j}"
    );
    assert!(
        j["hotspots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["symbol"].as_str() == Some("keep")),
        "keep.ts should be scanned; got: {j}"
    );
}

// ── Task 12: scan --lang python - → hotspot for the function ──

#[test]
fn scans_python_stdin_fragment() {
    let mut cmd = Command::cargo_bin("fxrank").unwrap();
    cmd.args(["scan", "--lang", "python", "-"])
        .write_stdin("def f():\n    pass\n")
        .assert()
        .success()
        // Report has scope/summary/hotspots/diagnostics (model.rs) — NO "language" field
        .stdout(predicates::str::contains("\"hotspots\""))
        .stdout(predicates::str::contains("\"symbol\":\"f\""));
}

// ── Task 12: Python corpus-hygiene defaults (.venv dir prune, *_pb2.py glob) ──
#[test]
fn prunes_python_noise_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Real Python source — must appear in hotspots
    std::fs::write(root.join("app.py"), "def real():\n    open('x')\n").unwrap();

    // .venv dir — pruned by bare-literal rule (never walked, not counted)
    std::fs::create_dir_all(root.join(".venv/lib")).unwrap();
    std::fs::write(
        root.join(".venv/lib/dep.py"),
        "def noise():\n    open('y')\n",
    )
    .unwrap();

    // *_pb2.py glob — excluded by file-glob rule (counted in skipped_excluded)
    std::fs::write(root.join("svc_pb2.py"), "def gen():\n    pass\n").unwrap();

    let out = Command::cargo_bin("fxrank")
        .unwrap()
        .args(["scan"])
        .arg(root)
        .assert()
        .success();
    let json: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();

    // .venv pruned (dir prune, uncounted) → "noise" never appears in output
    assert!(
        !json.to_string().contains("\"noise\""),
        ".venv/lib/dep.py must be pruned; 'noise' must not appear in output; got: {json}"
    );
    // svc_pb2.py excluded by *_pb2.py file glob and counted in skipped_excluded
    assert!(
        json["scope"]["skipped_excluded"].as_u64().unwrap_or(0) >= 1,
        "svc_pb2.py must be counted in skipped_excluded; got: {json}"
    );
    // app.py's real function is still scanned
    assert!(
        json.to_string().contains("\"real\""),
        "app.py must be scanned and 'real' must appear in hotspots; got: {json}"
    );
}

// ── Task 3 (Plan 5): --project/-p flag wires tsconfig paths into the TS frontend ──

/// With `--project <dir>`, an `@/`-imported callee's effect is inherited by the caller.
/// Without `--project`, the same `@/` import is opaque (FirstPartyOutOfScope reach).
#[test]
#[cfg(feature = "ts")]
fn project_flag_resolves_ts_alias_import() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    // src/helper.ts — does fetch() (net.fs.db, class 7)
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::write(
        src.join("helper.ts"),
        "export function helper() { fetch('x'); }\n",
    )
    .expect("write helper.ts");

    // src/caller.ts — imports helper via @/ alias
    std::fs::write(
        src.join("caller.ts"),
        "import { helper } from '@/helper';\nexport function caller() { helper(); }\n",
    )
    .expect("write caller.ts");

    // tsconfig.json at root: @/* → ./src/*
    std::fs::write(
        root.join("tsconfig.json"),
        r#"{"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
    )
    .expect("write tsconfig.json");

    // --- WITH --project: alias resolves → caller inherits helper's class-7 IO ---
    let out_with = fxrank()
        .arg("scan")
        .arg(root)
        .arg("--project")
        .arg(root)
        .output()
        .expect("ran");
    assert!(
        out_with.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out_with.stderr)
    );
    let j_with: serde_json::Value = serde_json::from_slice(&out_with.stdout).expect("valid JSON");

    let caller_with = j_with["hotspots"]
        .as_array()
        .expect("hotspots array")
        .iter()
        .find(|h| h["symbol"].as_str() == Some("caller"))
        .expect("caller hotspot present")
        .clone();

    // With --project, caller should inherit helper's class-7 IO via propagation.
    // At minimum: no FirstPartyOutOfScope reach for @/helper (it resolved).
    let reaches_with = caller_with["external_reaches"].as_array();
    let opaque_alias = reaches_with.map(|rs| {
        rs.iter().any(|r| {
            r["specifier"]
                .as_str()
                .map(|s| s.starts_with('@'))
                .unwrap_or(false)
        })
    });
    assert!(
        !opaque_alias.unwrap_or(false),
        "with --project, @/helper must NOT appear as an opaque external reach; \
         caller hotspot: {caller_with}"
    );
    // And caller.propagated_max_class should reflect helper's class 7.
    assert_eq!(
        caller_with["propagated_max_class"].as_u64(),
        Some(7),
        "with --project, caller must inherit helper's class-7 IO via propagation; \
         caller hotspot: {caller_with}"
    );

    // --- WITHOUT --project: alias stays opaque → caller does NOT inherit class-7 IO ---
    let out_no = fxrank().arg("scan").arg(root).output().expect("ran");
    assert!(
        out_no.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out_no.stderr)
    );
    let j_no: serde_json::Value = serde_json::from_slice(&out_no.stdout).expect("valid JSON");

    let caller_no = j_no["hotspots"]
        .as_array()
        .expect("hotspots array")
        .iter()
        .find(|h| h["symbol"].as_str() == Some("caller"))
        .expect("caller hotspot present")
        .clone();

    // Without --project, @/helper stays unresolved → propagated_max_class stays at own (< 7).
    assert!(
        caller_no["propagated_max_class"].as_u64().unwrap_or(99) < 7,
        "without --project, caller must NOT inherit helper's class-7 IO \
         (alias unresolved); caller hotspot: {caller_no}"
    );
}

/// --project accepts a file path (tsconfig.json) directly, not just a directory.
#[test]
#[cfg(feature = "ts")]
fn project_flag_accepts_tsconfig_file_path() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::write(
        src.join("helper.ts"),
        "export function helper() { fetch('x'); }\n",
    )
    .expect("write helper.ts");
    std::fs::write(
        src.join("caller.ts"),
        "import { helper } from '@/helper';\nexport function caller() { helper(); }\n",
    )
    .expect("write caller.ts");

    let tsconfig_path = root.join("tsconfig.json");
    std::fs::write(
        &tsconfig_path,
        r#"{"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
    )
    .expect("write tsconfig.json");

    // Pass the FILE path to --project (not just the dir)
    let out = fxrank()
        .arg("scan")
        .arg(root)
        .arg("-p") // short form
        .arg(&tsconfig_path)
        .output()
        .expect("ran");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let caller = j["hotspots"]
        .as_array()
        .expect("hotspots array")
        .iter()
        .find(|h| h["symbol"].as_str() == Some("caller"))
        .expect("caller hotspot present")
        .clone();
    assert_eq!(
        caller["propagated_max_class"].as_u64(),
        Some(7),
        "--project with explicit tsconfig.json file path must resolve aliases; \
         caller hotspot: {caller}"
    );
}

/// A bad tsconfig path → load error surfaces as a diagnostic (NOT a panic/exit failure).
/// Also verifies Bug 2 fix: scope.parsed == scope.files (the tsconfig error must NOT
/// decrement the parsed-source count; it is a config error, not a source parse failure).
#[test]
fn project_flag_bad_path_is_diagnostic_not_failure() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    // A real TS file so there's something to scan
    std::fs::write(root.join("app.ts"), "export function app() { return 1; }\n")
        .expect("write app.ts");

    let out = fxrank()
        .arg("scan")
        .arg(root)
        .arg("--project")
        .arg("/nonexistent/tsconfig.json")
        .output()
        .expect("ran");
    // Must not crash — exit 0 with a diagnostic surfaced in the JSON.
    assert!(
        out.status.success(),
        "bad --project path must not cause non-zero exit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    // A diagnostic (parsed=false) must be present mentioning the failed tsconfig load.
    let diags = j["diagnostics"].as_array().expect("diagnostics array");
    assert!(
        !diags.is_empty(),
        "bad --project must produce a diagnostic; got: {j}"
    );
    // Bug 2 fix: the tsconfig load error must NOT reduce scope.parsed.
    // scope.parsed counts only scanned-source parse failures, never config-file errors.
    let files = j["scope"]["files"].as_u64().expect("scope.files");
    let parsed = j["scope"]["parsed"].as_u64().expect("scope.parsed");
    assert_eq!(
        parsed, files,
        "scope.parsed must equal scope.files — the tsconfig error must not decrement \
         the parsed-source count; files={files} parsed={parsed}; got: {j}"
    );
}

/// Bug 1 fix: --project on a Python-only (or Rust-only) scan must NOT emit a
/// tsconfig diagnostic even when the path is invalid.  The flag is documented
/// TS/JS-only; tsconfig loading must be skipped when there are no TS sources.
#[test]
fn project_flag_no_ts_sources_emits_no_tsconfig_diagnostic() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    // Only a Python file — no TS sources
    std::fs::write(root.join("app.py"), "def f():\n    pass\n").expect("write app.py");

    let out = fxrank()
        .arg("scan")
        .arg(root)
        .arg("--project")
        .arg("/nonexistent/tsconfig.json")
        .output()
        .expect("ran");
    assert!(
        out.status.success(),
        "Python-only scan with bad --project must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    // No tsconfig diagnostic must appear — there are no TS files, so the flag is a no-op.
    let diags = j["diagnostics"].as_array().expect("diagnostics array");
    assert!(
        diags.is_empty(),
        "Python-only scan must NOT emit a tsconfig diagnostic when --project is given \
         but no TS files are present; got diagnostics: {diags:?}"
    );
}
