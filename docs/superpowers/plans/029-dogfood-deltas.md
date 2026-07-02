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

1. **A local `review-loop` plugin's `scripts/` directory** — 3 files, 16 functions
   (`copilot.sh`, `pr-comments.sh`, `sandbox-preflight.sh`). GitHub-API wrapper scripts:
   mostly `gh`/`curl` calls, `set -e`/`shift`/status tracking.
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

## Resolved after this doc was first written (Copilot PR review + final review)

Four items originally listed here were **fixed** on the branch by the whole-branch final review
and the Copilot PR review-loop (PR #66), so they are no longer limitations:

- **`has_file_operand` numeric-arg flags** (`sort -k 2`) — fixed: value-taking flags
  (`sort -k/-t/-S/-o`, `head`/`tail -n/-c`, `uniq -f/-w/-s`) now consume their argument.
- **Bare non-local write inside a function's `$()`** (`f(){ x=$(g=5); }`) — fixed: the
  substitution-recursion transient unit is now `is_script=false`, so the inner write surfaces
  as a `global.mutation` with `contained: true`.
- **`detect_pipeline` doc-comment example** — fixed (comment corrected to the real
  literal-adjacency limitation).
- **Disabled-feature CLI wording** — fixed (aligned with the sibling frontends' phrasing).

The Copilot loop also fixed: `is_computed_path` single-quoted-literal false positive,
`local_names` missing bare `local x` declarations, `risk.rs` not respecting `BuiltinOnly`, and
`env.write` wrongly contained under subshell (it now escapes per spec §A).

## Remaining Milestone-B / residual gaps (tracked as issues)

Narrow syntactic corners, not systemic false-positive/negative sources — filed as follow-ups:

- **Shebang-based routing** for extension-less scripts → **#67**.
- **Follow `source`** for cross-file symbol resolution → **#68**.
- **`alias` resolution / `trap` handlers** → **#69**.
- **`xargs` / `find -exec` argv recursion** → **#70**.
- Tracking issue **#71** for the smaller residuals: command-sub in a redirect TARGET word
  (`cmd > "$(mktemp)"`); `Site::FnDefine` dropping subshell context; `shopt -s lastpipe`;
  `.ksh`/`.zsh`/`.bats` routing; `set +e`/strict-mode risk metadata; external-tool dependency
  surface via `external_reaches`; and the upstream brush-parser apostrophe-in-`#`-comment gap
  (degrades gracefully to a `Diagnostic`).

Recorded per the same convention `008-dogfood-deltas.md` used for spec 008.
