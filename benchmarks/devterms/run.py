#!/usr/bin/env python3
"""Generate two-voice audio and A/B prompts through `xflow transcribe`.

GROQ_API_KEY must already be set; no credential-store access is performed.
A loopback proxy changes only the prompt for sentence-format A/B testing.
"""
import argparse
import concurrent.futures
import http.server
import json
import math
import os
import pathlib
import re
import statistics
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import wave

ROOT = pathlib.Path(__file__).resolve().parent
BPE = ROOT / '../../apps/cli/crates/xflow-providers/src/bpe'
MERGES = [{token.replace('_', ' '): rank for rank, token in enumerate(
    (BPE / f'{name}-merges.txt').read_text().split())} for name in ['gpt2', 'whisper']]


def tokens(prompt):
    total = 0
    for piece in re.findall(r"'s|'t|'re|'ve|'m|'ll|'d| ?[^\W\d_]+|.", prompt, re.UNICODE):
        if piece.isascii() and piece.lstrip(' ').isalpha():
            counts = []
            for ranks in MERGES:
                parts = list(piece)
                while True:
                    pairs = [(ranks[a + b], i) for i, (a, b) in enumerate(zip(parts, parts[1:]))
                             if a + b in ranks]
                    if not pairs:
                        break
                    _, i = min(pairs)
                    parts[i:i + 2] = [parts[i] + parts[i + 1]]
                counts.append(len(parts))
            total += max(counts)
        else:
            total += len(piece.encode())
    return total


def select(terms, variant):
    sent = []
    for term in terms:
        prompt = ', '.join(sent + [term])
        if variant == 'sentence':
            prompt = f'Glossary: {prompt}.'
        if variant == 'old':
            if len(prompt.encode()) > 224:
                break
        elif len(prompt.encode()) > 896 or len(prompt.encode()) > 224 and tokens(prompt) > 200:
            continue
        sent.append(term)
    return sent


def hits(text, expected):
    # Literal, case-sensitive spelling with identifier boundaries; no fuzzy matching.
    return sum(bool(re.search(r'(?<!\w)' + re.escape(term) + r'(?!\w)', text))
               for term in expected)


def rows():
    return [line.split('\t') for line in (ROOT / 'phrases.tsv').read_text().splitlines()
            if line and not line.startswith('#')]


def generate(cases, voices):
    audio = ROOT / 'audio'
    audio.mkdir(exist_ok=True)
    def generate_voice(voice):
        for i, (spoken, _) in enumerate(cases):
            wav = audio / f'{voice}-{i:03}.wav'
            if wav.exists() and wav.stat().st_size > 44:
                continue
            aiff = wav.with_suffix('.aiff')
            converted = wav.with_suffix('.partial.wav')
            subprocess.run(['say', '-v', voice, '-r', '175', '-o', str(aiff), spoken], check=True)
            subprocess.run(['afconvert', '-f', 'WAVE', '-d', 'LEI16@16000', '-c', '1',
                            str(aiff), str(converted)], check=True)
            converted.replace(wav)
            aiff.unlink()
        print(f'Generated {len(cases)} WAVs: {voice}', flush=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        list(pool.map(generate_voice, voices))
    # Deterministic #51 regression set mirrors daemon::speech_free_sessions... .
    silence = [0.0] * 40000
    click = silence.copy()
    click[400:720] = [0.5] * 320
    thump = [amp * math.sin(i * math.tau * 220 / 16000) for i, amp in enumerate(
        [0.001] * 8000 + [0.04] * 4800 + [0.001] * 27200)]
    for name, samples in [('silence', silence), ('hiss', [0.003, -0.003] * 20000),
                          ('click', click), ('thump', thump)]:
        import array
        with wave.open(str(audio / f'{name}.wav'), 'wb') as out:
            out.setparams((1, 2, 16000, 0, 'NONE', 'not compressed'))
            out.writeframes(array.array('h', [round(x * 32767) for x in samples]).tobytes())


def with_prompt(body, content_type, prompt):
    boundary = content_type.split('boundary=', 1)[1].strip('"').encode()
    closing = b'--' + boundary + b'--\r\n'
    assert body.endswith(closing)
    part = (b'--' + boundary + b'\r\nContent-Disposition: form-data; name="prompt"\r\n\r\n'
            + prompt.encode() + b'\r\n')
    return body[:-len(closing)] + part + closing


class Proxy(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = self.rfile.read(int(self.headers['Content-Length']))
        # Inject only the prompt; preserve the CLI's encoded audio and other fields.
        body = with_prompt(body, self.headers['Content-Type'], self.server.prompts[self.path])
        model = self.path.split('/')[1]
        pace = self.server.pacing[model]
        request = urllib.request.Request('https://api.groq.com/openai/v1/audio/transcriptions',
            data=body, headers={'User-Agent': 'xflow-devterms/1.0', 'Content-Type': self.headers['Content-Type'],
                               'Authorization': 'Bearer ' + os.environ['GROQ_API_KEY']})
        for attempt in range(8):
            # Pace attempts too, including benchmark-only 429 quota recovery.
            with pace['lock']:
                time.sleep(max(0, pace['next'] - time.monotonic()))
                pace['next'] = time.monotonic() + 4.0
            start = time.perf_counter()
            try:
                response = urllib.request.urlopen(request, timeout=90)
            except urllib.error.HTTPError as error:
                response = error
            except Exception:
                self.send_error(502)
                return
            with response:
                payload = response.read()
                if response.code == 429 and attempt < 7:
                    delay = min(60, max(3.1, float(response.headers.get('retry-after', '10'))))
                    # Benchmark-only quota recovery; never change product retry policy.
                    message = json.loads(payload).get('error', {}).get('message', '')
                    units = re.findall(r'\b(?:RPM|RPD|ASPH|ASPD|TPM|TPD)\b', message)
                    limits = re.findall(r'(Limit|Used|Requested)\s*([\d.]+)', message)
                    print(f'Groq quota pause: {delay:.1f}s ({model}); {units} {limits}', flush=True)
                    time.sleep(delay)
                    continue
                self.server.timings[self.path] = (time.perf_counter() - start) * 1000
                self.send_response(response.code)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
                return



def summary(records):
    for model in sorted({r['model'] for r in records}):
        for variant in ['old', 'list', 'sentence']:
            batch = [r for r in records if r['model'] == model and r['variant'] == variant
                     and r['kind'] == 'speech']
            if batch:
                got, total = sum(r['hits'] for r in batch), sum(r['total'] for r in batch)
                print(f'{model} {variant}: {got}/{total} ({100 * got / total:.2f}%), '
                      f'p50 {statistics.median(r["stt_ms"] for r in batch):.1f} ms, '
                      f'{len(batch)} clips', flush=True)
    silent = [r for r in records if r['kind'] == 'silent']
    if silent:
        print(f'Silent audio: {len(silent)} raw outputs retained in results/transcripts.jsonl', flush=True)


def self_test():
    assert len(rows()) >= 150
    assert len({t for _, expected in rows() for t in expected.split('|')}) >= 300
    assert hits('React, Next.js and useState.', ['React', 'Next.js', 'useState']) == 3
    assert hits('preReact and usestate', ['React', 'useState']) == 0
    for line in (ROOT / 'tokens.tsv').read_text().splitlines()[1:]:
        term, g, sg, w, sw = line.split('\t')
        assert tokens(term) >= max(int(g), int(w)), term
        assert tokens(' ' + term) >= max(int(sg), int(sw)), term
    for variant in ['list', 'sentence']:
        selected = select([f'term{i}' for i in range(60)], variant)
        prompt = ', '.join(selected)
        if variant == 'sentence':
            prompt = f'Glossary: {prompt}.'
        assert 180 <= tokens(prompt) <= 200 and len(prompt.encode()) <= 896
    assert select(['Q' * 180, 'Z' * 80, 'x'], 'list') == ['Q' * 180, 'x']
    audio = b'\xff\x00\x01'
    original = b'--test\r\nfile-data:' + audio + b'\r\n--test--\r\n'
    updated = with_prompt(original, 'multipart/form-data; boundary=test', 'Glossary: React.')
    assert audio in updated and updated.endswith(b'--test--\r\n')
    assert b'name="prompt"\r\n\r\nGlossary: React.\r\n' in updated
    print('Corpus/scoring/estimator self-check passed', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--generate-only', action='store_true')
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--summary', action='store_true')
    parser.add_argument('--limit', type=int, default=150, help='smoke-test phrase count (default: all 150)')
    parser.add_argument('--workers', type=int, default=2)
    args = parser.parse_args()
    if not 1 <= args.limit <= 150 or not 1 <= args.workers <= 4:
        parser.error('--limit must be 1..150 and --workers must be 1..4')
    result_dir = ROOT / 'results'
    result_dir.mkdir(exist_ok=True)
    result_file = result_dir / 'transcripts.jsonl'
    if args.self_test:
        self_test()
        return
    records = [json.loads(line) for line in result_file.read_text().splitlines()] if result_file.exists() else []
    if args.summary:
        summary(records)
        return
    cases = rows()[:args.limit]
    voices = ['Samantha', 'Daniel']
    generate(cases, voices)
    if args.generate_only:
        return
    if not os.environ.get('GROQ_API_KEY'):
        raise SystemExit('Set GROQ_API_KEY before running; credentials are never read from Keychain.')
    binary = pathlib.Path(os.environ.get('XFLOW_BIN', ROOT / '../../apps/cli/target/debug/xflow')).resolve()
    if not binary.exists():
        raise SystemExit('Build xflow first (see README.md), or set XFLOW_BIN.')
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
    server.timings = {}
    server.prompts = {}
    server.pacing = {model: {'lock': threading.Lock(), 'next': time.monotonic() + 3.1}
                     for model in ['whisper-large-v3-turbo', 'whisper-large-v3']}
    threading.Thread(target=server.serve_forever, daemon=True).start()
    done = {(r['model'], r['variant'], r['voice'], r['index']) for r in records}
    jobs = []
    for i in [*range(len(cases), len(cases) + 4), *range(len(cases))]:
        for voice in (voices if i < len(cases) else [['silence', 'hiss', 'click', 'thump'][i - len(cases)]]):
            # Alternate models to use their separate quotas; rotate prompt order.
            variants = ['old', 'list', 'sentence']
            for variant in variants[i % 3:] + variants[:i % 3]:
                for model in ['whisper-large-v3-turbo', 'whisper-large-v3']:
                    if (model, variant, voice, i) not in done:
                        jobs.append((i, voice, model, variant))
    write_lock = threading.Lock()
    def run(job):
        i, voice, model, variant = job
        silent = i >= len(cases)
        # 15 phrases share a dictionary (~57 terms); later terms expose old truncation.
        group = cases[(i // 15) * 15:(i // 15 + 1) * 15] if not silent else cases[:15]
        terms = list(dict.fromkeys(term for _, expected in group for term in expected.split('|')))
        sent = select(terms, variant)
        route = f'/{model}/{voice}/{i}/{variant}'
        prompt = ', '.join(sent)
        if variant == 'sentence':
            prompt = f'Glossary: {prompt}.'
        server.prompts[route] = prompt
        cfg = ('[stt]\nprovider = "groq"\napi_key_env = "GROQ_API_KEY"\n'
               f'endpoint = "http://127.0.0.1:{server.server_port}{route}"\n'
               f'model = {json.dumps(model)}\nlanguage = "en"\ntimeout_secs = 300\n'
               'vocabulary = []\n')
        wav = ROOT / 'audio' / (f'{voice}.wav' if silent else f'{voice}-{i:03}.wav')
        with tempfile.NamedTemporaryFile(mode='w', suffix='.toml') as config:
            config.write(cfg)
            config.flush()
            start = time.perf_counter()
            output = subprocess.run([str(binary), '--config', config.name, '--json', 'transcribe', str(wav)],
                                    capture_output=True, text=True, timeout=305)
            if output.returncode:
                # No remote body or key is logged; failed work is safe to resume.
                raise RuntimeError(f'xflow failed ({output.returncode}): {route}: {output.stderr.strip()}')
            value = json.loads(output.stdout)
            text = value.get('text', value.get('data', {}).get('text', ''))
        record = dict(model=model, variant=variant, voice=voice, index=i,
                      kind='silent' if silent else 'speech', text=text,
                      hits=0 if silent else hits(text, cases[i][1].split('|')),
                      total=0 if silent else len(cases[i][1].split('|')),
                      stt_ms=server.timings.pop(route, (time.perf_counter() - start) * 1000),
                      process_ms=(time.perf_counter() - start) * 1000,
                      hints=len(sent), prompt_bytes=len(prompt.encode()),
                      prompt_tokens=tokens(prompt))
        with write_lock:
            with result_file.open('a') as out:
                out.write(json.dumps(record) + '\n')
            records.append(record)
            if len(records) % 30 == 0:
                print(f'{len(records)} completed requests', flush=True)
        return record
    print(f'{len(jobs)} Groq requests pending; generated WAVs/results are ignored by git.', flush=True)
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
            list(pool.map(run, jobs))
    finally:
        server.shutdown()
        server.server_close()
    summary(records)


if __name__ == '__main__':
    main()
