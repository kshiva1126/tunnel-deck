#!/usr/bin/env python3
"""Stop the worker when admission requests operator recovery."""

import os
import signal
import subprocess
import sys

from gate import Refused, state_root


def main():
    os.umask(0o077)
    root = state_root()
    if (root / "halt").exists():
        raise Refused("worker is halted; operator recovery required")
    child = subprocess.Popen(sys.argv[1:], start_new_session=True)

    def terminate(_signal=None, _frame=None):
        try:
            os.killpg(child.pid, signal.SIGTERM)
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
        except ProcessLookupError:
            pass

    def interrupted(signum, frame):
        terminate(signum, frame)
        sys.exit(128 + signum)

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    signal.signal(signal.SIGHUP, interrupted)
    try:
        while True:
            if (root / "halt").exists():
                print("symphony worker: halted; admission requires operator recovery", file=sys.stderr)
                return 1
            try:
                return child.wait(timeout=1)
            except subprocess.TimeoutExpired:
                pass
    finally:
        terminate()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (Refused, OSError, KeyError):
        print("symphony worker: cannot start; inspect trusted admission state", file=sys.stderr)
        sys.exit(1)
