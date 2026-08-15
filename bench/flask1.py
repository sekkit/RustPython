import sys
sys.path.insert(0, 'bench/sitepkg3')
from flask import Flask
app = Flask(__name__)
@app.route('/')
def hello():
    return 'hello from rustpython'
client = app.test_client()
r = client.get('/')
print('flask:', r.status_code, r.data.decode())
