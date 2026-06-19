# Review Journal

## specs/001-fxrank-rust-effect-scanner.md (local docs-to-main, no PR)

Reviewers: local Claude subagent + Codex (`gpt-5.5`/`codex exec review`, headless,
embedded-diff path for the unpushed docs-to-`main` commit). No Copilot (no GitHub
PR target).

- **Round 1** — Claude: *approve-with-fixes*; Codex: ~14 findings. Strong
  convergence on: (T3) catalog over-claimed "purely syntactic" for type-dependent
  signals (`.lock()`/`.borrow_mut()`/`unwrap`/`&mut` write-through); (T3) `risk_weight`
  sat after `own_score` in the rank key so a risk-only fn ranked class-0;
  undefined function-unit, occurrence counting, `own_score` float ordering; `Command::new`
  scored as an effect; `hidden.mutation` missed shared-ref interior mutation.
  → fixes: detectability tiers (exact/path/heuristic + confidence penalty); risk
  features carry a severity class feeding `max_class`; generalize `hidden.mutation`
  to any shared `&`; score the terminal effectful call not the builder; define the
  function unit/id/occurrence rules; scaled-integer ordering; Known Limitations.
- **Round 2** (Codex) — major revisions landed; remaining = formalization gaps:
  numeric confidence, incomplete risk-class table, `local.mutation` write-vs-decl,
  `assert!` detectability, lexical `unsafe` cancellation, module-level risk
  attribution, `unknown.macro` rank teeth, summary risk fields, trait-impl ids.
  → fixes applied.
- **Round 3** (Codex) — three schema/number holes: stale `scope.confidence`
  sentence, `unknown.macro` confidence/shape, `scope.risk_features` entry schema.
  → fixes applied.
- **Round 4** (Codex) — **clean**: "no remaining implementation-blockers; clean
  enough to implement." One edge-case nit (zero-hotspot summary defaults) baked in.

T3 decisions (author-approved): convex Fibonacci weights over linear/prime;
containment discount as class down-shift; risk participates in ranking; tool emits
facts only (no `suggested_moves`); own-score-only extract-method laundering
accepted + documented until call-graph propagation lands.

Convention candidate: "constructor is not the effect; the terminal effectful call
is" — worth carrying into the JS/TS catalog when that frontend is specced.

## plans/001-fxrank-rust-effect-scanner.md (local docs-to-main, no PR)

Reviewers: local Claude subagent (ran a live `rustc 1.96`/`cargo` toolchain to verify
serde f64 output, syn 2.x spans, `parse_file` error shape) + Codex (resume of the
spec session, so it carried full spec knowledge).

- **Round 1** — Claude: *needs-rework* (two real source-of-truth conflicts: per-effect
  `confidence` in the struct but not the spec JSON; whole `own_score` serializes as
  `3.0` not `3`). Codex: feature-gate missing, `syn` `visit` feature missing,
  `GlobalMutation` base class 3≠6, `RiskFeature` missing `path`, `Hotspot` missing
  `await_count`, Task 7 test not runnable, `commit -am` drops new files, no CI task,
  Task 13 too big. → all fixed; spec adjusted (own_score `.0`, no per-effect
  confidence, defer global.mutation class-4).
- **Round 2** (Codex) — coverage gaps: `ambient.read`, heuristic `unwrap`/`expect`,
  `ExternBlock` risk kind, scope-risk roll-up test, no-feature CLI build,
  env.write+unsafe fixture. → fixed.
- **Round 3** (Codex) — two spec/plan mismatches surfaced (spec still required FFI
  call-site; spec's `unknown.macro` example still serialized `confidence`) + a Task 11
  prose gap. → reconciled in the spec; FFI call-site deferred.
- **Round 4** (Codex) — **clean**: "no remaining implementation-blockers; the spec
  and plan are now clean enough to implement."

T3 decisions: feature-gated frontend (optional dep + `[features]`), single binary;
`RiskKind` enum centralizes wire-strings/classes; FFI call-site + global.mutation
class-4 + a semantic pass all deferred to a later milestone. The live-toolchain
Claude pass was the highest-value reviewer — it caught serialization facts a static
read would miss.
