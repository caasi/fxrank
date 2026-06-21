# FIX 1 positive fixture: `load_dotenv` imported from the `dotenv` package
# must be flagged as env.write.
from dotenv import load_dotenv

def configure_env():
    load_dotenv()
