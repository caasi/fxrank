import pickle, subprocess, yaml, importlib

def dyn(code):
    eval(code)                          # dynamic.code exact
    exec(code)

def deserialize(b):
    return pickle.loads(b)              # dynamic.code path

def shell(cmd):
    subprocess.run(cmd, shell=True)     # process.control + dynamic.code

def uses_compile(code):
    compile(code, "<string>", "exec")   # dynamic.code exact

def uses_dunder_import(name):
    __import__(name)                    # dynamic.code exact

def unsafe_yaml(stream):
    return yaml.load(stream, Loader=yaml.Loader)   # dynamic.code path

def safe_yaml(stream):
    return yaml.safe_load(stream)       # NOT a risk (safe variant)

def dynamic_import(name):
    importlib.import_module(name)       # dynamic.code path

def monkey_patch(new_fn):
    setattr(importlib, "import_module", new_fn)   # dynamic.code heuristic (imported name)

def plain_setattr(obj, new_fn):
    setattr(obj, "method", new_fn)     # NOT a risk (non-imported name)
