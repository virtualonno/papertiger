"""Verify emitted planner contracts with Python's jsonschema package.

Usage: python scripts/verify_json_contracts.py <built-papertiger-binary>
Uses only a disposable authority and public CLI commands; no live state changes.
"""

import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from jsonschema import Draft202012Validator


def main():
    binary = str(Path(sys.argv[1]).resolve())
    environment = {**os.environ, "PAPERTIGER_ACTOR": "schema-fixture"}
    environment.pop("PAPERTIGER_MODEL", None)
    schema = json.loads(subprocess.check_output([binary, "schema"], env=environment))
    Draft202012Validator.check_schema(schema)
    validator = Draft202012Validator(schema)
    count = 0
    with tempfile.TemporaryDirectory(prefix="papertiger-schema-") as directory:
        database = str(Path(directory) / "fixture.sqlite")

        def run(*arguments, validate=True):
            nonlocal count
            data = subprocess.check_output([binary, "--db", database, *arguments], env=environment)
            if not validate:
                return None
            value = json.loads(data)
            validator.validate(value)
            count += 1
            return value

        run("init", validate=False)
        run("plan", "add", "source", "Source", "--json")
        run("plan", "add", "destination", "Destination", "--json")
        run("add", "Selected work", "--plan", "source", "--model", "fixture-author", "--start", "--why", "verify structured entry", "--json")
        run("add", "Related work", "--plan", "source", "--dep", "1", "--json")
        run("reference", "add", "1", "https://example.test/review", "--kind", "review", "--json")
        run("gate", "add", "1", "proof", "--kind", "test", "--requirement", "fixture proof", "--json")
        context = run("show", "1", "--json")
        run("status", "--json")
        run("list", "--plan", "source", "--json")
        run("log", "--json")
        run("export")
        run("move-plan", "1", "2", "--plan", "destination", "--why", "relocate complete set", "--json")
        run("export", "--plan", "destination")
        run("gate", "waive", "1", "proof", "--why", "test waiver", "--json")
        run("done", "1", "--result", "fixture outcome", "--model", "fixture-reviewer", "--json")
        run("show", "1", "--json")
        # Reject malformed structural values independently from runtime tests.
        for path, bad in [(('task', 'status'), 'duplicate'), (('task', 'seq'), '1'), (('activity', 'created_event', 'model'), 'invalid model')]:
            corrupt = copy.deepcopy(context)
            target = corrupt
            for key in path[:-1]:
                target = target[key]
            target[path[-1]] = bad
            assert not validator.is_valid(corrupt), path
        corrupt = copy.deepcopy(context)
        corrupt['dependents'][0]['intent'] = 'must be a summary'
        assert not validator.is_valid(corrupt)
    print(json.dumps({"schema": "papertiger.schema-fixture-proof.v1", "validated_outputs": count, "negative_controls": 4}))


if __name__ == "__main__":
    main()
