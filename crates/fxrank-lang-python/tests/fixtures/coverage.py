from typing import Any, cast

def fully_typed(x: int) -> list:
    acc = []
    acc.append(x)        # local.mutation, contained → discount to 0 under Full
    return acc

def has_any(x: Any) -> list:   # Any slot → can't reach Full + type.escape
    return []

def body_any(x: int) -> list:
    y = cast(Any, x)     # body Any → voids discount + type.escape
    acc = []
    acc.append(y)
    return acc

def untyped(x):          # None coverage → local.mutation NOT discounted (stays class 1)
    acc = []
    acc.append(x)
    return acc

def partial(x: int):     # Partial (param typed, return not) → class-1 local floors to 0
    acc = []
    acc.append(x)
    return acc

@some_unknown_wrapper    # unknown decorator → confidence reduced, coverage intact
def decorated(x: int) -> int:
    return x

def body_any_in_list(x: int) -> list:
    # cast(Any, …) inside a LIST literal → body Any (missed by the shallow walk)
    items = [cast(Any, x)]
    acc = []
    acc.append(items)
    return acc

def body_any_in_fstring(x: int) -> list:
    # cast(Any, …) inside an f-string interpolation → body Any
    s = f"{cast(Any, x)}"
    acc = []
    acc.append(s)
    return acc

def body_any_in_comprehension(xs: list) -> list:
    # cast(Any, …) inside a comprehension element → body Any
    ys = [cast(Any, x) for x in xs]
    acc = []
    acc.append(ys)
    return acc
