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

import config  # module-level import — `config` resolves through the ImportTable

def mutates_imported_module():
    config.settings.append(1)  # F5: root `config` → import → global.mutation/6, contained=false

def captures_outer_binding():
    outer = []
    def inner():
        outer.append(1)  # F1: `outer` is none of self/global/nonlocal/param/local-here
                         # nor an import → captured opaque binding → hidden.mutation/3
    return inner

def tags_nested_def():
    def helper():
        pass
    helper.alters_data = True  # #55: `helper` is a nested-def name → a local binding →
                               # LocalMutation(contained), NOT captured-binding hidden.mutation
    return helper

def tags_nested_def_in_branch(cond):
    if cond:
        def helper_in_if():
            pass
        helper_in_if.alters_data = True  # #55: nested def inside control-flow (an `if`) is
                                         # still a function-local → LocalMutation. Guards that
                                         # `seed_defs` is threaded through the If recursion arm.
        return helper_in_if
    return None

def module_level_taggable():
    pass
module_level_taggable.alters_data = True  # #55 guard: at MODULE scope a def name is a
                                          # module binding → GlobalMutation/6, NOT local —
                                          # the nested-def seeding must not reach the <module> unit
