"""Drive the installed Go TUI against an already running disposable server."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import sys
import termios
import time

binary, origin, token_file, config, title, output = sys.argv[1:]
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 120, 0, 0))
env = {k: v for k, v in os.environ.items() if not k.startswith('MOTION_')}
env.update(PATH='/nonexistent-motion-test-path', TERM='xterm-256color')
process = subprocess.Popen([binary, '--server', origin, '--operator-token-file', token_file,
                            '--config-dir', config, 'tui'], stdin=slave, stdout=slave,
                           stderr=slave, env=env, start_new_session=True)
os.close(slave)
transcript = bytearray()
receipt = {'scope': 'Installed Go TUI, real HTTP server, disposable operator/config', 'checks': []}


def until(predicate, timeout=30):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if select.select([master], [], [], .2)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                chunk = b''
            transcript.extend(chunk)
            # Bubble Tea queries terminal position/background at startup.
            if b'\x1b[6n' in chunk:
                os.write(master, b'\x1b[1;1R')
            if b'\x1b]11;?' in chunk:
                os.write(master, b'\x1b]11;rgb:0000/0000/0000\x1b\\')
        if predicate():
            return
        if process.poll() is not None:
            raise AssertionError(f'TUI exited early: {process.returncode}')
    raise AssertionError('TUI observation deadline exceeded')


try:
    until(lambda: b'Overview' in transcript)
    receipt['checks'].append('authenticated TUI overview rendered')
    os.write(master, b'6')  # Catalog tab, using the production keyboard mapping.
    until(lambda: title.encode() in transcript)
    receipt['checks'].append('real catalog fixture rendered')
    os.write(master, b'jkr')  # Selection movement and refresh.
    time.sleep(.3)
    os.write(master, b'q')
    process.wait(timeout=10)
    assert process.returncode == 0, process.returncode
    receipt['checks'].append('clean quit after selection and refresh key input')
    receipt['result'] = 'passed'
finally:
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
    os.close(master)
    Path(output).write_text(json.dumps(receipt, indent=2) + '\n')
    Path(output).with_suffix('.terminal.txt').write_bytes(transcript)
