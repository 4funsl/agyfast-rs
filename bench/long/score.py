"""Score a working-copy snapshot. The only writes are outside the fixture."""
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

HERE = Path(__file__).resolve().parent
AUTHORED = {
    "diff_tests": ("diff_archive_regression", "diff_boundary_regression"),
    "lcov_tests": ("lcov_archive_regression", "lcov_boundary_regression"),
    "tap_tests": ("tap_archive_regression", "tap_boundary_regression"),
}


def git(root, *args):
    return subprocess.check_output(["git", "-C", root, *args])


def snapshot(src, dst):
    names = git(src, "ls-files", "--cached", "--others", "--exclude-standard", "-z")
    for raw in set(names.split(b"\0")) - {b""}:
        path = Path(os.fsdecode(raw))
        origin = src / path
        if origin.is_file():
            dest = dst / path
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(origin, dest)
            shutil.copymode(origin, dest)


def run_checks(src, tmp, target, deadline):
    root = tmp / "candidate"
    root.mkdir()
    snapshot(src, root)
    probe = tmp / "hidden"
    shutil.copytree(HERE / "hidden", probe)
    spec = importlib.util.spec_from_file_location("acceptance", probe / "reports.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    groups = {}
    findings = {}
    verbose = os.getenv("LONG_VERBOSE") == "1"

    def record(group, key, ok):
        groups.setdefault(group, []).append(bool(ok))
        findings[key] = bool(ok)
        print(f"{key}={'pass' if ok else 'fail'}", flush=True)

    def cargo(name, *options):
        command = ["cargo", name, "--offline", "--locked", "--all-targets",
                   "--target-dir", str(target), *options]
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            record("hygiene", name, False)
            return ""
        with subprocess.Popen(command, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              start_new_session=True, text=True, errors="replace") as proc:
            try:
                output = proc.communicate(timeout=remaining)[0]
                ok = proc.returncode == 0
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                output = proc.communicate()[0] + "\nVerifier deadline reached"
                ok = False
        record("hygiene", name, ok)
        if verbose and not ok:
            print(output[-5000:], file=sys.stderr)
        return output

    artifacts = cargo("build", "--message-format=json")
    cargo("clippy", "--", "-D", "warnings")
    output = cargo("test", "--no-fail-fast", "--", "--format", "pretty", "--color", "never")
    for key, names in AUTHORED.items():
        passed = all(re.search(r"^test (?:.*::)?" + name + r" \.\.\. ok$", output, re.M)
                     for name in names)
        record("hygiene", key, passed)
    binary = None
    if findings["build"]:
        for line in artifacts.splitlines():
            if not line.startswith("{"):
                continue
            event = json.loads(line)
            if (event.get("reason") == "compiler-artifact" and event["target"]["name"] == "agy-rs"
                    and event["target"]["kind"] == ["bin"] and not event["profile"]["test"]):
                binary = event.get("executable")
    module.Reports.binary = binary
    module.Reports.deadline = deadline
    for group, names in module.CASES.items():
        for name in names:
            case = module.Reports("test_" + name)
            result = unittest.TestResult()
            available = binary and time.monotonic() < deadline
            if available:
                case.run(result)
            record(group, name, bool(available) and result.wasSuccessful())
            if verbose:
                for _, trace in result.failures + result.errors:
                    print(trace, file=sys.stderr)
    for group in (*module.CASES, "hygiene"):
        scores = groups[group]
        print(f"{group}={sum(scores)}/{len(scores)}")
    print(f"score={sum(findings.values())}/{len(findings)}")


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: verify.sh <fixture>")
    deadline = time.monotonic() + 55
    src = Path(sys.argv[1]).resolve()
    marker = src / ".git/long-repair-fixture"
    if not marker.is_file() or git(src, "rev-parse", "start^{commit}").strip() != marker.read_bytes().strip():
        sys.exit("expected the fixture created by setup.sh")
    with tempfile.TemporaryDirectory(prefix="agy-long-score-") as empty:
        tmp = Path(empty).resolve()
        target = Path(os.getenv("LONG_VERIFY_TARGET", tmp / "target")).resolve()
        for directory in (tmp, target):
            if any(directory == base or base in directory.parents for base in (src, HERE.parents[1])):
                sys.exit("scratch and Cargo target must be outside both repositories")
        run_checks(src, tmp, target, deadline)


if __name__ == "__main__":
    main()
