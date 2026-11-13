"""CLI fixtures exercise Cargo watch semantics and deliberate abstentions."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

PROBE = Path(__file__).with_name("probe.py")


class BuildProbeTests(unittest.TestCase):
    def test_missing_inputs_and_covered_inputs_through_cli(self):
        cases = {
            "missing_file": ('println!("cargo::rerun-if-changed=build.rs"); let _ = std::fs::read_to_string("assets/schema.json");', ["assets/schema.json"], [], []),
            "directory_watch": ('println!("cargo:rerun-if-changed=assets"); let _ = std::fs::read_to_string("assets/schema.json");', [], [], ["assets/schema.json"]),
            "default_scan": ('let _ = std::fs::read_to_string("assets/schema.json");', [], [], ["assets/schema.json"]),
            "env_no_directives": ('let _ = std::env::var("SCHEMA_MODE");', ["SCHEMA_MODE"], [], []),
            "env_only_file_gap": ('println!("cargo:rerun-if-env-changed=MODE"); let _ = std::fs::read_to_string("assets/schema.json");', ["assets/schema.json"], [], []),
            "cargo_home_user_env": ('let _ = std::env::var("CARGO_HOME");', ["CARGO_HOME"], [], []),
            "cargo_env": ('let _ = std::env::var("TARGET");', [], [], []),
            "env_macro": ('let _ = option_env!("SCHEMA_MODE"); let _ = std::env::var("SCHEMA_MODE");', [], [], ["SCHEMA_MODE"]),
            "helper_unknown": ('helper_build::setup(); let _ = std::env::var("SCHEMA_MODE");', [], ["SCHEMA_MODE"], []),
            "explicit_env": ('println!("cargo::rerun-if-env-changed=SCHEMA_MODE"); let _ = std::env::var("SCHEMA_MODE");', [], [], ["SCHEMA_MODE"]),
            "symbolic_watch": ('let key = "SCHEMA_MODE"; println!("cargo:rerun-if-env-changed={key}"); let _ = std::env::var(key);', [], [], ["SCHEMA_MODE"]),
            "joined_path": ('let path = std::path::Path::new("assets").join("schema.json"); println!("cargo:rerun-if-changed={}", path.display()); let _ = std::fs::read_to_string(&path);', [], [], ["assets/schema.json"]),
            "comment_not_watch": ('// println!("cargo:rerun-if-env-changed=SCHEMA_MODE");\nlet _ = std::env::var("SCHEMA_MODE");', ["SCHEMA_MODE"], [], []),
            "unreachable_read": ('println!("cargo:rerun-if-changed=build.rs");', [], [], []),
        }
        with tempfile.TemporaryDirectory(prefix="scry-build-fixtures-") as tmp:
            roots = []
            for name, (body, *_expectations) in cases.items():
                extra = 'fn unused() { let _ = std::env::var("UNUSED"); }' if name == "unreachable_read" else ""
                roots.append(make_repo(Path(tmp) / name, "fn main() {" + body + "}\n" + extra))
            result = run_probe(roots)
            for repo in result["repositories"]:
                with self.subTest(repo=repo["repo"]):
                    script = repo["scripts"][0]
                    _, expected, unknown, covered = cases[repo["repo"]]
                    self.assertEqual([f["value"] for f in script["candidates"]], expected)
                    self.assertEqual([f["value"] for f in script["unknown"]], unknown)
                    self.assertEqual([f["value"] for f in script["covered"]], covered)

    def test_manifest_selected_scripts_only(self):
        with tempfile.TemporaryDirectory(prefix="scry-build-manifest-") as tmp:
            root = make_repo(Path(tmp) / "custom", 'fn main() { let _ = std::env::var("SPECIAL"); }', custom="scripts/generate.rs")
            (root / "src").mkdir()
            (root / "src/build.rs").write_text('fn main() { let _ = std::env::var("NOT_A_BUILD_SCRIPT"); }')
            subprocess.run(["git", "-C", str(root), "add", "."], check=True, capture_output=True)
            result = run_probe([root])["repositories"][0]
            self.assertEqual(result["counts"]["build_scripts"], 1)
            self.assertEqual(result["scripts"][0]["path"], "scripts/generate.rs")
            self.assertEqual(result["scripts"][0]["candidates"][0]["value"], "SPECIAL")


def make_repo(root, build, custom=None):
    root.mkdir()
    (root / "assets").mkdir()
    (root / "assets/schema.json").write_text('{}\n')
    manifest = '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\n'
    if custom:
        manifest += f'build = "{custom}"\n'
    (root / "Cargo.toml").write_text(manifest)
    script = root / (custom or "build.rs")
    script.parent.mkdir(exist_ok=True)
    script.write_text(build)
    subprocess.run(["git", "init", "-q", str(root)], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(root), "add", "."], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(root), "-c", "user.email=fixture@example.invalid", "-c", "user.name=Fixture", "commit", "-qm", "fixture"], check=True, capture_output=True)
    return root


def run_probe(roots):
    process = subprocess.run([sys.executable, str(PROBE), *map(str, roots)], check=True, capture_output=True, text=True)
    return json.loads(process.stdout)


if __name__ == "__main__":
    unittest.main()
