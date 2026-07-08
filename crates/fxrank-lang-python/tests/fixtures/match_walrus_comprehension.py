# fxrank-fixture: #78/#55 (Task 8) — match-case captures, walrus targets, and
# comprehension `for` targets recovered as function-locals.
#
# Before Task 8, none of these three binding forms were collected by the
# prescan, so each fell through to `Unresolved` -> `hidden.mutation`
# "captured-binding" (a false positive). This fixture pins the recovery: every
# mutating use below must show as a plain `local.mutation`, with no
# `hidden.mutation` / "captured-binding" anywhere in the report.


def h(cmd, rows):
    match cmd:
        case [op, *args]:
            args.append(op)

    if (buf := []) is not None:
        buf.append(1)

    return [r.sort() or r for r in rows]
