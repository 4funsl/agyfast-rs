"""Public-protocol acceptance cases, loaded only by the external scorer."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
import time

class Reports(unittest.TestCase):
    binary = None

    def timeout(self):
        return min(10, max(0.001, self.deadline - time.monotonic()))

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.path = self.root / "sample report.txt"

    def rpc(self, method, params):
        frame = {"id": 17, "jsonrpc": "2.0", "method": method, "params": params}
        raw = subprocess.check_output([self.binary, "mcp"], cwd=self.root, text=True,
                                      input=json.dumps(frame) + "\n", timeout=self.timeout())
        reply = json.loads(raw)
        self.assertEqual(reply["id"], 17)
        return reply["result"]

    def inspect(self, fmt, data, *, error=None, cap=8800):
        self.path.write_bytes(data.encode("utf-8"))
        res = self.rpc("tools/call", {"name": "inspect_report", "arguments": {
            "format": fmt, "path": self.path.name, "cwd": str(self.root), "maxChars": cap}})
        text = res["content"][0]["text"]
        if error is not None:
            self.assertTrue(res["isError"], text)
            self.assertIn(f"{fmt} line {error}:", text)
            return
        self.assertFalse(res["isError"], text)
        self.assertLessEqual(len(text.encode()), cap)
        return json.loads(text)

    def test_inspect_help(self):
        out = subprocess.check_output([self.binary, "cli", "--help"], text=True, timeout=self.timeout())
        for term in ["inspect","--format","--report","diff","lcov","tap"]:
            self.assertIn(term, out)

    def test_inspect_flags(self):
        self.path.write_text("1..0")
        def invoke(*args):
            p = subprocess.run([self.binary,"cli","inspect",*args], cwd=self.root, capture_output=True, timeout=self.timeout())
            return p.returncode, p.stdout
        for fmt in ["tap","diff","lcov","unknown"]:
            data = dict(path=self.path.name, format=fmt)
            self.assertEqual(invoke("--format",fmt,"--report",self.path.name),
                             invoke("--json",json.dumps(data)))

    def test_inspect_schema(self):
        listed = self.rpc("tools/list", {})["tools"]
        self.assertEqual([x["name"] for x in listed], ["batch_search","quick_outline","apply_edits","run_checks","inspect_report"])
        schema = listed[-1]["inputSchema"]
        self.assertEqual(set(schema["required"]), {"path","format"})
        props = schema["properties"]
        self.assertEqual(set(props["format"]["enum"]), {"diff","lcov","tap"})
        for key, kind in [("path","string"),("format","string"),("cwd","string"),("maxChars","integer")]:
            self.assertEqual(props[key]["type"], kind)

# Each case checks one externally consumed value, or one physical error line.
# The final field points to its independently replayable fixture evidence.
D = "--- a/f\n+++ b/f\n"
M = "From " + "ab" * 20 + " Mon Sep 17 00:00:00 2001\nSubject: reproduction\n\n"
G = "diff --git a/f b/f\n"
MOVE = G + "similarity index 91%\ncopy from module\ncopy to module copy\n"
OTHER = "SF:lib\nFNL:20,9\nend_of_record\n"
SAMPLES = {
    "diff": [
        ("mail_sections", M + G + D + "@@ -1 +1 @@\n a\n-- \nfooter\n" + M + G + D, "/summary/files", 2, "mailbox"),
        ("mail_location", M + "unexpected body text\n" + G + D + "@@ -x +1 @@\n", None, 8, "mailbox_line"),
        ("copy_row", MOVE, "/rows/0", {"kind":"copy","old":"module","new":"module copy"}, "transfer"),
        ("transfer_edited_file_count", MOVE + "index aa..bb\n--- a/module\n+++ b/module copy\n@@ -1 +1 @@\n-s\n+t\n", "/summary/files", 1, "transfer"),
        ("transfer_destination_bytes", G + 'rename from old\nrename to "new/\\303\\251"\n', "/rows/0/new", "new/é", "transfer"),
        ("empty_context_cursor", D + "@@ -8,2 +11,2 @@\n\n-x\n+y", "/rows/0/newAt", 12, "blank_context"),
        ("empty_context_boundary", D + "@@ -1,3 +1,3 @@\n-a\n+b\n\n-c\n+d", "/rows/1/oldAt", 3, "blank_context"),
        ("old_range_rewind", D + "@@ -8,2 +1,2 @@\n x\n y\n@@ -9 +7 @@\n p", None, 6, "overlap"),
        ("new_range_rewind", D + "@@ -0,0 +4,2 @@\n+a\n+b\n@@ -2,0 +5 @@\n+c", None, 6, "new_overlap"),
    ],
    "lcov": [
        ("drive_identity", "SF:d:\\obj\\..\\pkg.rs\nDA:4,2\nend_of_record\nSF:D:/pkg.rs\nDA:4,4\nend_of_record", "/rows/0/hits", 6, "runners"),
        ("unc_root", "SF:\\\\build\\src\\..\\..\\main.rs\nend_of_record", "/rows/0/path", "//build/src/main.rs", "runners"),
        ("function_extent_name", "SF:s\nFNDA:3,M::f,a\nFN:10,12,M::f,a\nend_of_record", "/rows/0/functions", 1, "ranges"),
        ("function_extent_conflict", "SF:s\nFN:3,8,m\nend_of_record\nSF:s\nFN:3,9,m\nend_of_record", None, 5, "conflict"),
        ("function_extent_reverse", "SF:f\nFN:50,49,wrap\nend_of_record", None, 2, "backwards"),
        ("group_alias_hits", "SF:x\nFNL:11,20,23\nFNA:11,3,α\nFNA:11,0,β\nend_of_record", "/rows/0/uncoveredFunctions", ["β"], "aliases"),
        ("group_id_lifetime", OTHER + "SF:alt\nFNA:20,1,ghost\nend_of_record", None, 5, "alias_scope"),
        ("retry_retracts_lines", "TN:w\nSF:x\nDA:1,4\nDA:2,1\nend_of_record\nSF:./x\nDA:9,0\nend_of_record", "/rows/0/lines", 1, "retry"),
        ("retry_keeps_peers", "TN:a\nSF:x\nDA:1,8\nend_of_record\nTN:b\nSF:x\nDA:1,4\nend_of_record\nTN:a\nSF:x\nDA:1,2\nend_of_record", "/rows/0/hits", 6, "retry"),
    ],
    "tap": [
        ("lane_order", "[two] 1..1\n[one] ok - fast\n[one] 1..1\n[two] ok - slow", "/rows/0/lane", "two", "lanes"),
        ("lane_totals", "[build] not ok - no\n[test] not ok - error\n[test] 1..1\n[build] 1..1", "/summary/fail", 2, "lanes"),
        ("lane_source_line", "[w2] 1..0\n[w1] ok 1 - ok\n[w1] ok 9 - wrong", None, 3, "lane_error"),
        ("escaped_label", r"ok a\#b\\c # SKIP slow" + "\n1..1", "/rows/0/name", "a#b\\c", "escapes"),
        ("escaped_scope", "# Subtest: a\\#b\n    ok - child\n    1..1\nok 1 a\\#b\n1..1", "/rows/0/scope", ["a#b"], "escaped_scope"),
        ("buffered_postorder", "ok - outer {\n    not ok child\n    1..1\n}\n1..1", "/rows/1/subtest", True, "block"),
        ("buffered_close_location", "ok sub {\n    ok leaf\n    1..1\n1..1", None, 4, "unclosed"),
        ("diagnostic_indent", "ok x\n    ---\n    text: |\n      ...\n    ...\n1..1", "/rows/0/diagnostic", ["text: |","  ..."], "attachment"),
        ("diagnostic_blank", "not ok x\n  ---\n  before\n\n  after\n  ...\n1..1", "/rows/0/diagnostic", ["before","","after"], "attachment"),
    ],
}


def sample(fmt, raw, pointer, expected):
    def run(self):
        if pointer is None:
            self.inspect(fmt, raw, error=expected)
            return
        value = self.inspect(fmt, raw)
        for part in pointer.strip("/").split("/"):
            value = value[int(part)] if isinstance(value, list) else value[part]
        self.assertEqual(value, expected)
    return run


CASES = {}
for group, (fmt, entries) in zip("abc", SAMPLES.items()):
    CASES["workstream_" + group] = []
    for name, raw, pointer, expected, evidence in entries:
        CASES["workstream_" + group].append(name)
        setattr(Reports, "test_" + name, sample(fmt, raw, pointer, expected))
CASES["integration"] = ("inspect_help", "inspect_flags", "inspect_schema")
