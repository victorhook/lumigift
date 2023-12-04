from flask import Flask, render_template, request
from lumigiftserver import LumigiftServer

server = LumigiftServer()
app = Flask(__name__)


@app.route('/')
def home():
    return render_template('index.html', message="Hello, World!")

@app.route('/set_id', methods=['POST'])
def set_id():
    from_id = request.form.get('from_id')
    to_id = request.form.get('to_id')
    print(f'SET ID {from_id} -> {to_id}')
    if not to_id:
        return
    server.set_id(int(from_id), int(to_id))
    return render_template('index.html', message="Hello, World!")

@app.route('/blink', methods=['POST'])
def blink():
    print(request.form)
    id = request.form.get('id')
    print(f'BLINK ID {id}')
    server.blink(int(id))
    return render_template('index.html', message="Hello, World!")

@app.route('/reboot', methods=['POST'])
def reboot():
    id = request.form.get('id')
    print(f'REBOOT ID: {id}')
    server.reboot(int(id))
    return render_template('index.html', message="Hello, World!")

@app.route('/start', methods=['POST'])
def reboot():
    server.start()
    return render_template('index.html', message="Hello, World!")

@app.route('/stop', methods=['POST'])
def reboot():
    server.start()
    return render_template('index.html', message="Hello, World!")


if __name__ == '__main__':
    server.start()
    app.run(debug=True, use_reloader=False)