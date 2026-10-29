#!/usr/bin/env python3
"""Offline fixtures: uv run --with tiktoken==0.14.0 generate_tokens.py.

The estimator stops BPE at rank 8192, so its token count is an upper bound.
Only ASCII letter pieces (optionally space-prefixed) use merges; other bytes
are counted individually. Both GPT-2 and Whisper multilingual are bounded.
Sources: https://github.com/openai/whisper/blob/main/whisper/tokenizer.py
         https://github.com/openai/tiktoken (MIT)
"""
import base64
import pathlib
import urllib.request
import tiktoken

ROOT = pathlib.Path(__file__).resolve().parent
BPE = ROOT / '../../apps/cli/crates/xflow-providers/src/bpe'
PATTERN = r"'s|'t|'re|'ve|'m|'ll|'d| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+"


def encodings():
    data = urllib.request.urlopen(
        'https://raw.githubusercontent.com/openai/whisper/'
        'v20250625/whisper/assets/multilingual.tiktoken').read()
    ranks = {base64.b64decode(token): int(rank)
             for token, rank in (line.split() for line in data.splitlines())}
    return [tiktoken.get_encoding('gpt2'), tiktoken.Encoding(
        name='whisper-multilingual', pat_str=PATTERN,
        mergeable_ranks=ranks, special_tokens={})]


def main():
    codecs = encodings()
    for codec, name in zip(codecs, ['gpt2', 'whisper']):
        rows = []
        for token, rank in codec._mergeable_ranks.items():
            if rank >= 8192 or len(token) < 2:
                continue
            word = token.removeprefix(b' ')
            if word and all(65 <= c <= 90 or 97 <= c <= 122 for c in word):
                rows.append((token.decode('ascii'), rank))
        tokens = [token.replace(' ', '_') for token, rank in sorted(rows, key=lambda row: row[1])]
        (BPE / f'{name}-merges.txt').write_text('\n'.join(
            ' '.join(tokens[i:i + 16]) for i in range(0, len(tokens), 16)) + '\n')
    terms = list(dict.fromkeys(term for line in (ROOT / 'phrases.tsv').read_text().splitlines()
                              if line and not line.startswith('#')
                              for term in line.split('\t')[1].split('|')))
    terms += ['xqzj', 'pnpm', 'zxqv', 'éclair', '数据库', 'हिन्दी', '🦀',
              'NaïveBayes', 'foo::bar', '<|endoftext|>', 'a' * 896,
              "foo'stuff", "we'response", "we'version", "I'llambda", "I'delegate",
              'React, PostgreSQL, useEffect.', 'Glossary: React, PostgreSQL, useEffect.']
    assert len(terms) >= 300
    (ROOT / 'tokens.tsv').write_text('# term\tgpt2\tspace_gpt2\twhisper\tspace_whisper\n' + ''.join(
        term + '\t' + '\t'.join(str(len(codec.encode(prefix + term, disallowed_special=())))
                                  for codec in codecs for prefix in ['', ' ']) + '\n'
        for term in terms))
    print(f'Generated {len(terms)} offline token-count cases')


if __name__ == '__main__':
    main()
