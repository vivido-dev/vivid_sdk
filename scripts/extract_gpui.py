#!/usr/bin/env python3
"""Extract GPUI and its local dependency closure from a supplied Zed checkout."""
import argparse
import json
import pathlib
import shutil
import subprocess
import tomllib


def value(item):
    if isinstance(item, dict):
        return '{ ' + ', '.join(f'{json.dumps(k)} = {value(v)}' for k, v in item.items()) + ' }'
    if isinstance(item, list):
        return '[' + ', '.join(map(value, item)) + ']'
    if isinstance(item, bool):
        return str(item).lower()
    return json.dumps(item)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=pathlib.Path)
    args = parser.parse_args()
    source = args.source.resolve()
    destination = pathlib.Path(__file__).resolve().parents[1] / 'gui'
    if destination.exists():
        parser.error(f'{destination} already exists; extraction never overwrites local edits')
    root = tomllib.loads((source / 'Cargo.toml').read_text())
    dependencies = root['workspace']['dependencies']
    pending = ['gpui', 'gpui_platform']
    local = set()
    used = set()
    while pending:
        name = pending.pop()
        if name in local:
            continue
        local.add(name)
        manifest = tomllib.loads((source / dependencies[name]['path'] / 'Cargo.toml').read_text())
        for table in [manifest, *manifest.get('target', {}).values()]:
            for section in ['dependencies', 'dev-dependencies', 'build-dependencies']:
                for dep, spec in table.get(section, {}).items():
                    if isinstance(spec, dict) and spec.get('workspace'):
                        used.add(dep)
                        resolved = dependencies[dep]
                        if isinstance(resolved, dict) and 'path' in resolved:
                            pending.append(dep)
    for relative in sorted({dependencies[name]["path"] for name in local}):
        shutil.copytree(source / relative, destination / relative,
                        ignore=shutil.ignore_patterns('target', '.git'), dirs_exist_ok=True)
    # Upstream rendering tests embed these fonts using paths relative to crates/.
    shutil.copytree(source / 'assets/fonts', destination / 'assets/fonts')
    lines = ['[workspace]', 'resolver = "3"',
             'members = ' + value(sorted({dependencies[n]['path'] for n in local})),
             '\n[workspace.package]']
    lines.extend(f'{k} = {value(v)}' for k, v in root['workspace']['package'].items())
    lines.append('\n[workspace.dependencies]')
    # Root patch tables are ignored when the SDK consumes these crates by path.
    # Put upstream source overrides on the inherited dependencies themselves.
    patches = root.get('patch', {}).get('crates-io', {})
    for key in sorted(used):
        spec = dependencies[key]
        if key in patches and 'path' not in patches[key]:
            spec = dict(spec) if isinstance(spec, dict) else {'version': spec}
            spec.pop('version', None)
            spec.update(patches[key])
        lines.append(f'{key} = {value(spec)}')
    for group, settings in root['workspace'].get('lints', {}).items():
        lines.append(f'\n[workspace.lints.{group}]')
        lines.extend(f'{k} = {value(v)}' for k, v in settings.items())
    (destination / 'Cargo.toml').write_text('\n'.join(lines) + '\n')
    revision = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
    (destination / 'UPSTREAM.md').write_text(
        '# GPUI source provenance\n\nSource: https://github.com/zed-industries/zed\n\n'
        f'Revision: `{revision}`\n\n'
        'Extracted by `../scripts/extract_gpui.py`; includes the recursive local build, '
        'test, and optional dependency closure of gpui and gpui_platform. '
        'Original crate names and license files are preserved. '
        'This workspace is independent of the SDK protocol workspace.\n')
    print(f'Extracted {len(local)} local crates into {destination}')


if __name__ == '__main__':
    main()
