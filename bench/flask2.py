import sys
sys.path.insert(0, 'bench/sitepkg3')
from flask import Flask, session, render_template_string
app = Flask(__name__)
app.secret_key = 'test'
@app.route('/tpl')
def tpl():
    return render_template_string('<h1>Hello {{ name }}!</h1>', name='RustPython')
@app.route('/sess')
def sess():
    session['user'] = 'alice'
    return session.get('user', 'none')
c = app.test_client()
print('template:', c.get('/tpl').data.decode())
print('session :', c.get('/sess').data.decode())
# jinja2 直接
from jinja2 import Environment
env = Environment()
print('jinja2  :', env.from_string('{% for i in range(3) %}{{ i }}{% endfor %}').render())
