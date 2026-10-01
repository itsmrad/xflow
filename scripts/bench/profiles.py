#!/usr/bin/env python3
"""Compare release profiles through the shared cargo gate, using isolated daemons."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

import baseline

PROFILES = {
    'thin-3-cgu1': {},
    'fat-3-cgu1': {'LTO': 'fat'},
    'thin-s-cgu1': {'OPT_LEVEL': 's'},
    'thin-3-abort-cgu1': {'PANIC': 'abort'},
    'thin-3-cgu16': {'CODEGEN_UNITS': '16'},
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True)
    parser.add_argument('--cargo', default='/home/mrad/.cache/xflow-dev/bin/cargo')
    parser.add_argument('--profiles', default=','.join(PROFILES))
    parser.add_argument('--idle-seconds', type=float, default=60)
    parser.add_argument('--resume', action='store_true', help='retain completed profiles in the output file')
    args = parser.parse_args()
    names = args.profiles.split(',')
    if any(name not in PROFILES for name in names):
        parser.error('unknown profile')
    output = Path(args.output).resolve()
    result = json.loads(output.read_text()) if args.resume and output.exists() else {
        'schema_version': 1, 'profiles': {}, 'selection': None}
    result['selection'] = None
    # Binary copies belong on the checkout filesystem, not size-limited /tmp.
    with tempfile.TemporaryDirectory(prefix='xflow-profiles-', dir=baseline.ROOT / 'target') as temp:
        for name in names:
            overrides = {'LTO': 'thin', 'OPT_LEVEL': '3', 'PANIC': 'unwind', 'CODEGEN_UNITS': '1',
                         'STRIP': 'true', **PROFILES[name]}
            env = {k: v for k, v in os.environ.items() if not k.startswith('CARGO_PROFILE_RELEASE_')}
            env['TMPDIR'] = temp
            env.update({f'CARGO_PROFILE_RELEASE_{k}': v for k, v in overrides.items()})
            command = [str(Path(args.cargo).resolve()), 'build', '--release', '--workspace', '--locked', '--offline']
            started = time.monotonic()
            subprocess.run(command, cwd=baseline.ROOT, env=env, check=True)
            build_seconds = time.monotonic() - started
            folder = Path(temp) / name
            folder.mkdir()
            for binary in ('xflow', 'xflowd'):
                shutil.copy2(baseline.ROOT / 'target/release' / binary, folder / binary)
            params = argparse.Namespace(daemon=str(folder / 'xflowd'), cli=str(folder / 'xflow'),
                output=str(folder / 'measurement.json'), bench=None, idle_seconds=args.idle_seconds, warmup_seconds=5,
                startup_samples=30, samples=200, profile_label=name, network=False, network_samples=3)
            measurement = baseline.run(params)
            if measurement['error']:
                raise RuntimeError(measurement['error'])
            result['profiles'][name] = {'overrides': overrides, 'build_seconds': build_seconds,
                                       'build_command': command, 'baseline': measurement}
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
    # Deliberately leave the decision to the evidence review; the script does not
    # edit Cargo.toml, restore binaries or assume smaller means faster.


if __name__ == '__main__':
    main()
