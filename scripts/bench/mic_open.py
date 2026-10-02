#!/usr/bin/env python3
"""Manual CPAL first-callback probe. Never called by the automated baseline."""
import argparse
import subprocess
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bench', default='target/release/xflow-bench')
    parser.add_argument('--samples', type=int, default=5)
    args = parser.parse_args()
    if args.samples < 1:
        parser.error('samples must be positive')
    print('This opens the default microphone and discards its samples immediately.')
    print('Close other recording applications before running; audio is never saved or uploaded.')
    if input('Type OPEN MICROPHONE to consent: ') != 'OPEN MICROPHONE':
        print('Cancelled.')
        return 1
    return subprocess.call([str(Path(args.bench).resolve()), 'mic-open', '--samples', str(args.samples),
                            '--i-consent-to-open-the-microphone'])


if __name__ == '__main__':
    raise SystemExit(main())
