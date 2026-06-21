# fxrank dogfood fixture — exercises all headline scoring cases in one file.
#
# Functions designed to test:
#   io_world    — net.fs.db (class 7), world-effect boundary
#   typed_local — fully-typed local mutation → discounted to class 0, own_score 0.0
#   StateHolder.update — this.mutation (self.x = …) NOT discounted (class 3)
#   transform   — lambda, pure (class 0)
#   dynamic     — eval risk → dynamic.code risk
#   test_helper — test_* function, skipped by default

import os
import requests


def io_world(path: str) -> str:
    """Open a file and make a network request — two world effects."""
    data = open(path).read()
    return requests.get("http://example.com").text + data


def typed_local(xs: list[int]) -> list[int]:
    """Fully-typed: local mutation only, boundary discount reduces own_score to 0."""
    acc: list[int] = []
    for x in xs:
        acc.append(x * 2)
    return acc


class StateHolder:
    def __init__(self, value: int) -> None:
        self.value = value   # local.mutation in __init__ (building not-yet-aliased)

    def update(self, delta: int) -> None:
        self.value += delta  # this.mutation — escapes (receiver state), NOT discounted


transform = lambda x: x * 2   # noqa: E731  — pure lambda, class 0


def dynamic(code: str) -> None:
    """eval + exec — dynamic.code risk."""
    eval(code)   # noqa: S307


def test_helper() -> None:
    """test_* function — skipped by the Python frontend by default."""
    assert True
