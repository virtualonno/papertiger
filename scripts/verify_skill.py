"""Exercise an extracted Papertiger skill directory as an agent would use it.

    python scripts/verify_skill.py <extracted>/papertiger <expected-version>

Checks the layout, then runs the skill's own binaries against a fresh Git
project: a bare init refuses to create an authority, init through
--project-root adds the ignore rules, a nested
directory finds the authority, a directory outside every project refuses,
and Mise opens its separate authority beside the planner's.
"""
import os
import subprocess
import sys
import tempfile
from pathlib import Path

TOOL = 'papertiger'
skill = Path(sys.argv[1]).resolve(strict=True)
version = sys.argv[2]
suffix = '.exe' if os.name == 'nt' else ''
planner = skill / 'bin' / (TOOL + suffix)
mise = skill / 'bin' / (TOOL + '-mise' + suffix)

expected = {
    'SKILL.md', 'references/agent_integration.md', 'bin/' + planner.name, 'bin/' + mise.name,
    'README.md', 'CHANGELOG.md', 'MISE.md', 'LICENSE', 'LICENSE-SSL', 'LICENSE-VPL',
}
present = {p.relative_to(skill).as_posix() for p in skill.rglob('*') if p.is_file()}
assert present == expected, (sorted(present - expected), sorted(expected - present))
body = (skill / 'SKILL.md').read_text(encoding='utf-8')
assert 'bin/papertiger' in body and 'references/agent_integration.md' in body
assert 'tools/papertiger' not in body and 'setup-project' not in body

# Run as the operator outside any agent harness, whatever launched the check.
HARNESS_PREFIXES = ('PAPERTIGER_', 'CLAUDE', 'CODEX_', 'PI_', 'OPENCODE_', 'CURSOR_')
env = {k: v for k, v in os.environ.items() if not k.upper().startswith(HARNESS_PREFIXES)}
env['PAPERTIGER_MODEL'] = 'human'
env['PAPERTIGER_SESSION'] = 'skill-smoke'


def run(binary, *args, cwd, success=True):
    result = subprocess.run([str(binary), *map(str, args)], cwd=cwd, env=env,
                            capture_output=True, text=True)
    assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
    return result.stdout if success else result.stderr


with tempfile.TemporaryDirectory(prefix=TOOL + ' skill smoke ') as directory:
    root = Path(directory)
    project = root / 'consumer project'
    nested = project / 'nested' / 'work'
    outside = root / 'outside'
    nested.mkdir(parents=True)
    outside.mkdir()
    (project / '.git').mkdir()
    (project / '.gitignore').write_text('target/\n')
    (project / 'AGENTS.md').write_text('repository contract\n')

    assert run(planner, '--version', cwd=root).strip() == f'{TOOL} {version}'
    assert run(mise, '--version', cwd=root).strip() == f'{TOOL}-mise {version}'
    for removed in ('setup-project', 'setup-user'):
        assert 'unrecognized subcommand' in run(planner, removed, cwd=root, success=False)

    missing = run(planner, 'status', cwd=outside, success=False)
    assert 'no Papertiger authority' in missing and 'init' in missing, missing
    bare = run(planner, 'init', cwd=nested, success=False)
    assert '--project-root <project-root> init' in bare, bare
    assert not (nested / 'state').exists() and not (project / 'state').exists()
    created = run(planner, '--project-root', project, 'init', cwd=outside)
    assert 'added Papertiger' in created, created
    assert '/state/papertiger.sqlite' in (project / '.gitignore').read_text()
    run(planner, 'plan', 'add', 'work', 'Project work', '--intent', 'Preserve outcomes', cwd=nested)
    run(planner, 'add', 'Existing outcome', '--kind', 'feature', '--plan', 'work', cwd=nested)
    assert 'F-1 Existing outcome' in run(planner, 'list', cwd=nested)
    run(planner, 'audit', cwd=nested)
    assert not (nested / 'state').exists()
    assert not (outside / 'state').exists()
    assert (project / 'AGENTS.md').read_text() == 'repository contract\n'

    assert '"initialized": false' in run(mise, '--project-root', project, 'status', '--json', cwd=root)
    run(mise, '--project-root', project, 'init', cwd=root)
    assert '"initialized": true' in run(mise, '--project-root', project, 'status', '--json', cwd=root)
print(f'{TOOL}: skill directory smoke passed')
