"""Acceptance cases run against the candidate executable through its public interfaces."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class Replay(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.binary = os.environ["HARD_BINARY"]

    def cli(self, *args, ok=True):
        p = subprocess.run([self.binary, "cli", *args], cwd=self.root,
                           capture_output=True, text=True, check=False)
        self.assertEqual(p.returncode == 0, ok, p.stdout + p.stderr)
        return p.stdout.rstrip("\n")

    def request(self, verb, args, **opts):
        return self.cli(verb, "--json", json.dumps(args), **opts)

    def rpc(self, method, params=None):
        frame = {"jsonrpc": "2.0", "id": 42, "method": method, "params": params or {}}
        p = subprocess.run([self.binary, "mcp"], cwd=self.root, input=json.dumps(frame) + "\n",
                           capture_output=True, text=True, check=True)
        reply = json.loads(p.stdout)
        self.assertEqual(reply["id"], 42)
        return reply["result"]

    def headers(self, expr, expected):
        queries = [{"type": "text", "pattern": f"item{i}"} for i in range(1, 7)]
        data = {"queries": queries, "pick": expr}
        text = self.request("search", data)
        heads = [s for s in text.splitlines() if s.startswith("## ")]
        self.assertEqual(heads, [f"## Query {i} [text] item{i}" for i in expected])

    def test_selector_order(self):
        for expr, expected in [
            ("06, 02-04 , !3, 1 , 3 ,2", [6, 2, 4, 1, 3]),
            ("  !02-5,2,*,!4,04 ", [1, 6, 2, 3, 5, 4]),
            ("!* , 3-3,1,3-5", [3, 1, 4, 5]),
            (None, list(range(1, 7))),
        ]:
            with self.subTest(expr=expr):
                self.headers(expr, expected)

    def test_invalid_selections(self):
        marker = self.root / "ran"
        for expr in ["", " ", "1,", ",1", "1,,1", "!*", "1,!1", "0", "2", "2-1",
                     "1-0", "1--1", "-1", "+1", "! 1", "1 -1", "1- 1", "１",
                     "1,184467440737095516160", "1,!!1", "1,1-", 1, ["1"]]:
            with self.subTest(expr=expr):
                self.request("check", {"commands": [{"command": "touch ran"}], "pick": expr}, ok=False)
                self.assertFalse(marker.exists())

    def test_original_limits(self):
        for verb, field, item, count in [
            ("search", "queries", {"type": "text", "pattern": "x"}, 21),
            ("check", "commands", {"command": "touch ran"}, 13),
            ("outline", "paths", "x", 41),
            ("slices", "outlines", "x", 41),
        ]:
            with self.subTest(verb=verb):
                self.request(verb, {field: [item] * count, "pick": "1"}, ok=False)
                self.assertFalse((self.root / "ran").exists())

    def test_mixed_reads(self):
        for name, data in [("intro.md", "# First"), ("last.md", "# Last"),
                           ("s.txt", "uno\ndos"), ("r.txt", "three\nfour")]:
            (self.root / name).write_text(data + "\n")
        data = {"outlines": ["intro.md", "last.md"],
                "slices": [{"path": "s.txt", "ranges": [[2, 2]]}],
                "lineRanges": ["missing", "r.txt:1"], "pick": "5,2-3,2"}
        text = self.request("slices", data)
        self.assertEqual([s for s in text.splitlines() if s.startswith("##")],
                         ["## Slice: r.txt (2 lines)", "## last.md (1 lines)", "## Slice: s.txt (2 lines)"])
        for value in ["1: three", "# Last", "2: dos"]:
            self.assertIn(value, text)
        self.assertNotIn("missing", text)
        text = self.cli("outline", "--pick", "2,1", "intro.md", "last.md", "skip")
        self.assertLess(text.index("## last.md"), text.index("## intro.md"))
        self.assertNotIn("skip", text)

    def test_command_isolation(self):
        spec = [{"command": "touch skipped"}, {"command": ""}, {"command": "true", "cwd": "absent"},
                {"command": "printf '%s' " + "x" * 85}, {"command": "echo hit >> ran"}]
        text = self.request("check", {"commands": spec, "pick": "5,4,5"})
        self.assertTrue(text.startswith("Checks: 2/2 passed"), text)
        self.assertLess(text.index("echo hit"), text.index("check-4"))
        self.assertFalse((self.root / "skipped").exists())
        self.assertEqual((self.root / "ran").read_text(), "hit\n")
        for expr, index in [("1,2", 1), ("1,3", 2)]:
            msg = self.request("check", {"commands": spec, "pick": expr}, ok=False)
            self.assertIn(f"commands[{index}]", msg)
            self.assertFalse((self.root / "skipped").exists())

    def test_footer_roundtrip(self):
        commands = [{"command": "true" if i in (2, 7) else "false"} for i in range(1, 9)]
        data = {"commands": commands, "pick": "8,2,4-6,1"}
        text = self.request("check", data)
        self.assertTrue(text.startswith("Checks: 1/6 passed, 5 failed"), text)
        footer = text.splitlines()[-1]
        self.assertEqual(footer, "Replay failed: 1,4-6,8")
        text = self.request("check", dict(data, pick=footer.removeprefix("Replay failed: ")))
        self.assertTrue(text.startswith("Checks: 0/5 passed, 5 failed"), text)
        self.assertEqual(text.splitlines()[-1], footer)
        self.assertEqual(self.request("check", dict(data, pick="7,2")).splitlines()[-1], "Replay failed: -")
        for legacy in [{"commands": commands}, dict(data, pick=None)]:
            self.assertNotIn("Replay failed:", self.request("check", legacy))

    def test_byte_budgets(self):
        self.test_byte_budgets_footer()
        self.test_byte_budgets_selected()
        self.test_byte_budgets_framing()

    def test_byte_budgets_footer(self):
        cmd = {"command": "cat long.txt && false"}
        (self.root / "long.txt").write_text("λ🌍" * 5000)
        for mode in ["full", "compact"]:
            text = self.request("check", {"commands": [cmd] * 8, "pick": "8,2-4", "maxChars": 10,
                                          "outputMode": mode})
            self.assertLessEqual(len(text.encode()), 500)
            self.assertEqual(text.splitlines()[-1], "Replay failed: 2-4,8")

    def test_byte_budgets_selected(self):
        # A skipped item must not take any of the selected item's budget.
        (self.root / "long.md").write_text("".join(f"# Heading {i} λ🌍\n" for i in range(200)))
        one = self.request("outline", {"paths": ["long.md"], "maxChars": 1000})
        replay = self.request("outline", {"paths": ["long.md"] * 40, "pick": "37", "maxChars": 1000})
        self.assertEqual(one, replay)
        self.assertLessEqual(len(replay.encode()), 1000)

    def test_byte_budgets_framing(self):
        # Strip only the CLI's framing LF. Section separators count as payload.
        res = subprocess.check_output([self.binary, "cli", "slices", "--json", json.dumps({
            "lineRanges": ["s.txt:1"] * 550, "pick": "*", "maxChars": 1000
        })], cwd=self.root)
        self.assertLessEqual(len(res.removesuffix(b"\n")), 1000)

    def test_schema_cli(self):
        schemas = {s["name"]: s["inputSchema"] for s in self.rpc("tools/list")["tools"]}
        self.assertEqual(set(schemas), {"batch_search", "quick_outline", "apply_edits", "run_checks"})
        for name, schema in schemas.items():
            if name == "apply_edits":
                self.assertNotIn("pick", schema["properties"])
            else:
                self.assertEqual(schema["properties"]["pick"]["type"], "string")
                self.assertNotIn("pick", schema["required"])
        self.assertIn("--pick", self.cli("--help"))
        text = self.cli("search", "--json", '{"pick":"2"}', "--pick", "1", "--text", "aaa", "--text", "bbb")
        self.assertIn("## Query 1 [text] aaa", text)
        self.assertNotIn("bbb", text)
        result = self.rpc("tools/call", {"name": "batch_search", "arguments": {
            "pick": "2", "queries": json.dumps([{"type": "text", "pattern": x} for x in ["aaa", "bbb"]])
        }})
        self.assertFalse(result["isError"])
        self.assertEqual(result["content"][0]["text"].splitlines()[0], "## Query 2 [text] bbb")

    def test_writes(self):
        edits = [{"path": "saved", "create": "hello"}]
        result = self.rpc("tools/call", {"name": "apply_edits", "arguments": {"edits": edits, "pick": "*"}})
        self.assertTrue(result["isError"])
        self.assertFalse((self.root / "saved").exists())
        self.cli("edit", "--pick", "1", "--json", json.dumps({"edits": edits}), ok=False)
        self.assertFalse((self.root / "saved").exists())

    def test_search_fairness(self):
        padding = "ø" * 110 + "\n"
        for name in ["a.txt", "c.txt"]:
            (self.root / name).write_text(padding * 2 + "hit\n" + padding * 2)
        for name in ["b.txt", "d.txt"]:
            (self.root / name).write_text("hit\n")
        text = self.request("search", {"queries": [{"type": "text", "pattern": "x"},
                                                   {"type": "text", "pattern": "hit", "contextLines": 2}],
                                       "pick": "2", "maxChars": 500})
        self.assertIn("## Query 2", text)
        self.assertIn("b.txt\n 1: hit", text)
        self.assertIn("d.txt\n 1: hit", text)
        self.assertIn("showing 2 of 4", text)
        self.assertLessEqual(len(text.encode()), 500)


if __name__ == "__main__":
    unittest.main()
