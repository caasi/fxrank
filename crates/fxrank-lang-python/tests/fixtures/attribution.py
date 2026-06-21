# fxrank-fixture: def-header attribution (decorators + param defaults) and
# eager subscript index evaluation.

import requests


def outer(p):
    # `open(p)` is a parameter DEFAULT of the nested `def inner` — it runs when
    # `outer()` executes (def-time of inner) → charged to OUTER, not inner.
    def inner(x=open(p)):
        return x
    return inner


def top_default(x=open("f")):
    # The default `open("f")` runs at MODULE time (when `top_default` is defined),
    # NOT when `top_default` is called → uncounted on `top_default` itself.
    return x


def subscript_index(xs, u):
    # The index expression `requests.get(u)` is eagerly evaluated → NetFsDb on
    # THIS function.
    return xs[requests.get(u)]


def assign_target_subscript_index(xs, u):
    # The index expression `requests.get(u)` of an ASSIGNMENT TARGET is eagerly
    # evaluated → NetFsDb on THIS function. The mutation of `xs` (param) is a
    # SEPARATE, single effect — the index walk must NOT double-count it.
    xs[requests.get(u)] = 1


def assign_target_attr_base(u):
    # `requests.get(u).attr = 1` — the attribute target's base call
    # `requests.get(u)` is eagerly evaluated → NetFsDb on THIS function.
    requests.get(u).attr = 1
