# Spec 029 — Shell frontend dogfood deltas

> **Historical log, not a live model.** This is a one-time observation from Task 15 of the
> spec-029 Shell-frontend work — kept for the record, not maintained. For the current shell
> mutation/cross-file model see [`mutation-classification-guideline.md`](../../mutation-classification-guideline.md)
> and [`cross-file-resolution-guideline.md`](../../cross-file-resolution-guideline.md).

Observed by running `fxrank scan` (the branch's `feat/lang-shell` build) on real, non-fixture
shell corpora — the `review-loop` plugin's helper scripts and a bulk sample of this machine's
system `.sh` files (kernel-source hooks, `/etc/init.d`-style helpers, netdata's updater,
distro driver install/remove scripts).

## Corpora scanned

1. **`review-loop` skill scripts** (`~/.claude/plugins/marketplaces/caasi-dong3/plugins/review-loop/skills/review-loop/scripts/`)
   — 3 files, 16 functions (`copilot.sh`, `pr-comments.sh`, `sandbox-preflight.sh`). GitHub-API
   wrapper scripts: mostly `gh`/`curl` calls, `set -e`/`shift`/status tracking.
2. **A 146-file bulk sample** of this machine's real system shell scripts (copied to a
   scratch dir): Linux kernel build/test scripts (`conftest.sh`, `test-bootconfig.sh`,
   `adjust_autoksyms.sh`), `netdata-updater.sh` (self-update logic, ~1400 lines), an
   out-of-tree driver's `install-driver.sh`/`remove-driver.sh`/`edit-options.sh`, netdata's
   collector `.chart.sh` scripts (incl. `libreswan.chart.sh`'s privileged `ipsec` calls),
   Debian `/etc/init.d`-style helpers (`hwclock.sh`, `console-setup.sh`, `ifupdown.sh`).

## Sanity checks — all passed

- **Destructive/deploy/network functions surface high.** `update_binpkg`/`update_build`
  (`netdata-updater.sh`, self-update via `dpkg`/`rpm`/build-from-source) and
  `install-driver.sh`'s `<script>` unit all land at `max_class: 7`, `own_score` in the
  hundreds, carrying `dynamic.code` and/or `destructive.fs` risk features. `compile_test`
  (a kernel `conftest.sh` — a loop of `gcc` compiler invocations) is the single highest
  `own_score` in the bulk sample (4497.5) — a real, non-pathological hotspot (many
  compiler-invocation call sites in one function body, correctly summed by the damping
  formula, not a bug).
- **Echo-only helpers surface low.** In the bulk sample, several pure argument-validation
  helpers (`get_latest_version`, `validate_environment_file`, `is_integer` in
  `netdata-updater.sh`) score `own_score: 0.0`/`max_class: 0` (no detected effects at all);
  `str_in_list` scores `own_score: 2.0`/`max_class: 2` (a bare `logging` effect from an
  internal `echo`). None of the low-scoring helpers in either corpus exceeded class 2.
- **The `<script>` unit is present** in every scanned file that has top-level executable
  statements, and correctly carries its own effects separately from the functions defined
  in the same file (e.g. `netdata-updater.sh`'s `<script>` unit scores `own_score: 283.0`
  independently of `update_binpkg`'s `708.5`).
- **No crashes.** The 146-file bulk scan completed with exit code 0 and zero panics.
- **Diagnostics stayed rare and genuine.** 1 of 146 files (`adjust_autoksyms.sh`, a Linux
  kernel build script) produced a parse diagnostic (`unterminated single quote at 53,28`) —
  the offending line is an apostrophe (`Let's guard against that…`) inside a `#` comment.
  `bash` itself parses this file fine (comments aren't subject to quote-tracking in real
  shell grammar), so this is a genuine frontend parser gap, not a crash and not silent data
  loss — it correctly falls back to a `Diagnostic { parsed: false }` rather than panicking or
  mis-scoring the file. Not in the Task-15 review's known-limitations list below; noted here
  as a new observation for a future fixture/fix.
- **`RiskKind::PrivilegeEscalation` and `RiskKind::DestructiveFs` both fire on real code,
  with no observed false positives.** The bulk sample's 4 `privilege.escalation` hits are
  all genuine `sudo`/`su`/`doas` invocations (`is_able_sudo_ipsec`/`libreswan_ipsec` in
  netdata's `libreswan.chart.sh`, `root_check_run_with_sudo` in a shared `functions.sh`) —
  none are `sudo` mentioned only in a comment or `echo` string (a plausible false-positive
  shape sanity-checked directly: `edit-options.sh` has 5 `sudo` mentions but all in
  comments/`echo`/usage text, and correctly produced **zero** `privilege.escalation` hits).
  The 17 `destructive.fs` hits are concentrated in the driver install/remove scripts and
  kernel test scripts, matching their genuinely destructive `rm -rf`/similar operations.
  Confirmed synthetically too: `sudo rm -rf /var/lib/app` inside a function emits both
  `privilege.escalation`/6 and `destructive.fs`/5 risk features on the same site, plus the
  expected `net.fs.db`/7 effect from `rm`.
- **`source` reaches are recorded and path-keyed**, e.g. the bulk scan surfaced
  `netdata-updater.sh:954:7 → /opt/netdata/etc/netdata/.install-type` and several Debian
  helpers sourcing `/lib/lsb/init-functions` — all `ThirdParty`, keyed by the literal path
  text (not the bare word `source`), matching spec §9.

## Known accepted-heuristic limits (surfaced during review, not fixed in this branch)

These were identified during the spec-029 review pass (Sonnet 5 + Codex) and are deliberately
left as **documented Milestone-B candidates**, not blockers — recorded here so a future author
doesn't have to rediscover them:

- **`has_file_operand` can false-positive on numeric-arg flags** (`sort -k 2`) — a flag's
  numeric argument can be mistaken for a file operand by the stream-filter heuristic.
- **Command substitution inside a redirect TARGET word is not recursed**
  (`cmd > "$(mktemp)"`) — the redirect-target side of a `>`/`>>` is not walked for nested
  `$()`, unlike the general command-substitution recursion used elsewhere.
- **`Site::FnDefine` drops subshell context** — a function defined inside `( )` within an
  enclosing function is wrongly reported `contained: false` (the subshell-forced containment
  doesn't propagate through a nested function *definition* site, only through the mutation
  sites directly inside the subshell).
- **A bare non-local write inside a function's `$()` yields no effect**
  (`f(){ x=$(g=5); }`) — command-substitution recursion runs with `is_script=true`, so the
  inner `g=5` is (mis)classified as a script-scope declaration (no effect) instead of a
  hidden write escaping the substitution's parent function.
- **`detect_pipeline`'s doc-comment example isn't actually caught** — `curl | tee f | sh`
  (the canonical "read something, hand it straight to a shell" example in the detector's own
  doc comment) fails to match because the detector's `windows(2)` adjacency check doesn't
  span the 3-stage pipe the way the example implies.
- **Disabled-feature CLI diagnostic wording differs from the sibling frontends' phrasing** —
  a minor cosmetic inconsistency in the `#[cfg(not(feature = "shell"))]` stub's error message
  versus the Rust/TS/Python equivalents.

None of these affected the dogfood sanity checks above (they are narrow syntactic corners, not
systemic false-positive/negative sources); they're listed here as the honest residual, per the
same convention `008-dogfood-deltas.md` used for spec 008.
