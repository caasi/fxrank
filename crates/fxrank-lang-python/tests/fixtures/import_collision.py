# fxrank-fixture: #78 final-review delta — cross-function import collision.
#
# A top-level `from … import` binding and an UNRELATED function's own
# same-named function-local `import … as …` must resolve independently.
# Before #78's scope-aware resolution, the flat file-wide `Imports` table let
# g's local `CACHE` alias suppress/shadow f's top-level `CACHE` — this fixture
# pins the recovery: f's `CACHE.append(1)` must show as `global.mutation`
# (module-level `from`-import, escaping), unaffected by g's own local import.

from cfg import CACHE


def f():
    CACHE.append(1)


def g():
    import os as CACHE

    CACHE.remove("x")
