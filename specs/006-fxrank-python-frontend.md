# 006 — Python Frontend

## Goal

Add a Python language frontend (`fxrank-lang-python`, feature `python`) so
`fxrank scan` can profile the **own-body effect cost** of Python functions, alongside
the existing Rust and TS/JS frontends. Python is the next high-value dynamic-language
witness: effect-heavy glue code (CLIs, scripts, data pipelines, AI tooling, service
clients, notebook-derived modules, backend handlers) is exactly what an agent needs
ranked toward purer cores.

The thesis carries over from 003 and survives Python's gradual type system intact:
**explicit annotations bound a function's space of variation, so a typed boundary
discounts contained interior state — never world effects.** Python's optional typing
is the same situation as untyped JS (every value a shared mutable reference, nothing
declared) until annotations are written; `Any`, untyped `*args`/`**kwargs`, and opaque
decorators are the dynamic escape hatches that re-open the space (the `any ≈ unsafe`
analog).

Like Rust and TS (Milestone A), this frontend is **primarily syntactic**: it parses
with [`libcst`](https://github.com/Instagram/LibCST) and runs **no type checker**
(`mypy` / `pyright` are never invoked). Type-dependent signals are heuristic and carry
a confidence penalty. Only **explicitly-written annotations** are visible — inferred
types are invisible, the honest boundary of a syntactic instrument.

A measured prior shapes the whole design: real-world Python annotation adoption is
**~7–8% of args+returns** (Di Grazia et al., FSE 2022), *not* the self-reported ~88%.
So `BoundaryCoverage::None` is the common case and the boundary discount is a
**minority-case refinement** (FastAPI / Pydantic / dataclass code), not the hot path.
We do not over-invest in it.

## Scope

In scope (Milestone A, Python):

- A new `fxrank-lang-python` crate (feature `python`), mirroring `fxrank-lang-ts`'s
  structure, parsing `.py` files.
- The full effect / risk vocabulary mapping below — **reusing existing
  `EffectKind` / `RiskKind` only. No new core vocabulary this milestone** (parity-first;
  see *Decisions* and *Deferred*).
- The existing `fxrank-core` **boundary-containment discount** (`apply_boundary_discount`),
  fed by annotation-based `BoundaryCoverage` and a per-mutation `contained` flag. No new
  core scoring code — Python wires existing machinery.
- Syntactic **escape analysis** for mutation (local / parameter / `self` / `nonlocal`
  closure-capture / `global`), since "contained vs escaping" is what the discount turns
  on.
- **Fragment analysis from stdin** (`fxrank scan --lang python -`), scoring a single
  function in isolation.
- **Test-code skipping** by Python convention (path + function/class), with
  `--include-tests` to opt back in, counted in `skipped_tests` — identical contract to
  the Rust/TS frontends (spec 002).
- **Python corpus-hygiene defaults** added (interim) to the spec-004 `--exclude` union so
  a directory scan prunes `.venv` / `__pycache__` / build / cache noise (the
  frontend-owned `CorpusProfile` interface that should ultimately own these is deferred to
  #21).
- `async def` / `await` ride the existing `async_boundary` / `await_count` fields.

Out of scope (deferred — see *Deferred / Future work* for the full list):

- Any type resolution / `mypy` integration / aliasing proof.
- The **receiver-state gradient** from #14's design comment (private vs
  externally-reachable instance state, `@property`-getter suspicion, observer-vs-mutator
  method heuristics, `_private`-naming confidence shading). Milestone A treats `self.x =`
  as one escaping `this.mutation` tier; the gradient is a **dogfooding-informed**
  follow-up (its own spec) because it is genuinely new core vocabulary and its priors are
  unvalidated until we see real Python hotspots.
- New stdio vocabulary (`StdioRead`) and conditional-effect modeling — tracked in #20.
- Call-graph propagation / `inherited_score` (already deferred in 001).
- `.pyi` stub files (type-only, no bodies to score) — excluded by default.

## The core thesis: gradual typing still bounds *state*, not *world*

003's principle — *a type narrows the set of possible behaviors; a bounded behavior
set is a lower effect cost* — applies unchanged. Python only adds escape hatches:

- **Boundary-containable (state / memory):** mutation of a **locally-created** binding
  that does not escape. A typed in/out boundary genuinely bounds these (the `ST`-monad
  intuition).
- **Boundary-escaping (world effects):** filesystem, network, DB, `subprocess`, env
  read/write, `time` / `random`, logging, interactive `input()`. Observable to the
  world regardless of annotations. **Types never contain world effects** — and an
  annotated boundary must never hide `open`, subprocess, network, DB, env, filesystem,
  or deserialization effects.
- **Escaping state effects:** `self.x =` (receiver state), `nonlocal` (enclosing-env),
  `global`, and mutation of a **passed-in** parameter / collection (escapes via Python's
  pass-by-object-reference aliasing — the mutable-default-argument gotcha is the canonical
  teaching case). The in/out types do not bound these, so they are **not** discounted.

```python
# (1) typed boundary, effect stays internal → contained → discounts toward 0
def parse_config(raw: str) -> Config:
    acc: Config = {}
    for line in raw.splitlines():
        acc[k] = v          # local.mutation, never escapes
    return acc

# (2) identical-looking boundary, but mutates a passed-in dict → escapes → NOT discounted
def parse_config(raw: str, into: dict) -> None:
    into["x"] = raw         # param.mutation, caller sees it (aliasing)

# (3) typed boundary, but does IO → world effect → keeps full class
def parse_config(path: str) -> Config:
    return json.load(open(path))    # net.fs.db, escapes any boundary
```

## Effect vocabulary

**No new kinds.** Every Python signal maps to an existing `EffectKind` (003/001).
Wire strings come from `EffectKind::wire()` — never hand-written. Resolution uses the
`imports` table (`import x` / `import x as y` / `from m import n`).

### World effects (escape any boundary; never discounted by typing)

| kind | class | Python signal | tier |
| --- | --- | --- | --- |
| `net.fs.db` | 7 | **fs:** `open`, `pathlib.Path.read_text` / `write_text`, `os.*` fs ops, `shutil`, `tempfile`, `json.load` / `dump` (file-wrapped), `csv`, `pandas.read_csv` / `read_excel`. **net:** `requests.*`, `httpx.*`, `urllib`, `socket`, `aiohttp`. **db:** `sqlite3`, SQLAlchemy / ORM. Method-name writes: `session.commit()`, `.save()`, `.objects.create()`, `cursor.execute()`, `.to_csv()`, `.to_sql()` | path / heuristic |
| `process.control` | 6 | `subprocess.*`, `os.system`, `sys.exit` | path |
| `env.write` | 6 | `os.environ[...] = …`, `os.putenv`, `dotenv.load_dotenv()` (reads a `.env` file **and mutates `os.environ`** — the env-write footprint dominates) | heuristic |
| `concurrency` | 6 | `threading` / `multiprocessing` / `asyncio` primitives (locks, pools, `Process`) | heuristic |
| `time.read` | 5 | `time.time()` / `sleep`, `datetime.now()` / `today()` (easy to miss — *looks* like value construction) | path / heuristic |
| `random` | 5 | `random.*`, `secrets.*` | path |
| `env.read` | 4 | `os.getenv` / `os.environ.get`; **`input()`** (interactive stdin read) | heuristic / exact |
| `logging` | 4 | `logging.*`, `print()` | path / exact |
| `panic` | 4 | bare `assert` statement; `raise` | exact |
| `ambient.read` | 2 | `sys.argv`, `sys.platform`, module-level config reads | path |

`input()` has no stdio-specific kind, so it maps to `env.read` (class 4) as the
least-bad bucket under the no-new-vocabulary constraint — but its **evidence string
reads `input() — interactive stdin read`**, never a generic "environment read", so the
IO-boundary crossing stays honest. A dedicated `StdioRead` kind is the strongest case
for a future vocabulary addition (#20). `sys.argv` stays distinct at `ambient.read`
(class 2): process-invocation context, not live IO, and not the same as reading env/config.

`assert` is `panic` class 4 (Exact), mirroring the Rust frontend's `assert!`/`panic!`.
Because Python `assert` is **stripped under `python -O`**, its evidence string carries
`— stripped under -O` so an agent sees it is a *conditional* abort point. This
conditionality lives in the evidence text only; `confidence` is untouched (confidence
means *syntactic detection certainty*, a different axis from *runtime presence* — see
#20). Most `assert`s live in skipped test code; only non-test asserts (arg-validation)
score.

### State effects (boundary-containable)

| kind | class | Python signal | escapes? |
| --- | --- | --- | --- |
| `local.mutation` | 1 | `.append()` / `.add()` / `d[k] = …` / `.update()` / `+=` on a **locally-created** binding | no → contained, discounts to **0** |
| `param.mutation` | 3 | the same mutators targeting a **parameter** (passed-in list/dict/object, attribute or item) | yes → not discounted, **not** flagged `hidden` |
| `this.mutation` | 3 | `self.x = …` in a non-`__init__` method; `nonlocal x` write (enclosing-env, by the closure ≈ receiver isomorphism) | yes → declared, not hidden, not discounted |
| `global.mutation` | 6 | `global x` then write; module-level name rebind | yes → escapes by definition |

`self.x = …` is the honest `&mut self` analog: the receiver field is declared (not
`hidden`), but a method mutates an already-shared instance, so it **escapes**.
**`__init__` field initialization is `local.mutation`** — during construction `self` is
the freshly-allocated, not-yet-aliased instance, so the writes are contained (the
build-then-expose pattern, mirroring 003's constructor handling), hence
boundary-discountable. This `__init__`-vs-other-method split is a **Python-specific
escape rule local to the frontend** — it adds no core kind or scoring, just decides the
`contained` flag. We reuse the existing wire string `this.mutation` (language-neutral
"receiver-field mutation") for Python `self` rather than inventing `self.mutation`; as
with `input()`, the **evidence string carries the scope honesty** (e.g.
`self.x = … (instance state)`, `nonlocal x`), not the kind.

### Wrapper / inner-call attribution (architectural rule)

f-strings (`f"{datetime.now()}"`), **eager** comprehensions
(`[requests.get(u) for u in urls]`), and `with open(...) as f` are **pure wrappers
hosting effectful calls** that run **in the enclosing body**. The detectors must recurse
**into** f-string format expressions, list/set/dict-comprehension element and iterable
expressions, and `with`-items, attributing the *inner* effect — never letting the pure
construct mask detection.

**One deferral caveat — generator expressions are lazy.** A `(x for x in xs)` generator
expression evaluates **only its outermost iterable** at creation; the element and
filter (`if`) sub-expressions run when the generator is *consumed*, not in the enclosing
body. So the driver recurses into a genexp's outermost iterable (charged to the enclosing
fn) but **not** its element/condition body — which, unlike a `lambda`/nested-`def`, has
no unit of its own, so those deferred effects are simply **uncounted** (never charged to
the enclosing fn — see *Architecture*). List/set/dict comprehensions are eager (fully
charged); only the parenthesized generator form defers. (Free here because libcst
traversal is hand-rolled recursion; the spec makes it a required invariant, not an
accident.)

## Risk vocabulary

Reuse existing `RiskKind` (the danger channel, separate from effect cost). No new kinds.

| kind | class | Python signal | tier |
| --- | --- | --- | --- |
| `dynamic.code` | 7 | `eval` / `exec` / `compile`, `__import__` | exact |
| `dynamic.code` | 7 | `importlib.import_module`, `pickle.load` / `loads`, unsafe `yaml.load` (not `safe_load`) | path |
| `dynamic.code` | 7 | monkey-patching (`setattr` on an imported module / class) | heuristic |
| `dynamic.code` | 7 | `subprocess(..., shell=True)` (shell-injection; **also** emits a `process.control` effect, class 6) | path |
| `type.escape` | 3 | an explicit `Any` (signature slot, `cast(Any, …)`, or `Any` local) — the `any ≈ unsafe` escape hatch; gentle so it doesn't shadow real IO | exact |

`eval` / `exec` / `compile` / `__import__` are bare builtin names recognized
syntactically → **Exact**, exactly as the TS frontend treats `eval` Exact (a builtin
*could* be shadowed; we accept that, as TS does). Import-resolved forms (`pickle`,
`yaml`, `importlib`) are **Path**; ambiguous monkey-patching is **Heuristic**. Ordinary
`getattr` / `setattr` *attribute* access (dynamic dispatch, JSON→object mapping) is **not**
code execution and is **not** flagged — only `setattr` re-binding on an imported module /
class (monkey-patching) is.
`subprocess(..., shell=True)` emits its `process.control` effect (class 6) **plus** a
`dynamic.code` (class 7) risk — a shell string is arbitrary command execution via
`/bin/sh`, the same code-execution channel as `eval`, so it reuses the existing risk kind
(no new vocabulary), with evidence `subprocess(shell=True) — shell-injection surface`.
Only unsafe `yaml.load` is flagged (the taught default `safe_load` is not).

## The boundary-containment discount

Python wires the existing `fxrank-core` machinery (`BoundaryCoverage`,
`apply_boundary_discount`, `score.rs`). No new scoring code.

### Signature coverage (`coverage.rs`)

Let a function's signature have `S` **slots** = (one per parameter) + (one return slot).
A slot is **typed** iff it carries an *explicit* annotation whose top-level type is not
`Any`. `t` = typed slots; coverage tiers `None` (`t = 0`), `Partial` (`0 < t < S`),
`Full` (`t = S`). Two structural rules:

- **`self` / `cls` are excluded from the slot count.** Convention never annotates them;
  counting them would pin every method at `Partial` forever (the TS method-receiver
  treatment).
- **`*args` / `**kwargs`**: each is **exactly one slot** (so `def f(*args: int,
  **kw: str) -> int` has `S = 3`: `*args`, `**kwargs`, return). A typed star-param
  (`*args: int`) counts as a typed slot; **untyped counts as an untyped slot** (degrades
  coverage) — the escape-hatch rule.

### Poison & confidence rules

- **`Any` annotation** — parallel to 003's `any`-poison rule, with two precise cases:
  - In a **signature slot** (`x: Any`, `-> Any`) → that slot is **untyped** (can never
    reach `Full`); an `Any`-typed boundary is a non-boundary.
  - In the **body** (`cast(Any, …)`, an `Any`-annotated local) → there is no signature
    slot to degrade, so it **voids the boundary discount entirely** (the shift is forced
    to 0 regardless of slot coverage). `Any` anywhere re-opens the space of variation, so
    a boundary that casts its way out cannot be trusted to contain anything — exactly
    003's body-`any` poison.
  - In **both** cases, an explicit `Any` also **emits a `type.escape` risk** (existing
    `RiskKind`, class 3 — no new vocabulary), mirroring 003's full any-poison (which voids
    the discount *and* flags `type.escape`). Deliberately gentle (class 3): `Any` is
    surfaced as "look here, the space of variation is out of control" without shadowing a
    real IO effect (class 7). Implicit/absent annotations are **not** `type.escape` — only
    the explicit `Any` escape hatch is.
- **Unknown / dynamic decorators** → lower the function's **confidence** (weakest-link
  min), **not** its coverage. Rationale (settled against an adversarial review): Python's
  static type system only preserves a decorated function's signature when the decorator
  is *type-preserving* (`Callable[[F], F]`, PEP 612 `ParamSpec`, typed `functools.wraps`);
  an untyped decorator erases it to `Any`. But fxrank runs **no** type checker and cannot
  tell syntactically which it is — and the written annotations are real signal ("input
  types are still better than no types"). So we **keep the written annotations counting
  toward coverage** (graceful `Partial`/`Full`, never collapsed to `None`) and flag
  reduced confidence: "typed, but a wrapper may be lying." A known-pure allowlist
  (`@property`, `@staticmethod`, `@classmethod`, `@dataclass`, `@functools.wraps`,
  framework route decorators) does **not** penalize.

### Contained map (what's discountable)

Per the hard rule *"type hints discount contained local state, never world effects"*:

| effect | `contained`? | discountable |
| --- | --- | --- |
| `local.mutation` (true local) | **true** | yes → any `coverage > None` floors class 1 → **0** |
| `param.mutation`, `this.mutation` (incl. `nonlocal`), `global.mutation` | false (escaping) | no |
| every world effect + every risk | false / n/a | no |

### Honest scope statement

In Milestone A the **only** discountable effect is `local.mutation`, so the boundary
discount does exactly one thing: **a function with at least one typed slot — even
partially typed — whose only effect is local collection-building scores 0 instead of 1**
(class 1 floors to 0 at the first discount step, so `Partial` and `Full` are identical
here; only `None` vs not-`None` separates). Given ~7–8% real typing, this fires rarely
— but it is correct where it fires, and it installs the `contained`/escaping
classification that the deferred #14 receiver-state gradient will extend (where
honestly-typed `self.x =` could become *partially* containable). The graduated
Partial-vs-Full depth is latent here (every contained effect is class 1, floored at 0 by
both) and is exercised by a `fxrank-core` unit test on a synthetic class-≥2 input, as in
003 — no Python fixture can reach it.

## Architecture (mirrors `fxrank-lang-ts`)

- **New crate `fxrank-lang-python`**, behind feature `python`. It **depends on no parser
  type in `fxrank-core`** — `libcst` must never leak into core, exactly as `syn` / `swc`
  must not. The compiler enforces it.
- **Parser: `libcst` 1.8.6**, depended on with **`default-features = false`** (the
  default `py` feature pulls in PyO3 `extension-module` and breaks a normal binary's
  linkage; with `py` off the dep graph is **pure Rust** — no C toolchain, unlike
  tree-sitter). Entry point `parse_module(source, None) -> Module`.
- **Two divergences from `syn` / `swc`, called out so they are designed, not discovered:**
  1. **No visitor trait.** libcst exposes no `Visit`; detectors **manually match on the
     node enums** (`Statement` → `FunctionDef` / `ClassDef` / `Assign` / `AnnAssign` /
     `AugAssign` / `Global` / `Nonlocal` / `Import` / …; `Expression` → `Call` /
     `Attribute` / `Subscript` / `Name` / `Lambda` / `Await` / …) and recurse explicitly.
     A small shared recursion driver in `detect/` walks children so each detector follows
     the `classify_* → push` shape without re-implementing the walk. **A nested `def`
     body and a `Lambda` body are deferred-execution boundaries the driver does NOT
     descend into:** defining them does not run them, so charging their inner effects to
     the enclosing function would violate own-body attribution (a nested `def` and a
     `lambda` are each **their own unit** — their body effects are scored on *that* unit,
     never rolled into the parent). **But
     definition-time expressions that are EAGERLY evaluated in the enclosing body, and the
     driver MUST recurse into them for effect attribution:** a nested `def`'s
     **decorators** and **parameter default values** run when the `def` is defined (all
     Python versions — defaults are the root of the mutable-default-argument gotcha; a
     `lambda` has **only** parameter defaults — no decorators in Python), so
     `def inner(x=open(path)):` charges `open(path)` to the *enclosing* function, and
     `@app.route(...)` / `lambda x=datetime.now(): …` charge their decorator/default
     effects there too. **Annotation expressions are the exception — NOT charged as
     effects:** modern Python evaluates annotations lazily (PEP 649, default in 3.14) or
     stringizes them under `from __future__ import annotations`, so an annotation
     expression does not run at def-time; the frontend inspects annotations **only
     syntactically** (slot coverage + `Any` detection), never recursing into them for
     runtime effect attribution. (In the *legacy* eager-annotation regime a param/return
     annotation does evaluate at def-time, but the frontend — being syntactic — cannot
     know a file's annotation regime any more than it knows `-O`, so it uniformly declines
     to charge annotation effects: the honest syntactic default, knowingly under-charging
     the rare pathological case, same principle as `assert`/`-O`.) The inner callable's *body* (run when it is *called*) is
     likewise skipped. This is consistent with
     the wrapper-attribution rule below: f-strings, **eager** comprehensions, and
     `with`-items also evaluate in the enclosing body and are descended into — while a
     **lazy generator-expression** element body is *not* charged to the enclosing function
     (deferred execution; unlike a lambda there is no separate unit, so it is simply
     uncounted — see that section's genexp caveat).
  2. **Position-less nodes.** libcst's typed nodes carry **no** line/col (positions live
     only on the `tokenize()` token stream). fxrank's `id = path:line:col:symbol` (spec
     005) needs each unit's anchor 1-based **char** line:col. Two anchor paths:
     - **Named units** (`def` / `async def` / methods / nested `def`): a `FunctionDef`'s
       `name.value` is a `&str` **borrowed from the original source buffer**, so
       `name.value.as_ptr() − source.as_ptr()` yields the exact byte offset directly,
       converted to 1-based char line:col via a precomputed line-start index. Cheap, no
       token stream.
     - **Anonymous `lambda` units:** a `Lambda` has no `name.value`, and the
       `lambda`-keyword token is crate-private, so the pointer-trick can't anchor it
       directly. Use the **`tokenize()` token stream** (every `Token` carries
       `line_number()` 1-based and `char_column_number()` 0-based char col). `lambda` is a
       **reserved keyword used only in lambda expressions**, so the `lambda`-keyword tokens
       (in source order) stand in **exact 1:1 correspondence with `Lambda` CST nodes
       visited in pre-order** — the *k*-th `lambda` token anchors the *k*-th `Lambda` node
       (pre-order visits a node before its body, so source order and traversal order agree,
       including for nested lambdas and empty-body lambdas like `lambda: []`). This ordinal
       bijection is robust and needs **no** inner-`&str` guide — it is **not** the fragile
       whole-tree token↔node alignment (which tries to match *arbitrary* tokens to nodes);
       it matches a single keyword kind that already has a clean bijection. The anchor is
       that token's `line_number()` and `char_column_number() + 1` (0-based → 1-based char
       col, matching spec 005), yielding symbol `<lambda@L{line}C{col}>`, mirroring the TS
       frontend's `<arrow@L…C…>`. The `C{col}` suffix disambiguates two lambdas on one
       line, exactly as in TS.

     **(Both anchor paths — the borrowed-subslice pointer-trick and the lambda token
     lookup — must be verified at the review gate, see below.)**
- **Modules mirror the TS frontend:** `functions` (collect `FnUnit`s: `def`, `async def`,
  methods, nested functions, and anonymous `lambda`s. Named units anchor via the
  `name.value` pointer-trick; lambdas anchor via the `tokenize()` lambda-keyword lookup
  and get the synthesized symbol `<lambda@L{line}C{col}>` — both per *Position-less
  nodes* above. A `lambda` cannot be annotated in Python syntax, so its `BoundaryCoverage`
  is always `None`; a pure `lambda x: x*2` scores 0 and falls off `Report::build`'s
  limit, so collecting every lambda does not flood the output),
  `imports` (the import table), `coverage` (annotation slot coverage), and
  `detect/{calls, mutation, risk}` orchestrated by `detect::analyze_unit` — **the single
  owner** of turning effects / risks / coverage into a scored `Hotspot`. Detectors stay
  pure (`Vec<Effect>` + per-mutation `contained` flag); assembly, coverage, and the
  boundary shift live in `analyze_unit`.
- **`async` reuses existing fields:** `analyze_unit` sets `async_boundary` from
  `async def` or any `await`, and `await_count` from awaited expressions — identical to
  Rust/TS. `async` / `await` are **not** effects.
- **Borrowed AST (lifetime, unlike `syn`/`swc`):** libcst's inflated tree is
  **`Module<'a>`, borrowing `&'a str` slices from the source buffer** — it is *not*
  owned like swc's/`syn`'s. (This borrow is exactly what makes the position pointer-trick
  above sound: `name.value` points into the live source.) So `FnUnit` cannot retain an
  owned body the way the Rust/TS frontends do. Resolution: **collection and analysis run
  in a single borrowed pass** while the `Module` and its source string are both alive —
  `analyze_unit` consumes the borrowed nodes and emits **owned** `Hotspot`s (positions,
  effects, evidence are all extracted eagerly), so nothing borrowed outlives the pass and
  no `'a` lifetime threads into `core`. `FnUnit` holds borrowed node references valid only
  within that pass, not across it.

### Parser off-ramp

libcst is the choice for maintainability (typed, pure-Rust, no vendoring), accepting the
position-reconstruction cost. If the borrowed-subslice position trick fails to hold for
**named** units, or the PyO3 linkage fights the build during implementation, the documented
fallback is **`tree-sitter-python`** (positions free on every node; cost: untyped
`node.kind()` detectors + a C toolchain in CI). If only the **`lambda` token anchor** is
problematic, the narrower fallback is to defer anonymous lambdas (named units still work)
— not a parser switch. The fallback is recorded so a mid-implementation switch
is a known off-ramp, not a surprise.

### CLI

- File discovery gains `.py`; dispatch is feature-gated on `python` exactly as Rust is on
  `rust` and TS on `ts`. `.pyi` stubs are excluded by default (no bodies to score).
- **Fragment mode:** a path of `-` reads stdin; `--lang python` selects the frontend for
  stdin (no extension to infer). `--lang python` is the single value — Python has one
  source dialect, and the CLI's `--lang` is one-value-per-dialect (no alias mechanism; TS
  `ts`/`tsx`/`js`/`jsx` are distinct dialects, not synonyms). For file/dir paths the
  extension wins and `--lang` is rejected, as today.

### Corpus hygiene for Python (`--exclude` defaults)

The spec-004 default `--exclude` list is JS/TS-flavored (`node_modules`, `*.min.js`,
Storybook, `jest.*`, `__mocks__`, MSW). Because the directory walk prunes **before**
per-file language is known, the default must be a **union** of every ecosystem's noise.
Milestone A therefore **adds the Python entries to that default list** so a
`fxrank scan <pyproject>/` does not drown in installed third-party code (Python's
`node_modules`-scale problem is `.venv/.../site-packages/`):

- **Directory prunes** (base-name literals): `.venv`, `venv`, `.tox`, `.nox`,
  `__pycache__`, `.eggs`, `build`, `dist`, `.mypy_cache`, `.pytest_cache`, `.ruff_cache`,
  `site-packages`. (The existing JS `node_modules` stays — Python projects often carry a
  JS frontend.)
- **File globs** (files only): `*_pb2.py`, `*_pb2_grpc.py` — generated protobuf code, the
  Python analog of `*.min.js`.
- **Deliberately NOT in the default:** bare `env` / `.env` (collides with a `.env`
  *dotenv file* and legit packages named `env` — rely on `.venv`/`venv`); `migrations/`
  (effect-heavy but a legit module name, too project-specific); `setup.py` / `manage.py`
  (real effectful entry points — keep). Arbitrarily-named venvs are missed by name
  literals; a robust `pyvenv.cfg` content-marker prune is deferred (#21).

**Replace semantics still apply (spec 004).** Adding these entries grows the *default*
union, but `--exclude` still **replaces** the whole list when given — a caller adding one
custom pattern must restate the entire (now larger, cross-ecosystem) union. So the
documented default — the clap `default_value` string printed verbatim in `--help` (spec
004's copy-and-extend affordance) — **must be updated to include these Python entries**;
otherwise a user cannot reconstruct the union to extend it. (The ergonomic fix, an
append-mode `--exclude-add`, is deferred to #21.)

**This is an interim placement.** Per-ecosystem exclude/test defaults are *ecosystem
knowledge* and should be **frontend-owned**, not a central CLI list. The unifying
`CorpusProfile` interface (each `Frontend` declares its prunes / file globs / test-file
globs; the CLI unions enabled frontends; source-based test detection stays internal) is
tracked in **#21**; when it lands, these Python entries move out of the central list into
`fxrank-lang-python`'s profile.

### `--exclude` vs `--include-tests` (distinct mechanisms)

Two non-overlapping concerns, each with its own counter (per spec 004/002):

| mechanism | owns | counter | toggled by `--include-tests`? |
| --- | --- | --- | --- |
| `--exclude` (CLI discovery) | vendored / generated / cache: `.venv`, `__pycache__`, `build`, `*_pb2.py`, … | `skipped_excluded` | **no** |
| frontend test-skip | `tests/`, `test_*.py`, `*_test.py`, `conftest.py`, `Test*`, `unittest.TestCase` methods | `skipped_tests` | **yes** |

When the two could overlap, the spec-004 rule holds: **discovery-exclude wins**. A
load-bearing consequence: **`--include-tests` re-includes only the project's *own* test
code — never third-party test suites inside `.venv/.../site-packages/*/tests/`**, because
those are pruned by `--exclude` (a different mechanism). So a Python user passing
`--include-tests` is not suddenly flooded by numpy/pandas test code.

## Output schema

**No structural change** to `Report` / `Scope` / `Hotspot` / `Summary`, and **no new
kind strings** (Python reuses existing `EffectKind` / `RiskKind` wire values). The only
Python-specific surface is **richer evidence strings** (`input() — interactive stdin
read`, `assert — stripped under -O`, `subprocess(shell=True)`), plus the existing
`discounted_to` / `discount` fields carrying boundary rationale. `async_boundary` /
`await_count` populate as in 001/003. Per-effect `confidence` is still not serialized
(function-level only).

## Error handling

Mirrors 001/002/003: an un-parseable file or fragment becomes a `diagnostic` with
`parsed: false`, never a panic. A stdin fragment parses as far as libcst allows. `--lang`
is a plain enum flag.

## Detectability & confidence

- **`exact`:** `eval` / `exec` / `compile` / `__import__`, `assert`, `raise`, `input()`,
  `print()`, and explicit `Any` / `cast(Any, …)` (→ `type.escape`) — bare builtin names /
  statements / syntactic tokens recognized without import resolution.
- **`path`:** a call resolved through the `imports` table to a known module/member
  (`requests.get`, `subprocess.run`, `os.getenv`, `random.random`, `pickle.load`).
- **`heuristic`:** method-name signals (`session.commit()`, `.save()`,
  `cursor.execute()`, `.to_sql()` → `net.fs.db`; `os.environ` access; `datetime.now`),
  monkey-patching, the boundary discount itself (we *trust* annotations; no `mypy`), and
  any signal needing type info we do not have. Unknown decorators apply an additional
  confidence penalty.

A fragment scored in isolation has no surrounding `imports` table, so call resolution
degrades to heuristic and confidence drops — consistent with "we report only what is
syntactically visible."

## Testing strategy

Mirrors the other frontends: `tests/fixtures/*.py` read by a shared
`analyze_fixture(name)` helper (a subdir cargo does not compile as test targets);
`insta` snapshots for whole-report shape. Coverage must include:

- **World vs state:** `requests.get` / `open` keeps `net.fs.db` class 7 even behind a
  fully-typed boundary; a function with only local mutation behind a fully-typed boundary
  scores 0.
- **Typed vs untyped boundaries:** the same locally-mutating function at `None` (stays
  class 1), `Partial`, and `Full` (both floor to 0); `self` / `cls` excluded from slot
  counting; untyped `*args`/`**kwargs` degrading coverage.
- **`Any` poison (both cases):** a signature with one `Any` slot cannot reach `Full`;
  **and** a fully-typed-signature function with a body `cast(Any, …)` / `Any` local has
  its discount voided entirely — a typed local-mutation function that *would* score 0
  now stays class 1. **Both cases also emit a `type.escape` risk** (class 3).
- **Decorator confidence:** `@property` / `@dataclass` (allowlist, no penalty) vs an
  unknown decorator (confidence reduced, coverage intact).
- **Escape discrimination:** local mutation (contained, discounts) vs `param.mutation` /
  `self.x =` / `nonlocal` / `global` (escaping, no discount); `__init__` field init
  classified `local.mutation`.
- **Wrapper attribution:** `with open(...)`, `f"{datetime.now()}"`, and the eager
  `[requests.get(u) for u in urls]` all surface their inner effect; a **lazy**
  `(requests.get(u) for u in urls)` genexp charges only its outermost iterable, **not**
  the deferred element body; a nested-`def` parameter default `def f(x=open(p)):` and a
  decorator `@route(...)` charge to the enclosing function, while the nested body does
  not.
- **Risk kinds:** `eval` → `dynamic.code` exact; `pickle.load` → `dynamic.code` path;
  `subprocess(shell=True)` → `process.control` effect + `dynamic.code` risk.
- **Method-name DB/file writes:** `session.commit()` / `.to_csv()` → `net.fs.db`
  heuristic.
- **`input()` / `sys.argv` / `assert`:** correct kinds, classes, and honest evidence
  strings (incl. `assert` `-O` note).
- **async:** `async def` / `await` sets `async_boundary` / `await_count`, emits no effect
  of its own.
- **Function forms:** `def` / method / nested `def` / `lambda` collected as units; a pure
  nested helper and a pure `lambda x: x*2` both score 0; a `lambda` gets symbol
  `<lambda@L…C…>` from the token-stream anchor; two lambdas on one line get distinct
  `C{col}` suffixes; a `lambda: requests.get(u)` scores its body effect on the lambda's
  own unit (not the enclosing fn).
- **Lambda anchoring edge cases:** an empty-body `lambda: []` (no inner `&str`) and a
  **nested** `lambda x: (lambda y: y)` both anchor correctly via the *k*-th-token ↔
  *k*-th-node ordinal bijection — the outer and inner each get the right `line:col`.
- **Test skipping:** `test_*.py` / `*_test.py` / `conftest.py` / `tests/` dirs skipped;
  `test_*` functions, `Test*` classes, `unittest.TestCase` methods skipped; counted in
  `skipped_tests`; `--include-tests` opts back in.
- **Corpus hygiene:** in a scan tree, `.venv/` / `__pycache__/` / `build/` dirs are
  **pruned and not counted**, while a `*_pb2.py` file is **excluded and counted** in
  `skipped_excluded` (the dir-prune-vs-file-exclude counting asymmetry, per spec 004);
  `--include-tests` does **not** pull in a test file inside a pruned
  `.venv/.../site-packages/…` tree (never walked).
- **Fragment mode:** `echo '<fn>' | fxrank scan --lang python -` scores one function at
  lower confidence than in-file.
- **Slim builds:** `--no-default-features --features python` compiles, plus `--features
  rust` / `--features ts` (feature-gate hygiene), matching CI's slim-build gates.

## Verification

- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --check` green.
- `cargo build -p fxrank --no-default-features --features python` (slim Python build) and
  `--features rust` / `--features ts` all compile.
- Dogfood: scan a small typed-Python fixture tree and confirm world-effect hotspots
  surface while contained-mutation functions sink; confirm `self.x =` is **not**
  discounted; confirm test files are skipped by default.

## Review gate (blocking, before implementation)

The parser decision and positioning approach rest on agent web-research that can be stale
or wrong. Before the spec is approved / implementation starts, **Copilot (via the
review-loop skill's Copilot pass, once a PR exists) must independently re-confirm the
libcst API claims:**

1. exact crate name + latest crates.io version (`libcst = "1.8.6"`);
2. it is **pure Rust** with `default-features = false` (no C / Python build dependency);
3. **how to obtain 1-based char line:col for a unit's anchor**, for **both** anchor paths:
   (a) **named** units — that `FunctionDef.name.value` is a borrowed subslice of the
   source buffer so pointer arithmetic yields a valid byte offset; (b) **`lambda`** units
   — that `tokenize()` exposes per-`Token` `line_number()` / `char_column_number()`, and
   that `lambda`-keyword tokens are in 1:1 source-order correspondence with `Lambda`
   nodes (pre-order), so the *k*-th token anchors the *k*-th node → `<lambda@L…C…>`
   (with `+1` on the 0-based col).

If (3a) turns out unavailable or unreliable, the spec switches to the
**tree-sitter-python off-ramp** *before* any implementation. If only (3b) is problematic,
the narrower fallback is to **defer anonymous `lambda` units** (named units still anchor
via 3a) rather than switch parsers. This item is blocking, not advisory.

## Decisions

| Decision | Choice | Rationale |
| --- | --- | --- |
| Parser | `libcst` 1.8.6, `default-features = false` | Crates.io-publishable (Ruff's parser is git-only → would make `fxrank` unpublishable); pure Rust (no C dep, unlike tree-sitter); typed nodes give compile-safe detectors; actively maintained (Meta). Speed is ample for one-shot scans. |
| New core vocabulary | None (parity-first) | Every Python signal maps to an existing kind. Genuine gaps (stdio, conditional effects) are tracked in #20 for a deliberate later vocabulary milestone, not smuggled in. |
| Position reconstruction | named units: byte offset from the borrowed `name.value` `&str` subslice → line:col; `lambda` units: *k*-th `tokenize()` lambda-keyword token ↔ *k*-th `Lambda` node (pre-order) → `<lambda@L…C…>` | libcst nodes are position-less. The pointer-trick is exact for named units; lambdas (no name slice) use the robust ordinal bijection of the `lambda` keyword (reserved, 1:1 with `Lambda` nodes — not fragile whole-tree alignment), matching the TS `<arrow@L…C…>` convention. If only the lambda anchor proves unworkable, defer lambdas (narrower fallback, not a parser switch). Both verified at the review gate. |
| `input()` | `env.read` class 4, Exact, evidence `interactive stdin read` | No stdio kind exists; least-bad bucket, but the evidence label keeps the IO-boundary honest. `StdioRead` deferred to #20. |
| `sys.argv` | `ambient.read` class 2 | Process-invocation context, not live IO; kept distinct from env/config reads. |
| `assert` | `panic` class 4, Exact, evidence notes `stripped under -O` | Rust parity (`assert!` = panic); `-O` conditionality lives in evidence text, not confidence (confidence = detection certainty, a different axis — #20). |
| `self.x =` | `this.mutation` class 3, escaping (reuse existing wire string) | Honest `&mut self` analog: declared but shared-instance mutation escapes. `__init__` init is `local.mutation` (contained). Receiver-state gradient deferred to #14. |
| `nonlocal` | `this.mutation` class 3 | Closure ≈ receiver isomorphism (#14 comment): enclosing-env mutation, above local, below global. |
| Decorators | Unknown decorators reduce **confidence**, not coverage; pure-decorator allowlist exempt | fxrank can't tell syntactically whether a decorator is type-preserving; written annotations are real signal, so degrade gracefully + flag risk. |
| `*args` / `**kwargs`, `Any`, `self`/`cls` | untyped `*args`/`**kwargs` degrade coverage; signature `Any` poisons its slot, body `Any` voids the discount, **both emit `type.escape`(3)**; `self`/`cls` excluded from slots | Honest gradual-typing model; matches 003's `any`-poison (discount-void **and** `type.escape` risk) and method-receiver handling. Reuses existing `RiskKind`. |
| Test skipping | path (`test_*.py` / `*_test.py` / `conftest.py` / `tests/`) **and** function/class (`test_*`, `Test*`, `unittest.TestCase` methods); `--include-tests` opts in | Python's conventions are strong and near-universal; avoids re-living the Rust bare-`#[cfg(test)] fn` caveat. Same override as other frontends. |
| Wrapper attribution | recurse into f-strings / comprehensions / `with`-items | Else the highest-frequency effects (file IO via `with open`, calls in comprehensions) are systematically under-counted. |
| Corpus-hygiene defaults | **interim**: add Python noise (`.venv`/`venv`/`.tox`/`__pycache__`/`build`/`dist`/cache dirs/`site-packages` prunes; `*_pb2.py` globs) to the spec-004 union default; update the `--help` `default_value` string verbatim to match | The walk prunes before language is known, so the default must union every ecosystem's noise; else a Python scan drowns in `site-packages`. Replace semantics (spec 004) mean the enlarged union must be documented so callers can copy-and-extend. Frontend-owned `CorpusProfile` interface deferred to #21 — entries move there when it lands. |
| `--exclude` vs `--include-tests` | distinct mechanisms, distinct counters (`skipped_excluded` vs `skipped_tests`); discovery-exclude wins on overlap | `--include-tests` re-includes only the project's own tests, never third-party tests under a pruned `.venv` — they're a different mechanism. |
| async | Flag only (`async_boundary` / `await_count`) | Identical to Rust/TS; effects come from the body. |

## Deferred / Future work

1. **Receiver-state gradient (#14 design comment)** — the ordering `local <
   private-receiver < externally-reachable-receiver < class/global < world`, plus
   `@property`-getter suspicion (observation boundary that hides a write scores *up*),
   observer-vs-mutator method-name heuristics, `_private`-naming confidence shading, and
   class-attribute / module-global tiering. **New core vocabulary** + unvalidated priors
   → its own **dogfooding-informed** spec.
2. **Stdio vocabulary (`StdioRead` / stdio-write) and conditional-effect modeling** —
   tracked in #20; would let `input()` / `print()` and `-O`-conditional `assert` be
   labeled first-class instead of via evidence strings.
2a. **Frontend-owned corpus / test-pattern interface (`CorpusProfile`)** — tracked in
   **#21**: move the per-ecosystem `--exclude` defaults and test-file globs out of the
   central CLI list into each frontend (CLI unions enabled frontends; source-based test
   detection stays internal). Milestone A places Python's defaults in the central list as
   an interim step; they migrate to `fxrank-lang-python`'s profile when #21 lands. Related:
   a `pyvenv.cfg` content-marker prune (catches arbitrarily-named venvs) and an
   `--exclude-add` (append vs replace) flag.
3. **Type-preserving decorator recognition** — credit PEP 612 `ParamSpec` /
   `Callable[[F], F]` / typed `functools.wraps` decorators to *restore* confidence
   (Milestone A penalizes all unknown decorators uniformly).
4. **Bare module-level test helpers** — the Python analog of the Rust caveat: a
   `test_*`-named helper outside a test file/class. Milestone A skips by name conventions;
   tighten if dogfooding shows leakage.
5. **async as a weak IO prior** — `async def` strongly implies an IO-bound context;
   left as a flag only this milestone rather than feeding score.
6. **Namespace / attribute call-resolution depth** — `import os.path as p; p.exists()`
   member resolution through the import table (003's namespace-import deferral analog).
7. **DB / ORM method-name allowlist** — `.commit()` / `.save()` / `.execute()` are
   heuristic now; a small known-client allowlist if false positives appear.
8. **Call-graph propagation / `inherited_score`** (already deferred in 001).
9. **`csv` / `pandas` / `sqlite3` depth** — only the common entry points (`read_csv`,
   `to_sql`) are caught now; broaden with real scans.

## Open questions

- `input()` is settled as `env.read` (4) for Milestone A (see *Decisions*); the open
  item is only the **future** calibration of a dedicated `StdioRead` class once that
  vocabulary lands (#20) — tune with real scans, not speculation.
- `secrets` vs `random`: both → `random` class 5 now; a security-sensitivity split may
  warrant a risk flag later.
- Whether `nonlocal` deserves a tier between `local` and `this.mutation` once the #14
  gradient lands.
