# Fixture for genexp await-counting regression (FIX B).
#
# A generator-expression is LAZY — the element body, `if` conditions, and nested
# `for` clauses are not evaluated in the enclosing scope. Only the OUTERMOST
# iterable is eager. Awaits in the lazy parts must NOT count toward the enclosing
# function's `await_count` / `async_boundary`.
#
# The concrete bug: old code used `count_in_comp_for(&g.for_in)` which traversed
# the `if` conditions and nested `for` clauses inside the genexp — those are lazy
# and should not be counted. The fix uses `count_in_expr(&g.for_in.iter)` (only
# the outermost iterable).
#
# Contrast with a list-comprehension: its `if` conditions and element ARE eager
# and their awaits DO count.

import asyncio


async def predicate(x):
    return x > 0


async def get_items():
    return [1, 2, 3]


async def genexp_await_in_if_condition(xs):
    # `await predicate(x)` is in the genexp IF condition — lazy, NOT counted.
    # await_count for this function must be 0.
    gen = (x for x in xs if await predicate(x))  # noqa: PLE1142
    return gen


async def listcomp_await_in_if_condition(xs):
    # `await predicate(x)` is in a list-comp IF condition — eager, IS counted.
    # await_count for this function must be >= 1.
    result = [x for x in xs if await predicate(x)]
    return result


async def genexp_await_in_nested_for_iterable(xs):
    # `await get_items()` is in a NESTED for clause's iterable inside a genexp.
    # Nested-for iterables in a genexp are also lazy — NOT counted.
    # await_count for this function must be 0.
    gen = (y for x in xs for y in await get_items())  # noqa: PLE1142
    return gen


async def genexp_await_in_outermost_iterable(xs):
    # `await get_items()` is in the OUTERMOST iterable — eager, IS counted.
    # await_count for this function must be >= 1.
    gen = (x for x in await get_items())
    return gen
