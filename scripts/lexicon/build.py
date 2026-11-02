#!/usr/bin/env python3
"""Builds xflow's hint-only term lexicon (`xflow-core/src/hints/lexicon.txt`).

Two steps, both re-runnable:

    # 1. Import collector files (canonical, domain, rank, english, mishearings,
    #    context_cues, source_url). Each row is kept only if its name is confirmed
    #    on an official vendor index page or on its own cited source page; a
    #    Wikipedia source must exist. Pages are cached outside the repo.
    python3 scripts/lexicon/build.py import /path/to/vocab-data
    # 2. Compile data/*.tsv plus the hand-kept heads (data/hand/*.txt). Offline.
    python3 scripts/lexicon/build.py
    python3 scripts/lexicon/build.py --check   # fails if the artifact is stale

Only names are kept (and the URL that confirmed each); no descriptions,
mishearings or page text. See data/NOTICE.
"""
import argparse
import csv
import hashlib
import html
import json
import os
import pathlib
import re
import sys
import time
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

ROOT = pathlib.Path(__file__).resolve().parent
DATA = ROOT / 'data'
CORE = ROOT / '../../apps/cli/crates/xflow-core/src'
OUT = CORE / 'hints/lexicon.txt'
CACHE = pathlib.Path(os.environ.get('XFLOW_LEXICON_CACHE', pathlib.Path.home() / '.cache/xflow-lexicon'))
AGENT = 'xflow-lexicon/1 (https://github.com/itsmrad/xflow)'

# Hand-kept heads, in this order, each the start of its own section.
HAND = ['dev', 'corp', 'sheet', 'finance']
# Config group ([vocab] switch) of every section; anything else is `dev`.
GROUP = {'corp': 'corp', 'sheet': 'sheet', 'finance': 'finance'}
# Official indexes a vendor's names are confirmed against (names only are used).
INDEXES = {
    'aws': [
        'https://aws.amazon.com/api/dirs/items/search?item.directoryId=aws-products&item.locale=en_US&size=500',
        'https://docs.aws.amazon.com/en_us/main-landing-page.xml',
        'https://docs.aws.amazon.com/glossary/latest/reference/glos-chap.html',
        'https://aws.amazon.com/service-terms/',
        'https://docs.aws.amazon.com/global-infrastructure/latest/regions/aws-regions.html',
        'https://docs.aws.amazon.com/general/latest/gr/aws-service-information.html',
        'https://docs.aws.amazon.com/service-authorization/latest/reference/reference_policies_actions-resources-contextkeys.html',
    ],
    'gcp': [
        'https://cloud.google.com/products',
        'https://cloud.google.com/terms/services',
        'https://cloud.google.com/docs',
        'https://cloud.google.com/products/docs',
        'https://cloud.google.com/sdk/gcloud/reference',
        'https://cloud.google.com/iam/docs/roles-permissions',
    ],
    'azure': [
        'https://azure.microsoft.com/en-us/products/',
        'https://azure.microsoft.com/en-us/pricing/details/',
        'https://learn.microsoft.com/en-us/azure/',
        'https://learn.microsoft.com/en-us/azure/azure-glossary-cloud-terminology',
        'https://learn.microsoft.com/en-us/azure/cloud-adoption-framework/ready/azure-best-practices/resource-abbreviations',
        'https://learn.microsoft.com/en-us/azure/role-based-access-control/built-in-roles',
        'https://learn.microsoft.com/en-us/azure/azure-resource-manager/management/azure-services-resource-providers',
    ],
}
# Cross-cloud comparison pages confirm names for every vendor.
SHARED = [
    'https://learn.microsoft.com/en-us/azure/architecture/aws-professional/services',
    'https://cloud.google.com/docs/get-started/aws-azure-gcp-service-comparison',
]
MAX_BYTES, MAX_WORDS = 24, 3


def decode(path):
    """A front-coded word list (see words.rs)."""
    out, prev = set(), ''
    for line in path.read_text().splitlines():
        word = prev[:ord(line[0]) - 65] + line[1:]
        out.add(word)
        prev = word
    return out


EVERYDAY = decode(CORE / 'words.txt')
EXTRA = decode(CORE / 'vocab/english-extra.txt')


INSTRUCTION = {'spell', 'write', 'use', 'not', 'never', 'always', 'instead', 'ignore', 'transcribe', 'glossary'}


def shaped(term):
    """Spelling that sets a word apart from English: camelCase, digits,
    inner punctuation or an acronym of three or more capitals."""
    return (re.search(r'[a-z][A-Z]', term) or re.search(r'\d', term) or re.search(r'\w[.\-_+#@/]', term)
            or re.fullmatch(r'[A-Z]{3,}s?', term))


def rejected(term, english):
    """Why a term can't be a hint, or None. Hints are never rewrites, but a
    one-word everyday hint still biases casing, and plain phrases waste the
    small Whisper budget on words it already hears."""
    if not term or term != term.strip() or len(term.encode()) > MAX_BYTES or len(term.split(' ')) > MAX_WORDS:
        return 'shape'
    if not all(c == ' ' or 33 <= ord(c) < 127 for c in term) or any(c in term for c in ',<>"{}[]\\') or '  ' in term:
        return 'chars'
    if term.isdigit() or len(term) < 2 or term.endswith(('.', ':', '!', '?')):
        return 'shape'
    words = term.split(' ')
    # Hints read as transcript, never as instructions (the Rust lexicon test).
    if any(w.lower() in INSTRUCTION for w in words):
        return 'instruction'
    if len(words) == 1:
        if not shaped(term) and (term.lower() in EVERYDAY or english == '1'):
            return 'english'
    elif all(w.islower() and (w in EVERYDAY or w in EXTRA) for w in words):
        return 'english'
    return None


# --- import -----------------------------------------------------------------

def fetch(url):
    CACHE.mkdir(parents=True, exist_ok=True)
    path = CACHE / hashlib.md5(url.encode()).hexdigest()
    if not path.exists():
        try:
            req = urllib.request.Request(url, headers={'User-Agent': f'Mozilla/5.0 {AGENT}'})
            data = urllib.request.urlopen(req, timeout=25).read(5_000_000)
        except Exception:  # an unreachable page confirms nothing
            data = b''
        path.write_bytes(data)
    return path.read_text(errors='replace')


def plain(page):
    page = re.sub(r'<[^>]+>', ' ', page)
    page = html.unescape(html.unescape(page)).replace('\\u0026', '&').replace('\xa0', ' ')
    return re.sub(r'\s+', ' ', normal(page))


def normal(text):
    """Typographic dashes and quotes as names are typed (Bellman–Ford)."""
    return re.sub('[\u2010-\u2015\u2212]', '-', text).replace('\u2019', "'").replace('\u2018', "'")


def found(term, text, loose=False):
    """Whole-word match; vendor indexes need the exact casing, a cited page
    or article may write the name in any case, hyphenated or spaced."""
    flags = re.I if loose or term.islower() else 0
    forms = {term, term.replace('-', ' '), term.replace(' ', '-')} if loose else {term}
    return any(re.search(r'(?<![\w-])' + re.escape(f) + r'(?![\w-])', text, flags) for f in forms)


def aws_products():
    """The AWS product directory is JSON; keep its English product names."""
    text = fetch(INDEXES['aws'][0])
    try:
        return ' '.join(i['item']['additionalFields']['productName'] for i in json.loads(text)['items'])
    except (ValueError, KeyError):
        return ''


def wikipedia(titles):
    """title -> (resolved title, intro) for pages that exist; cached."""
    path = CACHE / 'wikipedia.json'
    cache = json.loads(path.read_text()) if path.exists() else {}
    todo = sorted(t for t in titles if t not in cache)
    for i in range(0, len(todo), 20):
        batch = todo[i:i + 20]
        query = urllib.parse.urlencode({
            'action': 'query', 'format': 'json', 'prop': 'extracts', 'exintro': 1, 'explaintext': 1,
            'exlimit': 20, 'redirects': 1, 'titles': '|'.join(batch)})
        req = urllib.request.Request('https://en.wikipedia.org/w/api.php?' + query, headers={'User-Agent': AGENT})
        answer = json.load(urllib.request.urlopen(req, timeout=30))['query']
        norm = {n['from']: n['to'] for n in answer.get('normalized', [])}
        redirect = {n['from']: n['to'] for n in answer.get('redirects', [])}
        pages = {p['title']: p for p in answer['pages'].values()}
        for title in batch:
            page = pages.get(redirect.get(norm.get(title, title), norm.get(title, title)))
            cache[title] = None if not page or 'missing' in page else [page['title'], page.get('extract', '')[:4000]]
        CACHE.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(cache))
        time.sleep(0.3)
    return cache


def wikipedia_text(title):
    """A whole article as plain text, for names past the intro; cached."""
    path = CACHE / 'wikipedia-full.json'
    cache = json.loads(path.read_text()) if path.exists() else {}
    if title not in cache:
        query = urllib.parse.urlencode({'action': 'query', 'format': 'json', 'prop': 'extracts',
                                        'explaintext': 1, 'redirects': 1, 'titles': title})
        req = urllib.request.Request('https://en.wikipedia.org/w/api.php?' + query, headers={'User-Agent': AGENT})
        pages = json.load(urllib.request.urlopen(req, timeout=30))['query']['pages'].values()
        cache[title] = ' '.join(p.get('extract', '') for p in pages)
        path.write_text(json.dumps(cache))
        time.sleep(0.2)
    return cache[title]


def articles(terms):
    """Which exact names have their own Wikipedia article (a redirect counts,
    a disambiguation page doesn't): real, named things. Cached."""
    path = CACHE / 'wikipedia-titles.json'
    cache = json.loads(path.read_text()) if path.exists() else {}
    todo = sorted({t for t in terms if t not in cache and '|' not in t})
    for i in range(0, len(todo), 50):
        batch = todo[i:i + 50]
        query = urllib.parse.urlencode({'action': 'query', 'format': 'json', 'prop': 'pageprops',
                                        'redirects': 1, 'titles': '|'.join(batch)})
        req = urllib.request.Request('https://en.wikipedia.org/w/api.php?' + query, headers={'User-Agent': AGENT})
        answer = json.load(urllib.request.urlopen(req, timeout=30))['query']
        norm = {n['from']: n['to'] for n in answer.get('normalized', [])}
        redirect = {n['from']: n['to'] for n in answer.get('redirects', [])}
        pages = {p['title']: p for p in answer.get('pages', {}).values()}
        for term in batch:
            page = pages.get(redirect.get(norm.get(term, term), norm.get(term, term)))
            cache[term] = bool(page and 'missing' not in page and 'invalid' not in page
                               and 'disambiguation' not in page.get('pageprops', {}))
        path.write_text(json.dumps(cache))
        time.sleep(0.2)
    return {t for t in terms if cache.get(t)}


def wiki_title(url):
    return urllib.parse.unquote(url.split('/wiki/', 1)[1].split('#')[0]).replace('_', ' ')


def import_dir(src):
    files = sorted(pathlib.Path(src).glob('*.tsv'))
    rows = {f: list(csv.DictReader(f.open(encoding='utf-8'), delimiter='\t')) for f in files}
    urls = {r['source_url'] for rs in rows.values() for r in rs if r['source_url'].startswith('http')}
    wiki = wikipedia({wiki_title(u) for u in urls if 'wikipedia.org/wiki/' in u})
    with ThreadPoolExecutor(8) as pool:
        pages = dict(zip(sorted(urls), pool.map(lambda u: '' if 'wikipedia.org/wiki/' in u else fetch(u), sorted(urls))))
    shared = ' '.join(plain(fetch(u)) for u in SHARED)
    index = {v: ' '.join(plain(fetch(u)) for u in us[v == 'aws':]) + ' ' + shared for v, us in INDEXES.items()}
    index['aws'] += ' ' + aws_products()
    DATA.mkdir(exist_ok=True)
    for f, rs in rows.items():
        kept, dropped, checked = [], [], []
        for r in rs:
            term, url = r['canonical'].strip(), r['source_url'].strip()
            evidence = None
            if r['domain'] in index and found(term, index[r['domain']]):
                evidence = 'index'
            elif 'wikipedia.org/wiki/' in url:
                page = wiki.get(wiki_title(url))
                if page and (found(term, normal(page[0] + ' ' + page[1]), True)
                             or found(term, normal(wikipedia_text(page[0])), True)):
                    evidence = 'wikipedia'
            elif pages.get(url) and found(term, plain(pages[url]), True):
                evidence = 'page'
            checked.append((r, term, url, evidence))
        titled = articles([term for _, term, _, evidence in checked if not evidence])
        for r, term, url, evidence in checked:
            evidence = evidence or ('wikipedia-title' if term in titled else None)
            if evidence:
                kept.append([term, r['domain'], r['rank'], r['english'], url, evidence])
            else:
                dropped.append(term)
        out = DATA / f.name
        with out.open('w', encoding='utf-8', newline='') as o:
            o.write('# canonical\tdomain\trank\tenglish\tsource_url\tconfirmed_by (scripts/lexicon/build.py import)\n')
            for row in kept:
                o.write('\t'.join(row) + '\n')
        print(f'{f.name}: kept {len(kept)}/{len(rs)}; unconfirmed, dropped: {", ".join(dropped[:25])}'
              f'{" ..." if len(dropped) > 25 else ""}', flush=True)


# --- build ------------------------------------------------------------------

def build():
    """`@section group` headers, then one term per line, best first. A term in
    several files stays where it ranks best relative to its file's length."""
    rows, stats = [], {}
    for name in HAND:
        for term in (DATA / 'hand' / f'{name}.txt').read_text().splitlines():
            rows.append((-1.0, name, term, None))
    for f in sorted(DATA.glob('*.tsv')):
        lines = [line.split('\t') for line in f.read_text(encoding='utf-8').splitlines() if not line.startswith('#')]
        ranked = [r for _, r in sorted(enumerate(lines), key=lambda x: (int(x[1][2]), x[0]))]
        rows.extend((i / len(ranked), r[1], r[0], r[3]) for i, r in enumerate(ranked))
    best = {}
    for row in rows:
        why = None if row[3] is None else rejected(row[2], row[3])
        stats[why or 'candidate'] = stats.get(why or 'candidate', 0) + 1
        if why is None and (row[2].lower() not in best or row[0] < best[row[2].lower()][0]):
            best[row[2].lower()] = row
    sections = {}
    for row in rows:
        if best.get(row[2].lower()) is row:
            sections.setdefault(row[1], []).append(row[2])
    stats['duplicate'] = stats.pop('candidate', 0) - len(best)
    lines = ['# Generated by scripts/lexicon/build.py from scripts/lexicon/data; do not edit.']
    for section, terms in sections.items():
        lines.append(f'@{section} {GROUP.get(section, "dev")}')
        lines.extend(terms)
    return '\n'.join(lines) + '\n', sections, stats


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('command', nargs='?', choices=['import', 'build'], default='build')
    parser.add_argument('src', nargs='?')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    if args.command == 'import':
        if not args.src:
            parser.error('import needs the collector directory')
        import_dir(args.src)
    text, sections, stats = build()
    if args.check:
        if OUT.read_text() != text:
            sys.exit(f'{OUT} is stale: run scripts/lexicon/build.py')
        return
    OUT.write_text(text)
    print(f'{OUT.name}: {sum(map(len, sections.values()))} terms, {len(text.encode())} bytes; '
          + ', '.join(f'{s} {len(t)}' for s, t in sections.items()) + f'; rows {stats}')


if __name__ == '__main__':
    main()
