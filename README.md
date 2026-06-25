# FxRank — GitHub Pages deploy branch

This `gh-pages` branch is the published site at <https://caasi.github.io/fxrank/>.
It is an orphan branch (no Rust source) holding only the static site.

Changes land here by **pull request**, reviewed before merge — see the repository's
default branch and issue #12 for the rationale. The page deliberately carries only the
**durable** facts (what FxRank is, its thesis, its epistemic stance); install, usage,
flags, the output schema, and the evolving known limitations are *not* duplicated here
because they drift — they live in the crate, the binary's `--help`, and the issue
tracker.

`.nojekyll` disables Jekyll processing so the HTML is served verbatim.
