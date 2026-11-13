#!/usr/bin/env python3
"""Exercise the public CLI against tracked fixture repositories."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    here = Path(__file__).resolve().parent
    cases = [
        ("rust-different-default", {"a.rs": 'fn a(){ let _ = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string()); }', "b.rs": 'fn b(){ let _ = std::env::var("PORT").unwrap_or("4000".into()); }'}, ["PORT"], []),
        ("ts-different-default", {"a.ts": 'const a = process.env.PORT ?? "3000";', "b.ts": 'const b = process.env["PORT"] ?? "4000";'}, ["PORT"], []),
        ("ts-same-normalized-number", {"a.ts": 'const a = Number(process.env.PORT ?? "3000");', "b.ts": 'const b = Number(process.env.PORT) || 3000;'}, ["PORT"], []),
        ("same-default", {"a.rs": 'fn a(){let _=std::env::var("A").unwrap_or("same".to_owned());}', "b.rs": 'fn b(){let _=std::env::var("A").unwrap_or_else(|_| "same".into());}'}, [], []),
        ("separate-manifests", {"one/package.json": '{}', "two/package.json": '{}', "one/a.ts": 'const a=process.env.PORT ?? "3000";', "two/a.ts": 'const a=process.env.PORT ?? "4000";'}, [], ["PORT"]),
        ("inline-test-scope", {"a.rs": 'fn a(){let _=std::env::var("A").unwrap_or("production".into());}\n#[cfg(test)] mod tests { fn b(){let _=std::env::var("A").unwrap_or("fixture".into());}}'}, [], ["A"]),
        ("example-scope", {"src/a.ts": 'const a=process.env.PORT ?? "3000";', "examples/example.ts": 'const a=process.env.PORT ?? "4000";'}, [], ["PORT"]),
        ("different-provider", {"a.ts": 'const a=process.env.PORT ?? "3000";', "b.ts": 'const a=import.meta.env.PORT ?? "4000";'}, [], []),
        ("nonliteral-default", {"a.ts": 'const a=process.env.PORT ?? configuredPort;', "b.rs": 'fn a(){let _=std::env::var("PORT").unwrap_or_else(|_| default_port());}'}, [], []),
        ("multi-key-precedence", {"a.ts": 'const a=process.env.PRIMARY || process.env.SECONDARY || "4000";', "b.ts": 'const a=process.env.PRIMARY || "3000";'}, [], []),
        ("parser-difference", {"a.ts": 'const a=Number(process.env.PORT ?? "3000");', "b.ts": 'const a=parseInt(process.env.PORT ?? "3000", 10);'}, ["PORT"], []),
        ("arbitrary-same-fields", {"a.ts": 'const a=config.port ?? 3000;', "b.ts": 'const b=settings.port ?? 4000;'}, [], []),
        ("test-filenames", {"a.test.ts": 'const a=process.env.PORT ?? "3000";', "src/a.ts": 'const a=process.env.PORT ?? "4000";'}, [], ["PORT"]),
        ("rust-parse-chain", {"a.rs": 'fn a(){let _=std::env::var("A").ok().and_then(|v| v.parse::<u16>().ok()).unwrap_or(100);}', "b.rs": 'fn b(){let _=std::env::var("A").ok().and_then(|v| v.parse::<u16>().ok()).unwrap_or(200);}'}, ["A"], []),
        ("unknown-transform", {"a.ts": 'const a=other(process.env.PORT) || "3000";', "b.rs": 'fn a(){let _=std::env::var("PORT").trim().unwrap_or("4000".into());}'}, [], []),
        ("benchmark-scope", {"a.perf.tsx": 'const a=Number(process.env.PORT ?? 3000);', "src/a.ts": 'const a=Number(process.env.PORT ?? 4000);'}, [], ["PORT"]),
        ("argument-siblings-are-not-precedence", {"a.ts": 'send(process.env.A ?? "one", process.env.B ?? "b");', "b.ts": 'const a=process.env.A ?? "two";'}, ["A"], []),
        ("explicit-precedence", {"a.ts": 'const a=explicit ?? process.env.PORT ?? "4000";', "b.ts": 'const a=process.env.PORT ?? "3000";'}, [], []),
        ("dynamic-keys", {"a.ts": 'const a=process.env[key] ?? "3000";', "b.rs": 'fn a(){let _=std::env::var(key).unwrap_or("4000".into());}'}, [], []),
    ]
    outcomes = []
    with tempfile.TemporaryDirectory(prefix="scry-config-fixtures-") as scratch:
        for name, files, expected_keys, expected_cross in cases:
            root = Path(scratch) / name
            root.mkdir()
            for rel, content in files.items():
                dest = root / rel
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_text(content + "\n")
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(["git", "-C", str(root), "add", "."], check=True)
            completed = subprocess.run([sys.executable, str(here / "prototype.py"), str(root)], capture_output=True, text=True, check=True)
            result = json.loads(completed.stdout)["repositories"][0]
            actual_keys = sorted(c["key"] for c in result["candidates"])
            actual_cross = sorted(c["key"] for c in result["cross_scope_differences"])
            assert actual_keys == sorted(expected_keys), (name, "within scope", actual_keys, expected_keys, result)
            assert actual_cross == sorted(expected_cross), (name, "cross scope", actual_cross, expected_cross, result)
            if name == "ts-same-normalized-number":
                assert result["candidates"][0]["differences"] == ["fallback-semantics"], result
            if name == "nonliteral-default":
                assert len(result["unsupported"]) == 2, result
            if name == "dynamic-keys":
                assert len(result["unsupported"]) == 2, result
            outcomes.append({"case": name, "passed": True, "within_scope_keys": actual_keys, "cross_scope_keys": actual_cross})
    (here / "validation.json").write_text(json.dumps({"command": f"{sys.executable} {here / 'validate.py'}", "cases": outcomes}, indent=2) + "\n")
    print(f"{len(outcomes)} CLI fixture cases passed")


if __name__ == "__main__":
    main()
