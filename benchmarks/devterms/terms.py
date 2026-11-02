#!/usr/bin/env python3
"""Lexicon term recall (`stt.domain_hints`) through `xflow transcribe`.

`terms.tsv` holds 2-term phrases per lexicon section: term A from the section's
top 30, term B from ranks 30-120. Each clip runs under four conditions, all
with the hints the production selector picks (`cargo run --example hints`),
cut like the provider cuts shipped terms (100 estimated tokens):

- off: no hints;
- before: the focused app only, no other signal (exactly #117's selection);
- section: plus two earlier dictated terms from the same section (not the
  clip's), the way a user who has been talking about AWS looks;
- repeat: plus the clip's own terms dictated earlier (history).

Manual and opt-in: 4 Groq requests per clip, about $0.0005 each (10 s minimum).
The CLI reads its own stored key; this script never reads, prints or passes
credentials.

    python3 benchmarks/devterms/terms.py --make-corpus   # rewrite terms.tsv from the lexicon
    python3 benchmarks/devterms/terms.py --self-test     # no network
    XFLOW_BIN=path/to/xflow python3 benchmarks/devterms/terms.py --per-section 5 \\
        --sections aws,gcp,azure,devops,sysdesign,dsa
    python3 benchmarks/devterms/terms.py --summary
"""
import argparse
import hashlib
import json
import os
import pathlib
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))
from hints import cut  # noqa: E402
from run import hits  # noqa: E402

CLI = ROOT / '../../apps/cli'
LEXICON = CLI / 'crates/xflow-core/src/hints/lexicon.txt'
CORPUS = ROOT / 'terms.tsv'
TERMINAL = 'com.apple.Terminal'
SECTIONS = ['aws', 'gcp', 'azure', 'cloud', 'devops', 'sysdesign', 'db', 'dsa', 'cs', 'ml']
PHRASES = 15
TEMPLATES = [
    'Next we should move {a} behind {b} this sprint.',
    'Can you check whether {a} still works with {b}?',
    'I set up {a} yesterday and then tried {b}.',
    'The team asked about {a} and {b} in the review.',
    'Let us compare {a} with {b} before we decide.',
]
CONDITIONS = ['off', 'before', 'section', 'repeat']


def lexicon():
    sections, current = {}, None
    for line in LEXICON.read_text().splitlines()[1:]:
        if line.startswith('@'):
            current = sections.setdefault(line[1:].split()[0], [])
        else:
            current.append(line)
    return sections


def make_corpus():
    sections = lexicon()
    lines = ['# section\tspoken text\texpected terms (|)\tsame-section context terms (|)']
    for name in SECTIONS:
        terms = sections[name]
        for i in range(PHRASES):
            a, b = terms[2 * i], terms[min(30 + 6 * i, len(terms) - 1)]
            context = terms[min(120 + 2 * i, len(terms) - 2):][:2]
            lines.append('\t'.join([name, TEMPLATES[i % len(TEMPLATES)].format(a=a, b=b),
                                    f'{a}|{b}', '|'.join(context)]))
    CORPUS.write_text('\n'.join(lines) + '\n')
    print(f'{CORPUS.name}: {len(lines) - 1} phrases, {2 * (len(lines) - 1)} terms')


def corpus():
    return [line.split('\t') for line in CORPUS.read_text().splitlines() if not line.startswith('#')]


def selections(contexts):
    """Production selections for each context, from the Rust example."""
    env = dict(os.environ, CARGO_TERM_QUIET='true')
    out = subprocess.run(['cargo', 'run', '--locked', '--quiet', '-p', 'xflow-core', '--example', 'hints'],
                         cwd=CLI, input=''.join(json.dumps(c) + '\n' for c in contexts),
                         capture_output=True, text=True, check=True, env=env)
    return [json.loads(line) for line in out.stdout.splitlines()]


def hint_lists(rows):
    contexts = []
    for _, _, expected, context in rows:
        own = expected.split('|')
        same = [t for t in context.split('|') if t]
        contexts += [
            {'app': TERMINAL, 'terminal': True, 'recent': ''},
            {'app': TERMINAL, 'terminal': True, 'recent': '. '.join(same)},
            {'app': TERMINAL, 'terminal': True, 'recent': '. '.join(same + own)},
        ]
    picked = selections(contexts)
    return [{'off': [], 'before': cut(picked[3 * i]), 'section': cut(picked[3 * i + 1]),
             'repeat': cut(picked[3 * i + 2])} for i in range(len(rows))]


def self_test():
    rows = corpus()
    assert 200 <= 2 * len(rows) <= 500, len(rows)
    lists = hint_lists(rows[:3])
    for row, hints in zip(rows, lists):
        assert hints['before'] and len(hints['before']) <= 30, hints
        assert all(t in hints['repeat'] for t in row[2].split('|')), (row, hints['repeat'])
    print(f'Self-check passed; {len(rows)} phrases, {2 * len(rows)} terms; '
          f'before sends {len(lists[0]["before"])} terms, section {len(lists[0]["section"])}', flush=True)


def summary(records):
    def line(label, batch):
        got, total = sum(r['hits'] for r in batch), sum(r['total'] for r in batch)
        return f'{label}: {got}/{total} ({100 * got / max(1, total):.0f}%)'
    sections = sorted({r['section'] for r in records}, key=SECTIONS.index)
    for condition in CONDITIONS:
        batch = [r for r in records if r['condition'] == condition]
        if not batch:
            continue
        parts = [line(s, [r for r in batch if r['section'] == s]) for s in sections]
        head = [dict(r, hits=r['hit_a'], total=1) for r in batch]
        tail = [dict(r, hits=r['hits'] - r['hit_a'], total=1) for r in batch]
        print(f'{condition:7} {line("all", batch)}; {line("top-30", head)}, {line("30-120", tail)}', flush=True)
        print('        ' + '; '.join(parts), flush=True)
        print(f'        p50 {statistics.median(r["ms"] for r in batch):.0f} ms (CLI process), '
              f'{statistics.mean(r["hints"] for r in batch):.0f} hints, '
              f'{sum(not r["text"].rstrip().endswith((".", "?", "!")) for r in batch)} without final punctuation',
              flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--make-corpus', action='store_true')
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--summary', action='store_true')
    parser.add_argument('--per-section', type=int, default=PHRASES)
    parser.add_argument('--sections', default=','.join(SECTIONS))
    args = parser.parse_args()
    if args.make_corpus:
        make_corpus()
        return
    if args.self_test:
        self_test()
        return
    result_file = ROOT / 'results' / 'terms.jsonl'
    if args.summary:
        summary([json.loads(line) for line in result_file.read_text().splitlines()])
        return
    binary = pathlib.Path(os.environ.get('XFLOW_BIN', CLI / 'target/debug/xflow')).resolve()
    if not binary.exists():
        raise SystemExit('Build xflow first, or set XFLOW_BIN.')
    wanted = args.sections.split(',')
    rows = [r for r in corpus() if r[0] in wanted]
    rows = [r for s in wanted for r in [r for r in rows if r[0] == s][:args.per_section]]
    lists = hint_lists(rows)
    audio = ROOT / 'audio'
    audio.mkdir(exist_ok=True)
    result_file.parent.mkdir(exist_ok=True)
    done = set()
    if result_file.exists():
        done = {(r['text_spoken'], r['condition']) for r in map(json.loads, result_file.read_text().splitlines())}
    for i, ((section, spoken, expected, _), hints) in enumerate(zip(rows, lists)):
        wav = audio / f'terms-{hashlib.md5(spoken.encode()).hexdigest()[:12]}.wav'
        if not wav.exists():
            aiff = wav.with_suffix('.aiff')
            subprocess.run(['say', '-v', 'Samantha', '-r', '175', '-o', str(aiff), spoken], check=True)
            subprocess.run(['afconvert', '-f', 'WAVE', '-d', 'LEI16@16000', '-c', '1', str(aiff), str(wav)],
                           check=True)
            aiff.unlink()
        order = CONDITIONS[i % 4:] + CONDITIONS[:i % 4]  # rotate so drift favours no condition
        for condition in order:
            if (spoken, condition) in done:
                continue
            vocabulary = hints[condition]
            cfg = ('[stt]\nprovider = "groq"\nmodel = "whisper-large-v3-turbo"\nlanguage = "en"\n'
                   f'timeout_secs = 60\nvocabulary = {json.dumps(vocabulary)}\n')
            with tempfile.NamedTemporaryFile(mode='w', suffix='.toml') as config:
                config.write(cfg)
                config.flush()
                time.sleep(3.1)  # below 20 requests/minute
                start = time.perf_counter()
                out = subprocess.run([str(binary), '--config', config.name, '--json', 'transcribe', str(wav)],
                                     capture_output=True, text=True, timeout=90)
                ms = (time.perf_counter() - start) * 1000
            if out.returncode:
                # stderr carries no key or body; it is safe to show.
                raise SystemExit(f'xflow failed ({out.returncode}) on {spoken!r}: {out.stderr.strip()}')
            value = json.loads(out.stdout)
            text = value.get('text', value.get('data', {}).get('text', ''))
            terms = expected.split('|')
            record = dict(section=section, condition=condition, text_spoken=spoken, text=text,
                          expected=expected, hits=hits(text, terms), hit_a=hits(text, terms[:1]),
                          total=len(terms), ms=ms, hints=len(vocabulary),
                          sent=[t for t in terms if t in vocabulary])
            with result_file.open('a') as f:
                f.write(json.dumps(record) + '\n')
        print(f'{i + 1}/{len(rows)} clips', flush=True)
    summary([json.loads(line) for line in result_file.read_text().splitlines()])


if __name__ == '__main__':
    main()
