"""Package a Papertiger release as one installable skill directory.

    python scripts/package_skill.py <release-binary-dir> <destination>

The destination is a directory to create (for a local install) or an archive
ending in .zip or .tar.gz whose single top-level entry is `papertiger/`:

    papertiger/SKILL.md
    papertiger/references/agent_integration.md
    papertiger/bin/papertiger[.exe], papertiger/bin/papertiger-mise[.exe]
    papertiger/README.md, CHANGELOG.md, MISE.md and the licenses

Installing is extracting that directory into a harness skill directory; the
skill runs bin/papertiger from beside itself. Integrity is the release
archive's checksum, verified at download.
"""
import shutil
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

TOOL = 'papertiger'
BINARIES = ('papertiger', 'papertiger-mise')
DOCUMENTS = ('README.md', 'CHANGELOG.md', 'MISE.md', 'LICENSE', 'LICENSE-SSL', 'LICENSE-VPL')
SOURCE = Path(__file__).resolve().parents[1]


def build(binaries: Path, skill: Path) -> None:
    suffix = '.exe' if (binaries / 'papertiger.exe').is_file() else ''
    skill.mkdir(parents=True)
    (skill / 'bin').mkdir()
    (skill / 'references').mkdir()
    for name in BINARIES:
        source = binaries / (name + suffix)
        if not source.is_file():
            sys.exit(f'package_skill: missing release binary {source}')
        shutil.copy2(source, skill / 'bin' / source.name)
    shutil.copyfile(SOURCE / 'templates/skills' / TOOL / 'SKILL.md', skill / 'SKILL.md')
    shutil.copyfile(SOURCE / 'templates/agent_integration.md',
                    skill / 'references/agent_integration.md')
    for name in DOCUMENTS:
        shutil.copyfile(SOURCE / name, skill / name)


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    binaries = Path(sys.argv[1]).resolve(strict=True)
    destination = Path(sys.argv[2]).resolve()
    if destination.exists():
        sys.exit(f'package_skill: {destination} already exists; choose a new destination')
    archive = destination.name.endswith(('.zip', '.tar.gz'))
    if not archive:
        build(binaries, destination)
        print(f'{TOOL}: skill directory at {destination}')
        return
    with tempfile.TemporaryDirectory(prefix=TOOL + '-package-') as stage:
        skill = Path(stage) / TOOL
        build(binaries, skill)
        files = sorted(p for p in skill.rglob('*') if p.is_file())
        if destination.name.endswith('.zip'):
            with zipfile.ZipFile(destination, 'x', zipfile.ZIP_DEFLATED) as output:
                for path in files:
                    output.write(path, path.relative_to(stage).as_posix())
        else:
            with tarfile.open(destination, 'x:gz') as output:
                for path in files:
                    output.add(path, arcname=path.relative_to(stage).as_posix(), recursive=False)
    print(f'{TOOL}: skill archive at {destination}')


if __name__ == '__main__':
    main()
