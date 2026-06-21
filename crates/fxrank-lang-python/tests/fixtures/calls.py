import os, subprocess, requests, logging, random, time, sys

def io_boundary(path):
    data = open(path).read()
    logging.info("read")
    return requests.get("http://x").text

def env_and_rng():
    subprocess.run(["ls"], shell=True)
    return os.getenv("X"), random.random(), time.time()

def reads_stdin():
    return input("name? ")

def db_write(session):
    session.commit()

def in_wrapper(paths):
    with open(paths[0]) as f:                 # with-open → net.fs.db attributed
        return f.read()

def eager_comp(urls):
    return [requests.get(u) for u in urls]    # eager comprehension → net.fs.db charged

def lazy_gen(urls):
    return (requests.get(u) for u in urls)    # lazy genexp → element body NOT charged

def cli_args():
    return sys.argv[1]

