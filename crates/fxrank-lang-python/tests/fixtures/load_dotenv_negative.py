# FIX 1 negative fixture: `load_dotenv` imported from a user/app package
# must NOT be flagged as env.write.
from myapp.config import load_dotenv

def configure_env():
    load_dotenv()
