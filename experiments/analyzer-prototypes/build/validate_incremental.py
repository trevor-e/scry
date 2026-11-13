#!/usr/bin/env python3
"""Confirm the missing-watch behavior on our own dependency-free Cargo fixture."""
import json
from pathlib import Path
import subprocess
import tempfile
import time


def main():
    with tempfile.TemporaryDirectory(prefix="scry-build-incremental-") as tmp:
        root = Path(tmp)
        (root / "src").mkdir()
        (root / "Cargo.toml").write_text('[package]\nname = "scry-build-watch-fixture"\nversion = "0.1.0"\nedition = "2021"\n')
        (root / "src/main.rs").write_text('fn main() { println!("{}", include_str!(concat!(env!("OUT_DIR"), "/copied.txt"))); }\n')
        script = '''fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let input = std::fs::read_to_string("input.txt").unwrap();
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join("copied.txt"), input).unwrap();
}
'''
        (root / "build.rs").write_text(script)
        (root / "input.txt").write_text("first")
        results = [{"phase": "initial", "observed": run(root), "expected": "first"}]
        time.sleep(1.1)
        (root / "input.txt").write_text("second")
        results.append({"phase": "input changed without watch", "observed": run(root), "expected": "first"})
        time.sleep(1.1)
        (root / "build.rs").write_text(script.replace('println!("cargo::rerun-if-changed=build.rs");', 'println!("cargo::rerun-if-changed=build.rs");\n    println!("cargo::rerun-if-changed=input.txt");'))
        results.append({"phase": "watch added", "observed": run(root), "expected": "second"})
        time.sleep(1.1)
        (root / "input.txt").write_text("third")
        results.append({"phase": "input changed with watch", "observed": run(root), "expected": "third"})
        assert all(r["observed"] == r["expected"] for r in results), results
        print(json.dumps({"scope": "synthetic fixture only, not a corpus finding", "cargo": subprocess.check_output(["cargo", "--version"], text=True).strip(), "command": "cargo run --offline --quiet", "phases": results, "passed": True}, indent=2))


def run(root):
    return subprocess.check_output(["cargo", "run", "--offline", "--quiet"], cwd=root, text=True).strip()


if __name__ == "__main__":
    main()
