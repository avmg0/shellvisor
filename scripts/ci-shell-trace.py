#!/usr/bin/env python3
"""CI diagnostics: start bash and zsh with Shellvisor's hooks the way the tests
do, run one command, and print exactly what the shell wrote (escape sequences
visible). Used to see why a runner's shells behave differently."""
import os, pty, select, subprocess, sys, tempfile, time

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
home = tempfile.mkdtemp(prefix="sv-trace-")
open(os.path.join(home, ".bash_profile"), "w").write("PS1='PROMPT> '\n")
env = {k: v for k, v in os.environ.items() if k not in ("PROMPT_COMMAND", "BASH_ENV", "ENV", "ZDOTDIR")}
env.update(HOME=home, TERM="xterm-256color", SHELL="/bin/sh", SHELLVISOR="1", SHELLVISOR_LOGIN="1")

def run(argv, inputs):
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe(argv[0], argv, env)
    out = b""
    def pump(t):
        nonlocal out
        end = time.time() + t
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.05)
            if r:
                try:
                    out += os.read(fd, 65536)
                except OSError:
                    return
    pump(2.0)
    for line in inputs:
        os.write(fd, line.encode())
        pump(1.0)
    try:
        os.kill(pid, 9)
    except OSError:
        pass
    return out

print("== system")
for cmd in (["sh", "-c", "head -2 /etc/os-release; bash --version | head -1; zsh --version"],
            ["sh", "-c", "env | grep -iE '^(vte|prompt|ps0|ps1|hist|term|bash|starship|kitty|iterm|wezterm)' | sort"],
            ["sh", "-c", "ls /etc/profile.d/"]):
    print(subprocess.run(cmd, capture_output=True, text=True).stdout)

print("== bash")
out = run(["bash", "--rcfile", os.path.join(root, "shell/bash/shellvisor.bash"), "-i"],
          ["declare -p PROMPT_COMMAND PS0 2>&1; trap -p DEBUG\r", "true\r", "echo two\r"])
print(repr(out.decode(errors="replace"))[:6000])
