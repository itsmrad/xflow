#!/usr/bin/env python3
"""Opt-in WP10 benchmark: file-only synthetic WAVs -> Groq turbo -> Rust pipeline.

Export GROQ_API_KEY privately. No microphone, speakers, daemon or credential store.
Resumes raw transcripts in ignored results/spoken-code.jsonl. Build the Rust
spoken_code example first; --summary can rescore without any network calls.
"""
import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import statistics
import subprocess
import threading
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent


def rows():
    return [line.split('\t') for line in (ROOT / 'spoken-code.tsv').read_text().splitlines()
            if line and not line.startswith('#')]


def generate(cases, audio):
    audio.mkdir(parents=True, exist_ok=True)
    for i, (spoken, _) in enumerate(cases):
        wav = audio / f'{i:03}.wav'
        if wav.exists():
            continue
        aiff = wav.with_suffix('.aiff')
        subprocess.run(['say', '-v', 'Samantha', '-r', '175', '-o', str(aiff), spoken], check=True)
        subprocess.run(['afconvert', '-f', 'WAVE', '-d', 'LEI16@16000', '-c', '1',
                        str(aiff), str(wav)], check=True)
        aiff.unlink()
    print(f'Generated {len(cases)} file-only WAVs', flush=True)


def score(cases, records):
    binary = Path(os.environ.get('XFLOW_CODE_BIN', ROOT / '../../apps/cli/target/debug/examples/spoken_code'))
    ordered = sorted(records, key=lambda r: r['index'])
    result = subprocess.run([str(binary)], input=''.join(r['text'].strip() + '\n' for r in ordered),
                            text=True, capture_output=True, check=True)
    outputs = result.stdout.splitlines()
    assert len(outputs) == len(ordered)
    # Ignore utterance-final punctuation and the sentence capital STT adds to a
    # leading prose word; never identifier case, dots, separators or boundaries.
    def normalize(s, spoken):
        s = s.strip().rstrip('.!?')
        first = s.split(' ', 1)[0]
        if first and spoken.split(' ', 1)[0].lower() == first.lower():
            s = s[0].lower() + s[1:]
        return s
    hits = strict = 0
    for record, output in zip(ordered, outputs):
        spoken, expected = cases[record['index']]
        strict += output == expected
        if normalize(output, spoken) == normalize(expected, spoken):
            hits += 1
        else:
            print(f'MISS {record["index"]}: {record["text"]!r} -> {output!r}; expected {expected!r}')
    if ordered:
        print(f'Exact after final punctuation normalization: {hits}/{len(ordered)} '
              f'({100 * hits / len(ordered):.2f}%); strict full string: {strict}/{len(ordered)}; '
              f'STT p50 {statistics.median(r["stt_ms"] for r in ordered):.1f} ms', flush=True)
    return hits, len(ordered)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--generate-only', action='store_true')
    parser.add_argument('--summary', action='store_true')
    parser.add_argument('--limit', type=int, default=240)
    parser.add_argument('--prompt', help='Whisper prompt for an STT-priming A/B; results go to a separate file')
    args = parser.parse_args()
    cases = rows()
    if not 1 <= args.limit <= len(cases):
        parser.error(f'--limit must be 1..{len(cases)}')
    result_dir = ROOT / 'results'
    result_dir.mkdir(exist_ok=True)
    result_file = result_dir / ('spoken-code-primed.jsonl' if args.prompt else 'spoken-code.jsonl')
    records = [json.loads(line) for line in result_file.read_text().splitlines()] if result_file.exists() else []
    if args.summary:
        score(cases, records)
        return
    audio = ROOT / 'audio' / 'spoken-code'
    generate(cases[:args.limit], audio)
    if args.generate_only:
        return
    if not os.environ.get('GROQ_API_KEY'):
        raise SystemExit('Export GROQ_API_KEY; no credential-store fallback is used.')
    pace_lock, write_lock = threading.Lock(), threading.Lock()
    next_attempt = time.monotonic()

    def transcribe(i):
        nonlocal next_attempt
        boundary = 'xflow-spoken-code-wav'
        fields = {'model': 'whisper-large-v3-turbo', 'language': 'en', 'temperature': '0'}
        if args.prompt:
            fields['prompt'] = args.prompt
        body = b''.join((f'--{boundary}\r\nContent-Disposition: form-data; name="{key}"\r\n\r\n'
                         f'{value}\r\n').encode() for key, value in fields.items())
        body += (f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="speech.wav"\r\n'
                 'Content-Type: audio/wav\r\n\r\n').encode()
        body += (audio / f'{i:03}.wav').read_bytes() + f'\r\n--{boundary}--\r\n'.encode()
        request = urllib.request.Request('https://api.groq.com/openai/v1/audio/transcriptions', body,
            {'Authorization': 'Bearer ' + os.environ['GROQ_API_KEY'], 'User-Agent': 'xflow-devterms/1.0',
             'Content-Type': f'multipart/form-data; boundary={boundary}'})
        for attempt in range(8):
            with pace_lock:
                time.sleep(max(0, next_attempt - time.monotonic()))
                next_attempt = time.monotonic() + 4.5  # ≤ 15 RPM on the shared key
            start = time.perf_counter()
            try:
                with urllib.request.urlopen(request, timeout=90) as response:
                    value = json.load(response)
                record = dict(index=i, text=value['text'], stt_ms=(time.perf_counter() - start) * 1000,
                              model=fields['model'], voice='Samantha')
                break
            except urllib.error.HTTPError as error:
                if error.code != 429 or attempt == 7:
                    raise RuntimeError(f'Groq HTTP {error.code}; response body withheld') from None
                delay = min(60, max(4, float(error.headers.get('retry-after', '15'))))
                print(f'Quota pause {delay:.0f}s', flush=True)
                time.sleep(delay)
        with write_lock:
            with result_file.open('a') as stream:
                stream.write(json.dumps(record) + '\n')
            records.append(record)
            if len(records) % 10 == 0:
                print(f'{len(records)} / {args.limit} transcripts saved', flush=True)
        return record

    done = {r['index'] for r in records}
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        list(pool.map(transcribe, (i for i in range(args.limit) if i not in done)))
    score(cases, records)


if __name__ == '__main__':
    main()
