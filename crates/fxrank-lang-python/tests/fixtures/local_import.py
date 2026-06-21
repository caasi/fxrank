# fxrank-fixture: function-local imports (FIX 5). The ONLY import in this file is
# inside the function body, so resolution requires file-wide import collection.

def f(c):
    import subprocess
    subprocess.run(c, shell=True)   # process.control effect + dynamic.code risk
