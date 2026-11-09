#!/usr/bin/env python3
"""Rebuild tests/fixtures/expected/ from cargo-mutants itself.

For each case of tests/fixtures/cases.json, run `cargo mutants --list` (with
`--json` unless the case has "text": true) in the fixture directory and write
what it prints to tests/fixtures/expected/<name>.json (or .txt). The expected
files are what `cargo test` compares `cargo schemata --list` against, so run
this with cargo-mutants 27.1.0 installed, and commit the result:

    cargo install cargo-mutants --version 27.1.0 --locked
    python3 scripts/gen-expected.py            # from the root of this repository

An optional argument names another root (a checkout of this repository).
In "args", `@fixtures@` stands for the absolute path of tests/fixtures (the
diff files of `--in-diff` are there). An empty expected file means that the
tool printed nothing.
"""
import json
import os
import subprocess
import sys

root = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
fx = os.path.abspath(os.path.join(root, "tests", "fixtures"))
cases = json.load(open(os.path.join(fx, "cases.json")))
os.makedirs(os.path.join(fx, "expected"), exist_ok=True)
for c in cases:
    text = c.get("text", False)
    args = ["cargo", "mutants", "--list"] + ([] if text else ["--json"])
    args += [a.replace("@fixtures@", fx) for a in c["args"]]
    r = subprocess.run(args, cwd=os.path.join(fx, c["dir"]), capture_output=True, text=True)
    assert r.returncode == 0, (c["name"], r.stderr)
    ext = "txt" if text else "json"
    with open(os.path.join(fx, "expected", f"{c['name']}.{ext}"), "w") as f:
        f.write(r.stdout)
    print(c["name"], len(r.stdout.splitlines()))
