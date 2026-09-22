#!/usr/bin/env python3
"""Isolated native OpenSSH probe for Linux and macOS.

The protocol checks are portable.  Guardian completion is reported through a
pipe owned by the harness, so the probe does not need Linux subreapers or
``/proc`` process inspection.
"""

import fcntl
import os
from pathlib import Path
import pwd
import select
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time


def run(args):
    return subprocess.run(args, capture_output=True, timeout=10, check=False)


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for(predicate, message, seconds=8):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise RuntimeError(message)


def listening(number):
    with socket.socket() as sock:
        sock.settimeout(0.2)
        return sock.connect_ex(("127.0.0.1", number)) == 0


def receive(sock, count):
    data = b""
    while len(data) < count:
        chunk = sock.recv(count - len(data))
        require(chunk, "unexpected EOF")
        data += chunk
    return data


def banner(number, target=None):
    with socket.create_connection(("127.0.0.1", number), timeout=3) as sock:
        if target is not None:
            sock.sendall(b"\x05\x01\x00")
            require(receive(sock, 2) == b"\x05\x00", "SOCKS authentication")
            sock.sendall(b"\x05\x01\x00\x01\x7f\x00\x00\x01" + target.to_bytes(2, "big"))
            require(receive(sock, 10)[:2] == b"\x05\x00", "SOCKS connect")
        require(sock.recv(256).startswith(b"SSH-"), "forwarded SSH banner missing")


def terminate(child):
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=3)


def probe(root):
    ssh = shutil.which("ssh")
    sshd = shutil.which("sshd")
    require(ssh and sshd and shutil.which("ssh-keygen"), "ssh, sshd, ssh-keygen required")
    if sys.platform == "darwin":
        require(Path(ssh).resolve() == Path("/usr/bin/ssh"),
                "macOS probe must use Apple's /usr/bin/ssh")
    print(run([ssh, "-V"]).stderr.decode().strip(), flush=True)
    for name in ("host", "client"):
        require(run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)]).returncode == 0,
                "test key generation failed")
    server_port, extra_port = port(), port()
    username = pwd.getpwuid(os.getuid()).pw_name
    server_config = root / "sshd_config"
    server_config.write_text(
        f"ListenAddress 127.0.0.1\nPort {server_port}\nHostKey {root}/host\n"
        f"PidFile {root}/sshd.pid\nAuthorizedKeysFile {root}/client.pub\n"
        f"AllowUsers {username}\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n"
        # The disposable authorized_keys path is below /tmp. Disable only the
        # server's ancestor mode check; client host-key verification stays strict.
        "UsePAM no\nStrictModes no\nAllowTcpForwarding yes\nPermitListen 127.0.0.1:*\n"
    )
    public = (root / "host.pub").read_text().split()
    (root / "known_hosts").write_text(f"[127.0.0.1]:{server_port} {public[0]} {public[1]}\n")
    config = root / "config"
    config.write_text(f"Include {root}/hosts\n")
    (root / "hosts").write_text(
        "Host jump\n ControlPath none\n"
        f"Host probe jump\n HostName 127.0.0.1\n Port {server_port}\n User {username}\n"
        f" IdentityFile {root}/client\n IdentitiesOnly yes\n"
        f" UserKnownHostsFile {root}/known_hosts\n GlobalKnownHostsFile /dev/null\n"
        " StrictHostKeyChecking yes\n BatchMode yes\n"
        f" ControlMaster auto\n ControlPath {root}/existing.sock\n"
        f" LocalForward 127.0.0.1:{extra_port} 127.0.0.1:{server_port}\n"
    )

    def master(path):
        return [ssh, "-F", str(config), "-N", "-T", "-n", "-S", str(path),
                "-o", "BatchMode=yes", "-o", "ClearAllForwardings=yes",
                "-o", "ControlMaster=yes", "-o", "ControlPersist=no",
                "-o", "ForkAfterAuthentication=no", "-o", "ExitOnForwardFailure=yes", "probe"]

    def control(path, operation, *args):
        return run([ssh, "-F", "/dev/null", "-S", str(path), "-O", operation,
                    "-o", "ClearAllForwardings=no", "-o", "ExitOnForwardFailure=yes", *args, "probe"])

    children = []
    with (root / "server.log").open("wb") as log:
        server = subprocess.Popen([sshd, "-D", "-e", "-f", str(server_config)], stdout=log, stderr=log)
        children.append(server)
        try:
            wait_for(lambda: listening(server_port), "isolated sshd did not start")
            existing_path, managed_path = root / "existing.sock", root / "managed.sock"
            for path in (existing_path, managed_path):
                child = subprocess.Popen(master(path), stdout=subprocess.DEVNULL, stderr=log)
                children.append(child)
                wait_for(lambda: control(path, "check").returncode == 0, "private master did not authenticate")
            require(not listening(extra_port), "config forwarding leaked into managed connection")
            require(control(existing_path, "check").stderr != control(managed_path, "check").stderr,
                    "managed master reused existing master")
            print("PASS: Include/Host/key/known-hosts; extra forwarding suppressed; master isolation", flush=True)

            unknown = master(root / "unknown.sock")
            unknown[-1:-1] = ["-o", "UserKnownHostsFile=/dev/null"]
            require(run(unknown).returncode != 0, "unknown host key incorrectly accepted")
            jump_path = root / "jump.sock"
            jump_command = master(jump_path)
            jump_command[-1:-1] = ["-J", "jump"]
            jump_child = subprocess.Popen(jump_command, stdout=subprocess.DEVNULL, stderr=log)
            children.append(jump_child)
            wait_for(lambda: control(jump_path, "check").returncode == 0, "ProxyJump authentication failed")
            jump_port = port()
            require(control(jump_path, "forward", "-L",
                            f"127.0.0.1:{jump_port}:127.0.0.1:{server_port}").returncode == 0,
                    "ProxyJump forwarding failed")
            banner(jump_port)
            terminate(jump_child)
            wait_for(lambda: not listening(jump_port), "ProxyJump listener survived stop")
            print("PASS: ProxyJump traffic and strict unknown-host rejection", flush=True)

            for kind in ("-L", "-R", "-D"):
                number = port()
                spec = f"127.0.0.1:{number}"
                if kind != "-D":
                    spec += f":127.0.0.1:{server_port}"
                require(control(managed_path, "forward", kind, spec).returncode == 0, f"{kind} rejected")
                banner(number, server_port if kind == "-D" else None)
                require(control(managed_path, "cancel", kind, spec).returncode == 0, "cancel failed")
                wait_for(lambda: not listening(number), "listener survived cancel")
                print(f"PASS: {kind} acknowledged, real traffic, cancellation", flush=True)

            with socket.socket() as occupied:
                occupied.bind(("127.0.0.1", 0))
                occupied.listen()
                number = occupied.getsockname()[1]
                for kind in ("-L", "-R"):
                    require(control(managed_path, "forward", kind,
                                    f"127.0.0.1:{number}:127.0.0.1:{server_port}").returncode != 0,
                            "occupied listener incorrectly accepted")
            require(control(managed_path, "forward", "-R",
                            f"0.0.0.0:{port()}:127.0.0.1:{server_port}").returncode != 0,
                    "server policy rejection incorrectly accepted")
            require(control(root / "missing.sock", "forward", "-L",
                            f"127.0.0.1:{port()}:127.0.0.1:{server_port}").returncode != 0,
                    "missing master incorrectly accepted")
            print("PASS: local/remote conflict, remote policy rejection, missing master", flush=True)

            dead = port()
            number = port()
            spec = f"127.0.0.1:{number}:127.0.0.1:{dead}"
            require(control(managed_path, "forward", "-L", spec).returncode == 0,
                    "setup unexpectedly tested destination reachability")
            with socket.create_connection(("127.0.0.1", number), timeout=3) as sock:
                require(sock.recv(1) == b"", "dead destination unexpectedly returned data")
            control(managed_path, "cancel", "-L", spec)
            print("PASS: setup success is distinct from destination health", flush=True)

            crash_probe(root, master, control, server_port)
            require(control(existing_path, "check").returncode == 0, "unrelated master stopped")
        except Exception:
            # Logs contain only generated test configuration, never personal credentials.
            try:
                print((root / "server.log").read_text()[-4000:], flush=True)
            except OSError as error:
                print(f"server log unavailable: {error}", flush=True)
            raise
        finally:
            for child in reversed(children):
                terminate(child)


def crash_probe(root, master, control, server_port):
    # The guardian reports completion after reaping SSH.  This explicit IPC is
    # portable and avoids relying on Linux subreapers or /proc visibility.
    read_fd, write_fd = os.pipe()
    path = root / "guardian.sock"
    daemon = os.fork()
    if daemon == 0:
        os.close(read_fd)
        lock_fd = os.open(root / "daemon.lock", os.O_CREAT | os.O_RDWR, 0o600)
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        lease_daemon, lease_guardian = socket.socketpair()
        guardian = os.fork()
        if guardian == 0:
            lease_daemon.close()
            child = subprocess.Popen(master(path), start_new_session=True,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            os.write(write_fd, f"{os.getpid()} {child.pid}\n".encode())
            lease_guardian.recv(1)
            # Do not poll/reap the leader until all group signaling is finished.
            os.killpg(child.pid, signal.SIGTERM)
            time.sleep(0.3)
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except PermissionError:
                # Darwin reports EPERM for a group containing only the exited
                # leader. Accept it only after confirming that leader exited.
                require(child.poll() is not None,
                        "guardian lost permission to terminate a live SSH group")
            child.wait(timeout=3)
            lease_guardian.close()
            os.close(lock_fd)
            os.write(write_fd, b"DONE\n")
            os.close(write_fd)
            os._exit(0)
        lease_guardian.close()
        os.close(write_fd)
        while True:
            signal.pause()
    os.close(write_fd)
    guardian = None
    try:
        require(select.select([read_fd], [], [], 5)[0], "guardian startup timed out")
        guardian, _ssh_pid = map(int, os.read(read_fd, 128).split())
        wait_for(lambda: control(path, "check").returncode == 0, "guardian master not ready")
        number = port()
        require(control(path, "forward", "-L", f"127.0.0.1:{number}:127.0.0.1:{server_port}").returncode == 0,
                "guardian forwarding failed")
        banner(number)
        with (root / "daemon.lock").open("rb") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                pass
            else:
                raise RuntimeError("daemon lock not held")
            os.kill(daemon, signal.SIGKILL)
            os.waitpid(daemon, 0)
            daemon = None

            def unlocked():
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    return True
                except BlockingIOError:
                    return False

            wait_for(unlocked, "guardian did not release lock after cleanup")
            require(not listening(number), "listener survived guardian cleanup")
            require(select.select([read_fd], [], [], 5)[0], "guardian completion timed out")
            require(os.read(read_fd, 128) == b"DONE\n", "invalid guardian completion")
        guardian = None
        print("PASS: daemon SIGKILL -> guardian EOF -> SSH reaped, listener closed, lock released", flush=True)
    finally:
        if daemon is not None:
            os.kill(daemon, signal.SIGKILL)
            os.waitpid(daemon, 0)
        if guardian is not None:
            # The daemon owns the guardian. After daemon SIGKILL it is orphaned,
            # so completion is observed through the pipe instead of waitpid.
            select.select([read_fd], [], [], 8)
        os.close(read_fd)


if __name__ == "__main__":
    os.umask(0o077)
    with tempfile.TemporaryDirectory(prefix="td-probe-", dir="/tmp") as directory:
        probe(Path(directory))
    print("All probe checks passed; temporary keys/configuration removed.")
