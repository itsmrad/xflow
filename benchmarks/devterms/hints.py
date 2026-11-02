#!/usr/bin/env python3
"""Shipped domain hints (`stt.domain_hints`) off vs on, through `xflow transcribe`.

Manual and opt-in: 2 Groq requests per phrase (about $0.01 in all). The CLI reads
its own stored Groq key; this script never reads, prints or passes credentials.
"On" sends the hints the daemon would send for the phrase's focused app, as
`stt.vocabulary`, cut like the CLI cuts shipped terms (100 estimated tokens
including the closing period); "off" sends no prompt.

    python3 benchmarks/devterms/hints.py --self-test   # no network
    XFLOW_BIN=path/to/xflow python3 benchmarks/devterms/hints.py
    python3 benchmarks/devterms/hints.py --summary
"""
import argparse
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
from run import hits, tokens  # noqa: E402

HINTS = ROOT / '../../scripts/lexicon/data/hand'
DOMAINS = ['dev', 'corp', 'sheet', 'finance']
MAX = 100
TERMINAL, MAIL, EXCEL = 'com.apple.Terminal', 'com.apple.mail', 'com.microsoft.Excel'

# (domain, focused app, spoken text, expected spellings). Controls must stay plain English.
PHRASES = [
    ('dev', TERMINAL, 'Ask Claude to review the PR before we merge.', 'Claude'),
    ('dev', TERMINAL, 'Run kubectl get pods in the staging namespace.', 'kubectl'),
    ('dev', TERMINAL, 'Deploy to Vercel with Supabase as the database.', 'Vercel|Supabase'),
    ('dev', TERMINAL, 'Open Claude Code and fix the failing test.', 'Claude Code'),
    ('dev', TERMINAL, 'We moved the API to Next.js and tRPC last week.', 'Next.js|tRPC'),
    ('dev', TERMINAL, 'Install the shadcn components with pnpm.', 'shadcn|pnpm'),
    ('dev', TERMINAL, 'The Postgres schema lives in Prisma now.', 'Postgres|Prisma'),
    ('dev', TERMINAL, 'Point the Ollama model at the Groq endpoint.', 'Ollama|Groq'),
    ('dev', TERMINAL, 'Add a Zod schema to the Hono route.', 'Zod|Hono'),
    ('dev', TERMINAL, 'Restart nginx and check the Grafana dashboard.', 'nginx|Grafana'),
    ('corp', MAIL, 'Update the opportunity in Salesforce and HubSpot.', 'Salesforce|HubSpot'),
    ('corp', MAIL, 'File a Jira ticket and link the Confluence page.', 'Jira|Confluence'),
    ('corp', MAIL, 'Our SOC 2 audit and the GDPR review are due in March.', 'SOC 2|GDPR'),
    ('corp', MAIL, 'Send the NDA through DocuSign today.', 'NDA|DocuSign'),
    ('corp', MAIL, 'Share the OKR deck in Microsoft Teams.', 'OKR|Microsoft Teams'),
    ('finance', MAIL, 'The EBITDA margin improved this quarter.', 'EBITDA'),
    ('finance', MAIL, 'We report under GAAP, not IFRS.', 'GAAP|IFRS'),
    ('finance', MAIL, 'Check the WACC and IRR in the DCF model.', 'WACC|IRR|DCF'),
    ('finance', MAIL, 'Complete KYC before the IBAN transfer.', 'KYC|IBAN'),
    ('finance', MAIL, 'Our capex and opex both grew this year.', 'capex|opex'),
    ('sheet', EXCEL, 'Use XLOOKUP not VLOOKUP for this table.', 'XLOOKUP|VLOOKUP'),
    ('sheet', EXCEL, 'Wrap it in IFERROR and add a SUMIFS total.', 'IFERROR|SUMIFS'),
    ('sheet', EXCEL, 'Count the rows with COUNTIFS by region.', 'COUNTIFS'),
    ('sheet', EXCEL, 'Use TEXTJOIN to merge the names.', 'TEXTJOIN'),
    ('sheet', EXCEL, 'Use EOMONTH and NETWORKDAYS for the dates.', 'EOMONTH|NETWORKDAYS'),
    ('control', TERMINAL, 'Store the backups in cloud storage.', 'cloud'),
    ('control', EXCEL, 'The clerk said the code was ready.', 'clerk|code'),
]


def lists():
    return {d: (HINTS / f'{d}.txt').read_text().splitlines() for d in DOMAINS}


def domain_hints(app, finance):
    """Mirror of xflow_core::hints::select for the default [vocab] (plus finance)."""
    on = {'dev': True, 'corp': True, 'sheet': False, 'finance': finance}
    focus = {'dev': app == TERMINAL, 'corp': False, 'sheet': app == EXCEL, 'finance': False}
    active = [d for d in DOMAINS if on[d] or focus[d]]
    first = [d for d in active if focus[d]]
    rest = [d for d in active if not focus[d]]
    terms = lists()
    def interleave(group):
        return [terms[d][r] for r in range(MAX) for d in group if r < len(terms[d])]
    out = interleave(first)[:MAX if not rest else MAX * 2 // 3]
    return out + interleave(rest)[:MAX - len(out)]


def cut(terms, limit=100):
    """Mirror of push_hints for shipped terms: the prompt is `a, b, c.`."""
    sent, size, cost = [], 1, 1
    for term in terms:
        extra = (len(term.encode()), tokens(term)) if not sent else (len(term.encode()) + 2, 1 + tokens(' ' + term))
        if size + extra[0] > 896 or size + extra[0] > limit + 24 and cost + extra[1] > limit:
            continue
        sent.append(term)
        size, cost = size + extra[0], cost + extra[1]
    return sent


def self_test():
    terms = lists()
    assert domain_hints(MAIL, False)[:4] == ['Claude', 'Salesforce', 'Claude Code', 'HubSpot']
    assert domain_hints(TERMINAL, False)[:66] == terms['dev'][:66]
    assert domain_hints(EXCEL, False)[:len(terms['sheet'])] == terms['sheet']
    assert 'EBITDA' in domain_hints(MAIL, True)[:3]
    sent = cut(domain_hints(TERMINAL, False))
    prompt = ', '.join(sent) + '.'
    assert 90 <= tokens(prompt) <= 100, prompt
    for _, _, _, expected in PHRASES:
        assert all(e.strip() for e in expected.split('|'))
    print(f'Self-check passed; {len(PHRASES)} phrases, terminal prompt: {len(sent)} terms, '
          f'{len(prompt.encode())} bytes, {tokens(prompt)} est. tokens', flush=True)


def summary(records):
    for kind in ['dev', 'corp', 'finance', 'sheet', 'control', 'all terms']:
        for hinted in [False, True]:
            batch = [r for r in records if r['hinted'] == hinted
                     and (r['domain'] == kind or kind == 'all terms' and r['domain'] != 'control')]
            if batch:
                got, total = sum(r['hits'] for r in batch), sum(r['total'] for r in batch)
                print(f'{kind:9} hints {"on " if hinted else "off"}: {got}/{total} '
                      f'({100 * got / total:.0f}%)', flush=True)
    def shouting(text):
        letters = [c for c in text if c.isalpha()]
        return sum(c.isupper() for c in letters) > 0.6 * max(1, len(letters))
    for hinted in [False, True]:
        batch = [r for r in records if r['hinted'] == hinted]
        if batch:
            print(f'hints {"on " if hinted else "off"}: {sum(not r["text"].rstrip().endswith((".", "?", "!")) for r in batch)} '
                  f'without a final period, {sum(shouting(r["text"]) for r in batch)} mostly upper case', flush=True)
            print(f'hints {"on " if hinted else "off"}: p50 {statistics.median(r["ms"] for r in batch):.0f} ms '
                  f'(CLI process, {len(batch)} clips), upload {statistics.mean(r["wav_bytes"] for r in batch):.0f} '
                  f'B audio + {statistics.mean(r["prompt_bytes"] for r in batch):.0f} B prompt', flush=True)
    off = {r['index']: r['ms'] for r in records if not r['hinted']}
    paired = [r['ms'] - off[r['index']] for r in records if r['hinted'] and r['index'] in off]
    if paired:
        print(f'hints on: paired median {statistics.median(paired):+.0f} ms per clip', flush=True)
    misses = [r for r in records if r['hinted'] and r['hits'] < r['total']]
    for r in misses:
        print(f'  on-miss: {r["text"]!r} (expected {r["expected"]})', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--summary', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    result_file = ROOT / 'results' / 'hints.jsonl'
    if args.summary:
        summary([json.loads(line) for line in result_file.read_text().splitlines()])
        return
    binary = pathlib.Path(os.environ.get('XFLOW_BIN', ROOT / '../../apps/cli/target/debug/xflow')).resolve()
    if not binary.exists():
        raise SystemExit('Build xflow first, or set XFLOW_BIN.')
    audio = ROOT / 'audio'
    audio.mkdir(exist_ok=True)
    result_file.parent.mkdir(exist_ok=True)
    records = []
    for i, (domain, app, spoken, expected) in enumerate(PHRASES):
        wav = audio / f'hints-{i:02}.wav'
        if not wav.exists():
            aiff = wav.with_suffix('.aiff')
            subprocess.run(['say', '-v', 'Samantha', '-r', '175', '-o', str(aiff), spoken], check=True)
            subprocess.run(['afconvert', '-f', 'WAVE', '-d', 'LEI16@16000', '-c', '1', str(aiff), str(wav)],
                           check=True)
            aiff.unlink()
        hints = cut(domain_hints(app, domain == 'finance'))
        # Alternate the order so drift and connection reuse don't favour one side.
        for hinted in ([False, True] if i % 2 else [True, False]):
            vocabulary = hints if hinted else []
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
                raise SystemExit(f'xflow failed ({out.returncode}) on phrase {i}: {out.stderr.strip()}')
            value = json.loads(out.stdout)
            text = value.get('text', value.get('data', {}).get('text', ''))
            terms = expected.split('|')
            record = dict(index=i, domain=domain, hinted=hinted, text=text, expected=expected,
                          hits=hits(text, terms), total=len(terms), ms=ms,
                          wav_bytes=wav.stat().st_size, hints=len(vocabulary),
                          prompt_bytes=len(', '.join(vocabulary).encode()) + bool(vocabulary))
            records.append(record)
            with result_file.open('a') as f:
                f.write(json.dumps(record) + '\n')
        print(f'{i + 1}/{len(PHRASES)} phrases', flush=True)
    summary(records)


if __name__ == '__main__':
    main()
