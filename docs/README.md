# `docs/` — FxRank shared knowledge

Two tiers live here:

1. **Shared-knowledge references** (this directory's root) — the durable, cross-language
   models every frontend must agree on. These are *descriptive* ("code wins where they
   disagree") but they are the working contract. **Read these, not the spec history.**
2. **SDD artifacts** (`docs/superpowers/specs/`, `docs/superpowers/plans/`) — the numbered
   spec→plan pairs that *produced* the code. They are historical record and the
   **tie-breaker of record**: read one only when the shared knowledge contradicts itself
   or the code, never as a how-to. (Then fix the guideline so the next reader needn't look.)

## If you are adding a new language frontend

You should be able to build a working frontend from **this README + the shared-knowledge
docs + the code**, without reading any spec. Start here:

1. **[`adding-a-language-frontend.md`](adding-a-language-frontend.md)** — the prescriptive
   authoring checklist: the trait to implement, the crate skeleton, every per-axis
   decision, wiring, and the test bar. It links out to the three guidelines below at the
   points where each decision is made.
2. Then the three shared models, each governing one axis you must fill a column into:
   - **[`mutation-classification-guideline.md`](mutation-classification-guideline.md)** — how a write site becomes a mutation effect.
   - **[`corpus-profile-guideline.md`](corpus-profile-guideline.md)** — which files are scanned vs skipped (your `CORPUS_PROFILE`).
   - **[`cross-file-resolution-guideline.md`](cross-file-resolution-guideline.md)** — imports → definitions, the call graph, propagation, and the first-party/third-party boundary.
3. Normative source for scores (no spec needed): `crates/fxrank-core/src/score.rs`
   (weights, discounts, `own_score`, `rank_key`) and the `EffectKind`/`RiskKind`
   vocabularies in `crates/fxrank-core/src/effect.rs`.

## Reference map

| File | Genre | Governs |
|---|---|---|
| `adding-a-language-frontend.md` | Prescriptive guide | Building a new frontend end-to-end |
| `mutation-classification-guideline.md` | Descriptive model | Write-site → mutation effect, per language |
| `corpus-profile-guideline.md` | Descriptive model | File selection / skip, per language |
| `cross-file-resolution-guideline.md` | Descriptive model | Cross-file resolution, call graph, propagation |
| `008-dogfood-deltas.md` | Historical log | One-time spec-008 before/after observation (not a live model) |

All three descriptive models roughly share one shape — a **shared model**, a per-language
breakdown (a table in the mutation/corpus guidelines; a richer structure in the cross-file
one), **Honest per-language differences (intentional)**, and **Per-frontend realization**
(headings vary slightly) — so a new language is, in each, one new column/bullet plus one
realization bullet.

## SDD artifacts (`docs/superpowers/`)

Numbered `NNN-*` spec/plan pairs share a 3-digit prefix (`specs/003-*` ↔ `plans/003-*`).
**Spec 001 is the scoring source-of-truth of record**, but for day-to-day frontend work
the code (`fxrank_core::score` / `effect`) is the live authority — consult 001 only when
you need the original rationale. The 025-series specs cover cross-file resolution +
propagation; their durable content is distilled into the cross-file guideline above.
