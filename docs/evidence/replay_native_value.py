"""Ordinary regression comparison using the same frozen native inputs as Mise.

Read-only toward source, authorities and CAS. Hook commands are parser input,
never shell execution. This is a disclosed fixture comparison, not RSI or a
fresh candidate-production cost benchmark.
"""
from pathlib import Path
import argparse
import hashlib
import json
import subprocess
import time


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, default=Path("F:/AI"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not __debug__:
        parser.error("Run this comparison without Python -O; input-binding checks are required")
    reports = []
    for name, attempt in [("contextmink", "a02"), ("ghidramink", "a03")]:
        project = args.workspace / name
        retained = project / f"state/mise-native-behavior-{attempt}"
        manifest_bytes = (retained / "control/campaign.json").read_bytes()
        manifest = json.loads(manifest_bytes)
        profile_bytes = (retained / "control/profile.json").read_bytes()
        profile = json.loads(profile_bytes)
        assert digest(profile_bytes) == manifest["evaluator"]["argv"][-1]
        source = Path(manifest["source"]["repository_locator"])
        fixture_path = "fixtures/mise-native/cases.json"
        fixture_bytes = (source / fixture_path).read_bytes()
        frozen_fixture = subprocess.check_output(["git", "-C", str(source), "show",
                                                  f'{manifest["source"]["base_commit"]}:{fixture_path}'])
        assert fixture_bytes == frozen_fixture
        fixture = json.loads(fixture_bytes)
        assert fixture["domain"] in ("contextmink-hook-guard", "ghidramink-pcode-environment")
        cases = fixture["cases"]
        assert len({c["key"] for c in cases}) == len(cases) > 0
        variants = []
        for tree, binding in profile["variants"].items():
            executable = Path(binding["executable"])
            assert digest(executable.read_bytes()) == binding["executable_sha256"]
            assert binding["source_tree"] == tree and binding["build_exit"] == 0
            actual_tree = subprocess.check_output(["git", "-C", str(source), "rev-parse",
                                                   binding["source_commit"] + "^{tree}"]).decode().strip()
            assert actual_tree == tree
            assert subprocess.check_output(["git", "-C", str(source), "show",
                                             f'{binding["source_commit"]}:{fixture_path}']) == frozen_fixture
            runs = []
            for repetition in range(2):
                started = time.perf_counter()
                captures = []

                def execute(argv, value):
                    result = subprocess.run([str(executable), *argv], cwd=source,
                                            input=json.dumps(value).encode(),
                                            capture_output=True, timeout=15)
                    assert len(result.stdout) <= 262144 and len(result.stderr) <= 262144
                    captures.append({"argv": [str(executable), *argv],
                                     "exit": result.returncode,
                                     "stdout": result.stdout.decode("utf-8"),
                                     "stderr": result.stderr.decode("utf-8")})
                    return result

                if name == "contextmink":
                    actual = []
                    for case in cases:
                        result = execute(["--no-config", "hook-guard", "--shell", "posix"], case["input"])
                        assert result.returncode in (0, 2)
                        actual.append(result.returncode)
                else:
                    result = execute([], [c["input"] for c in cases])
                    assert result.returncode == 0
                    rows = json.loads(result.stdout)
                    assert len(rows) == len(cases)
                    assert all(set(r) == {"key", "result"} and r["key"] == c["key"]
                               and isinstance(r["result"], list) for r, c in zip(rows, cases))
                    actual = [r["result"] for r in rows]
                mismatches = [{"key": c["key"], "expected": c["expected"], "actual": a}
                              for c, a in zip(cases, actual) if c["expected"] != a]
                unsafe = sum(not c["valid"] and a == (0 if name == "contextmink" else [])
                             for c, a in zip(cases, actual))
                runs.append({"repetition": repetition, "wall_seconds": time.perf_counter() - started,
                             "case_count": len(cases), "exact_outcomes": len(cases) - len(mismatches),
                             "unsafe_acceptances": unsafe, "mismatches": mismatches,
                             "native_invocations": len(captures), "captures": captures})
            assert runs[0]["mismatches"] == runs[1]["mismatches"]
            variants.append({"tree": tree, "source_commit": binding["source_commit"],
                             "executable": str(executable), "executable_sha256": binding["executable_sha256"],
                             "runs": runs})
        baseline, = [v for v in variants if v["tree"] == manifest["source"]["base_tree"]]
        projection_file = retained / "research-projection.json"
        projection = json.loads(projection_file.read_bytes())
        research_tree = next(v["tree"] for v in variants if "/research/" in v["executable"].replace("\\", "/"))
        research, = [v for v in variants if v["tree"] == research_tree]
        primary, = [o for o in manifest["objectives"] if o["role"] == "primary"]
        assert primary["key"] == "exact-outcomes" and primary["direction"] == "maximize"
        headroom = len(cases) - baseline["runs"][0]["exact_outcomes"]
        ordinary_accepts = (research["runs"][0]["unsafe_acceptances"] == 0
                            and research["runs"][0]["exact_outcomes"] == len(cases))
        # An ordinary regression pass is necessary, not sufficient for nomination.
        reports.append({"domain": name, "campaign": manifest["campaign_id"],
                        "manifest_sha256": digest(manifest_bytes), "profile_sha256": digest(profile_bytes),
                        "fixture_sha256": digest(fixture_bytes), "variants": variants,
                        "ordinary_research_passes_regression": ordinary_accepts,
                        "mise_recorded_disposition": projection["disposition"],
                        "projection_sha256": digest(projection_file.read_bytes()),
                        "primary_headroom_cases": headroom,
                        "minimum_practical_change": primary["minimum_practical_change"],
                        "improvement_possible_on_this_primary": headroom >= primary["minimum_practical_change"]})
        print(json.dumps({"domain": name, "baseline_exact": baseline["runs"][0]["exact_outcomes"],
                          "research_exact": research["runs"][0]["exact_outcomes"],
                          "total": len(cases), "ordinary_pass": ordinary_accepts,
                          "mise_disposition": projection["disposition"], "headroom": headroom}))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({"schema": "mise.value-comparison.v1", "reports": reports,
                                     "limitations": ["Same disclosed historical workloads and prebuilt candidates; no new candidate proposal or build.",
                                                     "Ordinary runner checks exact outputs and unsafe acceptance; it does not replace Mise lineage, calibration, CAS, budgets or recovery.",
                                                     "Warm prebuilt execution timings exclude setup, builds and operator work; do not compare them to historical campaign ledgers as end-to-end speedups.",
                                                     "Projection is a retained view; its CAS rederivation was independently checked in the accompanying historical-reopening evidence."]}, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
