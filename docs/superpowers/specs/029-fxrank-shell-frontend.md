# Spec 029 — FxRank Shell frontend (`fxrank-lang-shell`)

Status: **Draft** · Issue: #15 · Milestone: A (fourth frontend candidate)

## 1. Purpose

Add a **Shell** language frontend (`crates/fxrank-lang-shell`, feature `shell`) that scores
Bash/POSIX-ish `.sh`/`.bash` source by own-body effect cost, the same way the Rust, TS/JS,
and Python frontends do.

Shell is a deliberate **contrast case**. It has no useful static type boundary: it is
declared `BoundaryCoverage::None`, so **no containment discount applies** — nearly every
command is a world boundary. The value proposition is unchanged: give an agent a ranked list
of effect hotspots so it can find the destructive / network / deploy code fast and refactor
toward smaller, more contained cores. FxRank stays a **measuring instrument** (facts +
evidence + confidence, no advice) and **primarily syntactic** (no shell execution, no alias
resolution, no `shellcheck`-style style linting).

## 2. The one invariant (from the authoring guide §0)

Hidden state must score **higher** than honestly declared state. Shell has no
`&mut`/`&self` analog, so the invariant turns on **declaration vs hidden write**, mirroring
how Rust treats `static`: *declaring* a `static` is not an effect; only a *write* to it is.
The ladder:

- **Declared** — `local x=…` → `local.mutation`/1; and a **top-level** (`<script>`-scope)
  `FOO=bar` is a *declaration* of the script's own global (like a Rust `const`/`static`
  declaration) → **no mutation effect** (§7). This is **top-level-vs-in-function**, *not* a
  claim about the RHS: a dynamic RHS such as `VERSION=$(git describe)` is still a top-level
  declaration for the *assignment*, and the `$(git describe)` substitution's own effects are
  charged separately (§4). Honest, visible, cheap.
- **Honest cross-process** — `export X=…` → `env.write`/6 (openly declares it crosses the
  process boundary into child processes).
- **Hidden** — an assignment **inside a function to a name it did not declare `local`**
  (silently writing shared outer/global state — or, via dynamic scope, a caller's `local`)
  → `global.mutation`/6. This is the shell "spooky action at a distance".
- **Hidden via code execution** — `eval "$name=v"` (actually runs code) → **`DynamicCode`**/7
  risk. (`printf -v`/`read` into a *computed* name is indirect assignment, *not* code
  execution → `global.mutation`/6 with `hidden`, see §7 — never below a plain named non-local
  write.)

The invariant holds construct-by-construct: for the *same* script-global, a **declared**
top-level `FOO=bar` (no effect) scores **below** a **hidden** function-body `FOO=…` with no
`local` (`global.mutation`/6). Nothing lets "declaring your effect" outscore "hiding it".
There is **no discount channel** at all (`BoundaryCoverage::None`) — the point of this
frontend as a contrast to the typed languages. `export`/`env.write` is a *different axis*
(crossing the process boundary), not a more-severe global: `global.mutation` measures shared
mutable state *within the shell*; `env.write` measures the *child-process environment*.

## 3. Parser decision

**brush-parser** (`brush-parser` crate, MIT, pure-Rust, actively maintained — 0.4.0 on
crates.io as of 2026-05, releases throughout 2026). It exposes a real Bash AST via a
tokenize-then-parse pipeline: **`tokenize_str(&str)` → `parse_tokens(tokens: &[Token],
options: &ParserOptions)`** (a **2-arg** signature, verified against the shipped 0.4.0 source;
there is no `SourceInfo` argument on this path). The crate exposes the AST types

`ast::{Program, SimpleCommand, Pipeline, AndOrList, FunctionDefinition, Assignment, Word,
IoRedirect, and the real compound-command variants IfClauseCommand / ForClauseCommand /
WhileOrUntilClauseCommand / CaseClauseCommand / BraceGroupCommand / DoGroupCommand /
SubshellCommand / ArithmeticCommand, ProcessSubstitutionKind}`

**and real source-location types (`SourceSpan`/`SourcePosition`)** — verified on docs.rs by
both reviewers — which is what de-risks the spike (see below).

Rejected alternatives:

- **yash-syntax** — **GPL-3.0-or-later**, incompatible with fxrank's `MIT OR Apache-2.0`
  (would force the whole binary to GPL). Hard blocker.
- **tree-sitter-bash** — C grammar dependency; violates the pure-Rust preference.
- **hand-rolled lexer** — too fragile for heredocs/quoting/nested substitution; the issue
  explicitly warns against pretending to understand shell grammar with a hack.

Per the contract, the parser lives **only** in `fxrank-lang-shell`; `fxrank-core` stays
parser-free.

**Parser spike is task 0 of the plan.** Two things to nail down before anything else:
(1) the **exact** parse entry signatures and how to obtain a `Program` from a `&str`;
(2) how `SourceSpan`/`SourcePosition` attach to the AST nodes we care about — we need
`line`/`col` for every effect's evidence. Spans are confirmed to *exist*; if a given node
doesn't expose one directly, map it back through token positions. If source locations turn
out to be unusable, we stop and re-scope rather than shipping effects without evidence.
The spike also confirms how brush-parser represents a few syntactically awkward constructs so
fixtures match the real AST — notably **`time`** (a reserved word over a pipeline, not merely a
command word) and the compound-command/redirect-list attachment points.

## 4. Effect vocabulary — command classification

The heart of the frontend is a **command classifier** keyed on the first word of a
`SimpleCommand`. The governing rule (confirmed with the maintainer):

> **Any command word that is not a recognized pure builtin is a process spawn**
> (`process.control`/6). Only a small allowlist scores low.

This makes "everything is mostly a world boundary" the honest default — unknown commands
surface rather than being silently treated as pure.

| Category | Example command words | `EffectKind` | class |
|---|---|---|---|
| pure builtin (low/no effect) | `:` `true` `false` `test` `[` `[[` `return` `break` `continue` | (none) | — |
| positional-param mutation | `shift`, `set --` | → §7 (`local.mutation`/1, function-scoped) | 1 |
| declaration / assignment builtins | `local` `declare`/`typeset` `readonly` `let` `(( … ))` `getopts` | → §7 mutation model | (per §7) |
| stdout writers | `echo` `printf` (without `-v`) | `logging` | 2 |
| stdin/var readers (external input) | `read` `mapfile`/`readarray` reading real stdin/terminal/a redirected file | `net.fs.db` (input-boundary IO) + §7 var write | 7 |
| filesystem — always touches disk | `cp` `mv` `rm` `mkdir` `rmdir` `touch` `ln` `chmod` `chown` `dd` `truncate` `install` `shred` `mktemp` `stat` `readlink` `ls` `find` | `net.fs.db` | 7 |
| filesystem — **only with a file operand** (see *stream-filter rule*) | `cat` `grep` `sed` `awk` `head` `tail` `sort` `uniq` `wc` `cut` `rev` `tee` (write side) | `net.fs.db` (else none) | 7 (else —) |
| never fs | `tr` (stdin→stdout only) | (none, unless redirected) | — |
| network | `curl` `wget` `ssh` `scp` `sftp` `rsync` `nc` `telnet` `ftp` | `net.fs.db` | 7 |
| database | `psql` `mysql` `sqlite3` `mongo` `redis-cli` | `net.fs.db` | 7 |
| deploy / infra / process | `docker` `kubectl` `helm` `terraform` `ansible` `aws` `gcloud` `az` `systemctl` `service` **+ any unrecognized command word** | `process.control` | 6 |
| environment write | `export X=…` / `export -n X` / `unset X` (env-affecting) | `env.write` | 6 |
| global-state mutation (pwd / options / limits) | `cd` `pushd` `popd` `set` `shopt` `umask` `ulimit` | `global.mutation` | 6 |
| concurrency / background | trailing `&`, `wait` `coproc` `jobs` `disown` | `concurrency` | 6 |
| process replacement | `exec cmd …` | `process.control` (+ redirs, §redirection) | 6 |

**Design notes:**

- **Command-prefix wrappers recurse into their argv.** `sudo`/`su`/`doas`, `command`/`builtin`,
  and best-effort `env`/`nice`/`nohup`/`exec` do **not** classify by their own name alone
  (**`time` is NOT a wrapper** — it is a reserved word represented as `Pipeline.timed`, and
  `time h` *runs* a shell function `h`, so it resolves Normally; do not put it in the bypass set)
  — the frontend strips the wrapper (and its options / `VAR=val` prefixes) and **re-classifies
  the remaining command** as its own `SimpleCommand`, then **unions** the wrapper's own risk
  (`sudo`/`su`/`doas` → `PrivilegeEscalation`, §5) / effect (`exec` → its own
  `process.control`/6) with the wrapped command's effects+risks. So `sudo rm -rf /x` yields
  `net.fs.db`/7 **and** `DestructiveFs`/5 **and** `PrivilegeEscalation`/6 → `risk_class` 6
  (`risk_weight` 13) — genuinely *out-ranking* a bare `rm -rf /x` (`risk_class` 5, `risk_weight`
  8) on the `rank_key` risk-weight element. (This is why `PrivilegeEscalation` is class 6, not 5
  — see §5.) `exec rm -rf /x` similarly layers `process.control`/6 (own) atop
  `net.fs.db`/7 + `DestructiveFs`/5 (same shape as `sudo`).
  - **Resolution mode differs per wrapper (§9 interaction).** The wrapped word is **not**
    subject to §9's same-file-function precedence for wrappers that exec a program or force a
    lookup class: `sudo`/`su`/`doas`/`env`/`nice`/`nohup`/`exec` run an **external
    program** (they never see shell functions) and `command` **explicitly bypasses functions**
    → so `sudo docker` / `command docker` classify `docker` as the external tool, never a
    same-file `docker()` function. `builtin foo` is **builtin-only** → classify `foo` as a
    builtin (never recurse to an external `foo`). Only an *unwrapped* call consults §9's
    function → builtin → external order.
  - (`xargs cmd`, `find … -exec cmd` argv recursion is Milestone-B, §14.)
- **Assignment prefixes (`VAR=val cmd`).** A `SimpleCommand` may carry `VAR=val` prefixes.
  When followed by a command, they are a **temporary environment for that command only** (they
  do not persist) → treated as a scoped `env.write` on that command's line, **not** a script
  mutation. A prefix with **no** following command (`VAR=val` alone) is a real assignment →
  routed to §7's mutation model. (The POSIX **special-builtin persistence** corner — a `VAR=x`
  prefix on a special builtin like `export`/`eval`/`:`/`.`/`readonly`/`set`/`shift`/`trap`/`unset`
  *does* persist — is an accepted heuristic edge for Milestone A across the whole list, not just
  `export`.)
- **Subshell / execution-environment boundary (§A decision).** Bash runs command
  substitution `$(…)`/backticks, explicit subshells `( … )`, background jobs `cmd &`, and (by
  default) each **pipeline stage** in a *separate execution environment*. Mutations there
  (`cd`, bare `x=`, `local`, `set`, `unset`, `read` into a var …) **do not escape** to the
  caller shell, so the walker tracks a "subshell depth"; inside it, **mutation effects are
  marked `contained`** (own-body still records them, but they do **not** propagate via the
  fold) while **world effects (fs/net/process/env-visible-side-effects) still count** (they hit
  the world regardless). **Launching a background job (`cmd &`, `coproc`)** is a
  `concurrency`/6 effect that **escapes** — the job genuinely outlives the statement. A plain
  **multi-stage pipeline (`a | b`) does NOT emit a concurrency effect**: it is bounded and
  *joined* before the next statement (synchronous shell control flow), so charging it
  `concurrency`/6 would push every `… | grep …` text helper to class 6 and flatten the ranking
  the tool exists to provide — the stages' own effects already count. The `shopt -s lastpipe`
  exception (last pipeline stage runs in the parent) is an accepted miss (§14).
- **The stream-filter rule (pipe containment), per-command.** The "only with a file operand"
  row holds only for tools that genuinely read a *named file* vs *stdin*: `cat`/`grep`/`sed`/
  `awk`/`head`/`tail`/`sort`/`uniq`/`wc`/`cut`/`rev`. With a file operand (`grep pat f.txt`,
  or a file via option `grep -f pats`, `sort -o out`, `sed -f script`, `awk -f prog`) →
  `net.fs.db`/7. As a bare stdin stage (`… | grep pat`) → **no fs effect** (the pipe bounds the
  flow, not a durable sink). Corrections baked into the table above (from review): `tr` **never**
  takes a file operand → never fs; `ls`/`find` default to `.` when given no path → they
  **always** perform fs IO → moved to the *always* row; `stat`/`readlink` require a path → also
  *always*. Operand-vs-flag detection is best-effort (a positional `Word` not consumed by a
  known option); ambiguous cases lower confidence (§6) rather than guess.
- **Redirections apply to every command form, not just `SimpleCommand`.** Output redirects to a
  file (`> f`, `>> f`, `>| f` noclobber-override, `<> f`, `exec > f`, `2> f`) → `net.fs.db`/7
  **write**; **input** redirect from a file (`< f`) → `net.fs.db`/7 **read** (this is how a
  stdin-filter stage acquires a real fs effect). A redirect list may hang off a **compound
  command or a function definition** — `f(){ …; } >out`, `{ …; } >out`, `while …; do …; done
  >out` — and is attributed to the unit whose body runs it (the redirect fires when that unit
  executes). fd-dups (`>&1`, `2>&1`), here-docs (`<<EOF`) and here-strings (`<<< "$x"`) feed a
  descriptor, **not** a file → no fs effect of their own. A persistent `exec > f` (no command)
  redirects the *script's* stdout → a script-level fs write.
- **Command / process substitution** `$(cmd)` / backticks / `<(cmd)` / `>(cmd)`: the inner
  command is classified recursively (its effects charged to the enclosing unit, but as
  *subshell* context per the boundary rule above — mutations contained, world IO counts).
  Confidence is lowered slightly (§6). **The `<(…)`/`>(…)` pseudo-file that appears as an
  operand of the *surrounding* command is NOT a durable file** — `grep pat <(gen)` /
  `cat < <(gen)` must **not** add an fs effect for the outer command; only the inner `gen`'s
  effects count (plus the outer command's own non-file effects).
- **Assigning parameter expansions.** `${var:=word}` (and `${var:=word}` via `: ${x:=…}`)
  *assigns* `var` as a side effect of expansion. It is a mutation site subject to §7's
  local-vs-non-local rule exactly like a bare `x=…` (inside a function, to a non-local name →
  `global.mutation`/6). Best-effort syntactic detection of the `:=` form.
- **fs reads and writes both map to `net.fs.db`/7**, matching Rust (`std::fs::read` is class 7
  there too). The destructive-vs-read distinction is carried by **risk** (§5), not the effect
  class — so `rm -rf` out-ranks `cat file` on `risk_weight` even though both are class 7.
- **`read`/`mapfile` measure the *input boundary*, not the filesystem specifically.** Reading
  real stdin/terminal/a redirected file is external world input → `net.fs.db`/7. But when the
  input is an **in-process descriptor** — a here-string (`read x <<< "$s"`) or here-doc
  (`<<EOF`) built from in-shell data — there is **no external boundary** → **no IO effect**,
  only the §7 variable write. (Consistent with the redirection rule: here-strings/here-docs
  feed a descriptor, not a file.)
- **Function-vs-command precedence.** A command word that matches a **same-file** function
  name (§9) resolves to that function (a call ref), **before** the external-command
  classification — mirroring bash's own resolution order (function → builtin → external). A
  script that defines `docker()` and calls `docker` calls its own function, not the tool.
- **`echo`/`printf`** are `logging`/2 so echo-only helpers stay well below fs/net/deploy —
  satisfying acceptance criterion 2.

No new `EffectKind` variants are needed — the existing vocabulary covers shell (the only core
additions are the two `RiskKind`s in §5).

## 5. Risk vocabulary — 2 new `RiskKind`, 1 reused

Add to `fxrank_core::effect::RiskKind` (with `wire()` + `class()` + `escapes()`):

| Trigger | `RiskKind` | class | `escapes()` |
|---|---|---|---|
| `eval …` (runs code), `source`/`.` on a **computed** path, a **download-piped-to-shell** (see below) | **`DynamicCode`** (reuse existing, class 7) | 7 | true (already) |
| `rm -rf` / `rm -r`, `chmod -R`, `chown -R`, `dd`, `shred` | **`DestructiveFs`** (new) | 5 | **true** |
| `sudo`, `su`, `doas` | **`PrivilegeEscalation`** (new) | **6** | **true** |

All three are capability risks — they propagate to callers, so `escapes()` is `true`
(consistent with the existing capability judgment table in `RiskKind::escapes`).
`PrivilegeEscalation` is **class 6** (deliberately above `DestructiveFs`/5 and the
`UnsafeBlock`/5 tier, below `DynamicCode`/7): running as another user is a more severe
capability than a bounded recursive fs op, so a wrapped destructive command
(`sudo rm -rf`, `risk_class` 6) correctly out-ranks the bare one (`risk_class` 5) on the
`rank_key` risk-weight element (13 vs 8). Class 6 is otherwise unused by `RiskKind` — a gap
in the scale, which is fine.

- **`DynamicCode` is code execution only.** `eval "$n=v"` counts (eval *runs* the string).
  **`printf -v "$var"` is indirect *assignment*, not code execution** → it is **not**
  `DynamicCode`; it is a `global.mutation`/6 with `hidden` (§7). Mislabeling it as code
  execution would make reports claim execution where none occurred.
- **Download-piped-to-shell** is a best-effort *syntactic* pattern, not just `curl|sh`:
  a network fetch (`curl`/`wget`/…) whose output flows into a shell interpreter — the classic
  pipe `curl … | sh`/`| bash`/`| zsh`/`| dash`, and the substitution form
  `sh -c "$(curl …)"` / `bash -c "\`curl …\`"`. Downloaded-temp-file-then-execute is **not**
  matched in Milestone A (data-flow tracking; §14). The grammar we match is stated in the
  plan; anything outside it is an accepted miss, not a silent guarantee.
- **`sudo`/`su`/`doas`** additionally trigger the wrapper-recursion of §4 — the
  `PrivilegeEscalation` risk is *unioned* with the wrapped command's own effects/risks, never
  replacing them.

**Multiple risks on one unit.** A single command can raise several `RiskFeature`s
(`rm -rf` → `DestructiveFs`; `sudo rm -rf` → `DestructiveFs` **and** `PrivilegeEscalation`).
The frontend collapses them exactly as the existing frontends do (verified in
`fxrank-lang-rust`'s `detect::analyze_unit`): `risk_class` = **max** over the unit's risk
classes; `risk_weight` = `weight_for_class(risk_class)` — the weight of that single top risk
class, **not** a sum. All raised `RiskFeature`s are still emitted individually in the report's
`risk_features[]`; only the `rank_key` inputs collapse to the max.

**Unquoted variable in a destructive command** (e.g. `rm -rf $DIR` unquoted) is **not** a
separate risk kind — it lowers the confidence of the paired **`net.fs.db` effect** (§6), **not**
the risk (`RiskFeature` carries no confidence; function confidence is computed from effects
only). This keeps us out of the "reimplement shellcheck" trap.

## 6. Confidence & detectability tiers

Every shell signal is tier **`heuristic`** — there is no type info and a command name can be
shadowed by a function or alias. Confidence values (per detection; surfaced only at the
function level as the weakest-link min):

| Situation | confidence |
|---|---|
| literal recognized command (`rm -rf x`) | base 0.9 |
| literal assignment / mutation detection (`x=…`, `local`, `cd`, `export`, `printf -v x`) | base 0.9 |
| unrecognized command word (spawn certain, category unknown) | base 0.7 |
| command via variable expansion (`$CMD`, `${cmd} …`) | base 0.5 |
| indirect/computed mutation target (`printf -v "$var"`, `read "$name"`, `${x:=…}`) | base 0.5 |
| `eval` / computed `source` (already `DynamicCode`) | base 0.4 |
| command inside `$(…)` / `<(…)` substitution | −0.1 |
| unquoted variable in a destructive command | −0.1 |
| ambiguous file-operand (stream-filter rule can't decide) | −0.1 |

**Combination rule.** Exactly one **base** applies per detection (the most specific row —
`eval`/computed-`source` < variable-expansion < unrecognized < literal in specificity; a
lower base wins when several could match). The **deltas** are then subtracted additively and
the result is **floored at 0.1**. Per-detection confidences surface only at the function level
as the weakest-link **min** (matching the sibling frontends).

## 7. Mutation model (fills a new column in the mutation guideline)

The organizing question for every assignment is **§2's declaration-vs-hidden line**: *is this
an assignment, inside a function, to a name it did not declare `local`?* If yes → hidden
write. Otherwise → a declaration.

| Concept | Shell construct | `EffectKind` | class | `contained` / `hidden` |
|---|---|---|---|---|
| declared local | `local x=…`, `declare`/`typeset x=…` (no `-g`/`-x`); `local -x x=…` also emits an `env.write` | `local.mutation` | 1 | contained |
| positional-param mutation | `shift`, `set --` (positional params `$1..$n`; function-scoped in a function, the script's args at top level) | `local.mutation` | 1 | contained |
| **top-level declaration** | a `<script>`-scope `FOO=bar` / `readonly FOO=…` / `let`/`(( x=… ))` **declaring** the script's own global (RHS content irrelevant; RHS `$(…)` effects charged separately) | **no effect** (like a Rust `const`/`static` declaration) | — | — |
| **hidden non-local write** | inside a function, to a name **not** declared `local` here: bare `x=…`, `x+=…`, `let x=…`, `(( x++ ))`, `${x:=…}`, `declare -g x=…`, `readonly x=…` (readonly does **not** create local scope), `printf -v x`, `read x`, `mapfile x`, `getopts … x` (also writes global `OPTIND`/`OPTARG`) | `global.mutation` | 6 | escaping |
| hidden indirect | `printf -v "$var"`, `read "$name"`, `${!ref}=`-style (target name **computed**) | `global.mutation` (subreason `indirect-assign`) | 6 | escaping + `hidden` |
| global-state mutation | `cd`/`pushd`/`popd` (global `pwd`), `set`/`shopt` (options), `umask`/`ulimit` (limits) | `global.mutation` | 6 | escaping |
| env write | `export X=…`, `export -n X`, `declare -x`/`typeset -x`, `unset X` where `X` is exported / global | `env.write` | 6 | escaping |
| env-unset of a local | `unset x` where `x` is a function-local | `local.mutation` | 1 | contained |
| code-execution write | `eval "$n=v"` (runs code) | (`DynamicCode` risk, §5) | 7 | — |

- **No discount** is applied. `apply_boundary_discount(class, BoundaryCoverage::None,
  contained)` is a no-op (shift 0) and is used uniformly so the "None boundary" is explicit
  in code. There is **no** mut-channel `apply_discount` (that is Rust-only ownership).
- **Subshell containment (§A).** An assignment inside a subshell context (`$(…)`, `( … )`,
  `cmd &`, a non-`lastpipe` pipeline stage) does not escape the subshell → its mutation effect
  is marked **`contained`** (recorded in own-body, not propagated). World effects on the same
  line still count.
- **Local-name resolution is a per-function pre-scan** (the set of names declared `local`/
  `declare`/`typeset` **without `-g`** in *this* function) — **`readonly` is deliberately
  excluded** (it does not create local scope; see below), plus the script-top binding set.
- **Loop vars vs `read`/`mapfile`/`getopts` targets (intentional split).** A `for i`
  loop variable is **not** actually function-local in bash (`f(){ for i in 1 2; do :; done; };
  f; echo $i` prints `2`), but loops are pervasive and low-signal, so Milestone A treats a
  `for` loop var as a **documented noise-reduction heuristic → local (contained)**
  (recorded here and in §14 as an intentional under-report). (`select` has no AST command
  variant in brush-parser 0.4.0 — only `ForClauseCommand` — so it is not handled in Milestone A.)
  By contrast an explicit
  `read x`/`mapfile x`/`getopts … x` writing a **non-local** name is a deliberate write → the
  hidden-non-local-write rule (`global.mutation`/6) applies — these are **not** in the local
  pre-scan.
  A function-scoped name shadowing a script-global still resolves to local — an accepted
  heuristic limit (same residual as TS/Python per spec 008). We do **not** attempt to prove
  whether a bare function write lands on a true script-global or a caller's `local` via dynamic
  scope: both are "hidden non-local writes" and both earn `global.mutation`/6, so the
  ambiguity does not change the score (recorded as a known limit, §14).
- **`readonly` does not create local scope.** Only `local`/`declare`/`typeset` do. `readonly x=1`
  inside a function sets a *global* readonly (verified: `f(){ readonly x=1; }; f; declare -p x`
  leaves `x` global), so it follows the bare-assignment rule: top-level → declaration; in a
  function on a non-local name → `global.mutation`/6.
- **Indirect writes are never *below* named ones.** `printf -v "$var"` / `read "$name"` with a
  computed target is `global.mutation`/6 with `hidden` (not the lower `hidden.mutation`/3 tier),
  since a dynamically-named write is at least as hard to audit as a named-but-undeclared one
  (§7 hidden-indirect row) — keeping the anti-Goodhart ordering intact.
- **`unset -f name`** removes a *function*, not a variable — it is a global-namespace mutation
  → `global.mutation`/6, not `env.write`.

## 8. Function units & the synthetic `<script>` unit

- `functions::collect` walks the `Program` and emits one `FnUnit` per `FunctionDefinition`
  (both `name() { … }` and `function name { … }` forms), including nested definitions
  (nested-fn effects are its own; the enclosing carries a call ref).
- **Nested `function` definitions are *not* closures** (unlike Python's nested `def` or Rust's
  block-local `fn`). Defining `inner(){…}` inside `outer` installs `inner` into bash's single
  **global** function namespace the first time `outer` runs, and it persists after `outer`
  returns. Milestone A records this as a `global.mutation`/6 on `outer` (a global-namespace
  write, subreason `fn-define`) with a known-limitation note (§14) — the collected `FnUnit` for
  `inner` is still scored on its own body; what we add is the side effect of the *definition*.
- A synthetic **`<script>`** unit (symbol `"<script>"`, line 1, col 1) captures every
  top-level command/assignment **outside** any function — the script body. Many shell scripts
  define no functions at all, so this is often the most important unit. Emitted only when the
  script has executable top-level statements (mirrors Python `module_init_unit`, but named
  `<script>` for shell idiom — the fold matches units by `unit_id`, not by this display
  string, so the divergence from Python's `<module>` is safe).

## 9. Cross-file resolution & refs

- **Function-call resolution is same-file only (§B decision), via `canonical_path` — not the
  flat `SymbolIndex`.** Shell functions are **not** linked across files just because the files
  were scanned together — a function exists in the running shell only after it is defined (or
  `source`d) at runtime. Two unrelated scripts that each define `log()` do not call each other's
  `log`. **The generic `SymbolIndex` is keyed by simple name over the whole per-language
  partition and drops any name with >1 candidate as ambiguous** — so routing a plain
  `CallSiteRef{base:"log"}` through it would drop *even a script's call to its own same-file
  `log`* the moment another scanned script also defines `log` (precisely shell's common case).
  To get exact, same-file resolution the frontend does what Rust/TS/Python already do (spec
  025-3e, `CanonicalIndex`/`resolve_ref_precise`, #36 shipped): it assigns each unit a
  **`canonical_path` unique per file** — `[path, "fn", name]` for a function, `[path, "<script>"]`
  for the synthetic unit — and, for a call to a **same-file** function, sets the ref's
  **`resolved_target`** to that callee's `canonical_path` (with `first_party: true`). Resolution
  then goes through the exact `CanonicalIndex` lookup, not the ambiguity-prone flat index, so the
  SCC-condensed fold propagates escaping effects correctly (extracting a helper within a script
  does **not** wash the caller's score) even when helper names collide across files. A word
  matching a function only in a *different* scanned file is **not** linked (a false edge). The
  cross-file helper-name-reuse reality (`log`/`die`/`main`/`usage`) is recorded as a per-language
  honesty note in §15.
- **Same-file resolution is by name, order-insensitive (accepted heuristic).** Bash functions
  exist only after their definition line executes, so `foo; foo(){ …; }` actually calls an
  external `foo`. Milestone A matches by name regardless of textual order (a call *before* the
  definition is a documented rare miss, §14) — the syntactic frontend does not model execution
  order.
- **`source`/`.` executes code in the *current* shell**, so it cannot silently vanish from the
  ranking. Milestone A emits, at the `source` site, an **opaque `process.control`/6 effect**
  (the sourced script runs *something*, unknown to us) **plus** a **path-keyed** `external_reach`.
  Its `CallSiteRef` always carries **`resolved_target: None`, `qualified: true`** so it lands in
  `resolve_ref_precise`'s unconditional-Opaque branch and **never** touches `CanonicalIndex`'s
  exact map or the name-based `SymbolIndex` fallback (which would otherwise collide every file's
  shared `"<script>"` symbol, or wrongly *follow* an in-scope target — following is Milestone-B).
  The reach is keyed by **file path** (the fold's specifier = `module.unwrap_or(base)`, so the
  `CallSiteRef.base = <literal path>`). Milestone-A reach shape is deliberately **flat**, because
  the wire type `ExternalReach { specifier, kind, site }` has **no confidence field** and a
  pass-1 frontend has **no scanned-file set** (so `first_party` can't be set): every resolvable
  `source` path emits a uniform **`ThirdParty`** opaque reach. Precise `FirstPartyOutOfScope`
  classification and confidence-graded reaches are **Milestone-B** (they need the cross-file
  view). Path handling — note shell resolves `source` paths against the process **cwd at
  runtime**, unknowable statically:
  - a **literal path** (absolute `/abs/x.sh`, or slash-relative `./x.sh`/`../lib/x.sh`) → own
    `process.control`/6 effect **plus** a `ThirdParty` path-keyed reach (`base = the literal
    path`). Slash-relative paths are cwd-relative, so the reach is a best-effort pointer, not a
    resolved file (accepted heuristic miss, §14);
  - a **bare filename, no slash** (`source common.sh`) → same: own effect + a `ThirdParty`
    reach keyed on the bare name — **not silently dropped** (PATH/cwd lookup is unknowable);
  - a **computed path** (`source "$dir/x"`, `source "$(dirname "$0")/x"`) → `DynamicCode` risk
    (§5) + the own `process.control`/6 effect, and **no reach** (the `base` would be
    non-literal garbage — skip emitting a `CallSiteRef` for it).

  A `source`'s own opaque `process.control`/6 effect is a **world effect**, so when the `source`
  appears inside a subshell context (`$(…)`/`( )`/`&`) it follows §A's general rule (world
  effects still count; it is **not** contained).
- External command invocations (`docker`, `curl`, …) are **effects**, not `external_reaches`
  entries in Milestone A (recording the tool-dependency surface via `external_reaches` is a
  documented Milestone-B option, §14).
- **Imports & bindings axis (guide §3).** Shell needs **no `ImportTable`** — there is no
  import syntax; the only cross-unit linkage is same-file function calls (above) and `source`
  (via the reach mechanism). The only binding set the frontend maintains is the per-function
  local-name pre-scan + the script-top binding set used by §7's mutation resolution.
- `is_root` is always emitted `false`; the CLI sets roots from explicit FILE args.

## 10. Corpus profile & test-skip

```rust
pub const CORPUS_PROFILE: CorpusProfile = CorpusProfile {
    prune_dirs: &[],                                  // no standard shell vendor/build dir
    exclude_file_globs: &[],
    test_file_globs: &["*_test.sh", "test_*.sh"],     // file-name based
    prune_marker_files: &[],
};
```

Shell has **no standard in-file test marker** (unlike Rust's `#[test]` or Python's
`test_*`/`pytest`), so test-skip is **file-name based only** (a legitimate per-language
decision to record in `corpus-profile-guideline.md`). `--include-tests` toggles the file-glob
skip; matched files count toward `scope.skipped_tests` (a frontend test-skip, like the other
frontends — **not** `skipped_excluded`, which is for `--exclude` corpus prunes).
`.bats` files are out of scope (not routed).

## 11. Wiring (per authoring guide §5)

1. Workspace member `crates/fxrank-lang-shell` in root `Cargo.toml`.
2. `Language::Shell` variant in `fxrank-core/src/frontend.rs`.
3. CLI feature `shell = ["dep:fxrank-lang-shell"]` (in the default set); optional dep.
4. CLI dispatch: `Route::Shell`; `route_for_path` maps `.sh`/`.bash`; `--lang shell` for
   stdin; `dispatch_shell` (+ `#[cfg(not)]` stub); push `CORPUS_PROFILE` in
   `default_corpus_profiles`; `--about`/`--lang` help strings updated;
   `ShellFrontend { include_tests }` carries the toggle.
5. CI: slim-build line `cargo build -p fxrank --no-default-features --features shell` + a
   dogfood-scan line over committed shell fixtures.
6. Publishing (**release-time, out of scope for the feature branch** — see plan Task 13): at
   the next release, bump `version` in `[workspace.package]` and the internal dep pins, and add
   `fxrank-lang-shell` to the ordered publish list (before the `fxrank` binary) per `CLAUDE.md`.
   The new crate's internal pin tracks the current workspace version until then.

## 12. Verification bar

- **Fixtures** under `tests/fixtures/*.sh` (a subdir cargo won't compile), driven by an
  `analyze_fixture`-style helper; **insta** snapshots for output shape. Fixtures cover the
  acceptance sketch (functions, top-level commands, pipelines, command substitution,
  redirection, env mutation, destructive-command risks) **plus the review-driven cases**:
  the declaration-vs-hidden ladder (top-level `FOO=bar` = no effect vs a function's bare
  non-local write = `global.mutation`/6); subshell containment (`cd`/`x=` inside `$(…)`/`( )`/
  `&` does not escape, but a `curl` inside does); wrapper recursion (`sudo rm -rf` out-ranks
  bare `rm -rf`); the stream-filter rule (`grep pat f` = fs vs `… | grep pat` = none; `tr`
  never fs; `ls`/`find` always fs); input redirection `< f` as a read; `source` as an opaque
  effect + reach (literal-slash vs bare-name vs computed); assignment prefixes (`VAR=v cmd`
  scoped, `VAR=v` alone = assignment); `printf -v` as `global.mutation`/6 `hidden` (not
  `DynamicCode`).
- **RED→GREEN** per detector/decision (TDD).
- **Gates** (CI-enforced): `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, slim
  build `--features shell`.
- **Dogfood**: `fxrank scan` a real shell corpus (e.g. this repo's own `.sh` scripts / a
  dotfiles repo); IO/deploy boundaries should surface high, echo-only helpers low. Record
  intentional deltas the way `008-dogfood-deltas.md` did.

## 13. Acceptance criteria (from issue #15)

1. `fxrank scan script.sh` routes to the Shell frontend.
2. The report ranks destructive/network/deploy functions above echo-only helpers.
3. Top-level commands appear in a stable synthetic `<script>` unit.
4. Fixtures cover functions, top-level commands, pipelines, command substitution,
   redirection, env mutation, and destructive-command risks.
5. Existing Rust / TS / Python tests continue to pass; own-body output for the other
   frontends is unchanged.

## 14. Milestone-B deferrals (documented, not built now)

- Shebang-based routing for extension-less scripts (`#!/bin/bash`).
- `source` cross-file symbol resolution — following a sourced first-party file's symbols
  (Milestone A emits an opaque effect + reach at the `source` site instead).
- Alias tracking (`alias foo=bar` / `unalias`); `trap 'handler' SIG` deferred-callback modeling;
  `hash`/`enable`/`bind` shell-state mutation (treated as generic builtins in Milestone A).
- `xargs cmd` / `find … -exec cmd` argv recursion (wrapper recursion covers
  `sudo`/`su`/`doas`/`command`/`builtin`/`env`/`nice`/`nohup`/`exec` in Milestone A).
- Call-before-definition of a same-file function (`foo; foo(){…}` calls external `foo`) —
  Milestone A matches same-file functions by name regardless of order (§9).
- Precise `source` target resolution for relative-slash paths — shell resolves them against the
  runtime cwd (unknowable statically), so Milestone A emits a uniform `ThirdParty` best-effort
  reach; precise `FirstPartyOutOfScope` classification + confidence-graded reaches are M-B (§9).
- Downloaded-temp-file-then-execute detection (needs data-flow tracking; the pipe/substitution
  `curl|sh` forms *are* matched — §5).
- `shopt -s lastpipe` (last pipeline stage runs in the parent, not a subshell) — Milestone A
  treats every pipeline stage as a subshell.
- Dynamic-scoping precision (a function's bare write to a caller's `local`) — Milestone A folds
  this into `global.mutation`/6, which is score-equivalent, so precision is cosmetic.
- Nested-`function`-definition global-namespace effect is recorded coarsely (§8).
- `set +e` / missing-strict-mode metadata as low-confidence risk.
- Recording the external-tool dependency surface via `external_reaches`.
- `.ksh` / `.zsh` / `.bats` routing.

## 15. Shared-knowledge doc updates (part of the work)

Add a Shell **column/bullet** to each of: `docs/adding-a-language-frontend.md` (worked-example
mention), `docs/mutation-classification-guideline.md`, `docs/corpus-profile-guideline.md`,
`docs/cross-file-resolution-guideline.md`, and note the new `RiskKind`s in `CLAUDE.md`'s
vocabulary discussion. Preserve intentional per-language differences rather than "aligning"
them. Per-language **honesty notes** to record explicitly (so the next author doesn't "fix"
them):

- **Mutation guideline** — shell's declaration-vs-hidden line (§2/§7): top-level `FOO=bar` is a
  declaration (no effect); a function's bare non-local write is `global.mutation`/6. Subshell
  containment marks mutations inside `$(…)`/`( )`/`&`/pipeline stages as `contained`. Dynamic
  scoping is folded into `global.mutation` (score-equivalent). `global.mutation` (in-script
  shared state) and `env.write` (child-process environment) are **distinct axes**. **Shell is
  the first frontend to pair `hidden: true` with `global.mutation`** (for computed/indirect
  write targets like `printf -v "$var"`): it deliberately does **not** route these through the
  class-3 `hidden.mutation` kind — that would make an indirect write score *below* a plain
  named-but-undeclared one and invert the anti-Goodhart ordering. Preserve this; don't "align"
  it back to `hidden.mutation`. (`hidden` is evidentiary only — unread by scoring/fold.)
- **Corpus guideline** — shell test-skip is **file-name based only** (no in-file marker), and
  `skipped_tests` counts **1 per skipped file** (not per unit, since files aren't parsed) —
  a deliberate per-language divergence in that wire field.
- **Confidence scale** — shell's heuristic bases (0.9 for a literal recognized command) sit
  **above** core's `tier_base(Heuristic) = 0.6`, so in a mixed-language scan a literal shell
  signal reads as confident as a Rust `Path`-tier one. This is deliberate (a literal `rm` word
  is high-certainty despite no types) — record it so it isn't "normalized" to 0.6.
- **Cross-file guideline** — function-call resolution is **same-file only**, done via
  per-file-unique `canonical_path` + `resolved_target` (exact `CanonicalIndex` lookup), *not*
  the flat `SymbolIndex` — because shell reuses helper names (`log`/`die`/`main`/`usage`) across
  unrelated files far more than the typed languages, which would otherwise drop even same-file
  calls as ambiguous. `source` is a **path-keyed** opaque effect + reach, not a followed import;
  note that its opaque token is deliberately **`process.control`/6** (the sourced code runs
  *something*), which **diverges** from the guideline's standard opaque token
  (`external.unresolved`/2) — a `source` runs code in the current shell, so it is not a mere
  unresolved symbol. Do not "align" it away.
