def top():
    return 1

class C:
    def method(self):
        pass

async def fetcher():
    pass

g = lambda x: x * 2
h = lambda: 0                # empty-ish body (no inner &str) — must still anchor
nested = lambda a: (lambda b: b)   # outer + inner lambda, distinct anchors
