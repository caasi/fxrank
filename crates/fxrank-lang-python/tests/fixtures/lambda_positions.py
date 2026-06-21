# Lambdas in positions the original collection walk MISSED, each appearing
# BEFORE an effectful trailing lambda. If any of these is not collected, the
# ordinal lambda<->anchor bijection drifts and `t` is mis-anchored.

# (1) lambda in a comprehension iterable (.for_in)
a = [x for x in map(lambda y: y, [1, 2])]

# (2) lambda in a comprehension condition (.ifs)
b = [x for x in [1, 2] if (lambda z: z)(x)]

# (3) lambda inside a generator-expression ELEMENT body (lazy for effects,
#     but still its own unit + still tokenized → collection MUST descend here)
g = ((lambda q: q)(x) for x in [1, 2])

# (4) lambda in a subscript slice key
d = {"k": 1}
s = d[(lambda: "k")()]

# (5) lambda in an f-string expression part
f = f"{(lambda: 7)()}"

# (6) lambda in a parameter-default expression
def with_default(cb=lambda: 0):
    return cb()

# (7) lambda in a `with`-item context expression
class CM:
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


def uses_with():
    with (lambda: CM())() as cm:
        return cm


# Trailing effectful lambda — its anchor must be CORRECT.
t = lambda z: requests.get(z)
