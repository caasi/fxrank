# fxrank-fixture: mutation escape analysis

_counter = 0

class Counter:
    def __init__(self, n):
        self.n = n  # LocalMutation, contained=true (building not-yet-aliased instance)

    def bump(self):
        self.n += 1  # ThisMutation, contained=false (receiver state escapes)

def uses_global():
    global _counter
    _counter += 1  # GlobalMutation, contained=false

def mutates_param(lst):
    lst.append(1)  # ParamMutation, contained=false

def builds_local():
    acc = []
    acc.append(1)  # LocalMutation, contained=true

def plain_global_rebind():
    global _counter
    _counter = 1  # GlobalMutation, contained=false (plain `=` to a global name)

def outer_with_nonlocal():
    x = 0
    def plain_nonlocal_rebind():
        nonlocal x
        x = 1  # ThisMutation, contained=false (plain `=` to a nonlocal name)
    return plain_nonlocal_rebind

def plain_local_binding():
    y = 1  # NO mutation effect — a true local binding

class Bag:
    def __init__(self):
        self.items = []        # LocalMutation, contained=true (direct attr init)
        self.items.append(1)   # ThisMutation, contained=false (escaping instance state)

    def store(self, i, v):
        self[i] = v            # ThisMutation, contained=false (subscript on self)
