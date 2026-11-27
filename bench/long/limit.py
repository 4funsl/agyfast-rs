#!/usr/bin/env python3
"""Run an operator-supplied trial command in its own process group for at most 600 s."""
import os
import signal
import subprocess
import sys
import time


def stop(child):
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def trial(argv, seconds=600):
    deadline = time.monotonic() + seconds
    child = subprocess.Popen(argv, start_new_session=True)
    try:
        return child.wait(timeout=max(0, deadline - time.monotonic()))
    except subprocess.TimeoutExpired:
        print("trial_timeout=true", file=sys.stderr, flush=True)
        return 124
    finally:
        stop(child)
        child.wait()


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit("usage: limit.py <headless command> [arguments ...]")
    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGHUP, interrupted)
    sys.exit(trial(sys.argv[1:]))
