# Preferred languages (#40)

`corpus.tsv` promotes the existing
2026-10-29 language spike's 30 synthetic, non-personal Hindi–English developer
utterances. Hindi words are in Devanagari and English terms in Latin. The second
reference uses romanized Hindi. Clips are macOS `say` Lekha (`hi_IN`) output,
16 kHz mono 16-bit WAV, generated locally and not committed (`audio/` is ignored).
They are **TTS recordings, not consenting human speech**.

To generate a clip on macOS (substitute a corpus row's Devanagari reference):

```sh
say -v Lekha -o /tmp/xflow-language.aiff 'कल Next.js app को deploy करना है।'
afconvert -f WAVE -d LEI16@16000 -c 1 /tmp/xflow-language.aiff audio/Lekha-m01.wav
```

Configure the next dictation with `xflow config set stt.languages '["hi", "en"]'`,
or use Settings → Preferred languages. This replaces the legacy fixed language.
An empty list restores Automatic; one entry pins a language. Up to three distinct
two-letter language codes, optionally with a BCP-47 region (`hi-IN`, `es-419`),
from the [IANA language registry](https://www.iana.org/assignments/language-subtag-registry/language-subtag-registry)
are accepted. Region subtags currently use the base language; script and extension
tags are refused rather than implying script control. TUI edits use language
names, validate before saving and reload the daemon for the next dictation.
`xflow transcribe file.wav --language fr` overrides the saved set for that file.

## Protocol behavior

- Whisper multipart / whisper.cpp: no forced `language` for multiple preferences.
  Hindi + English starts the prompt with the spike's measured lead,
  `हाँ, मैं अभी check करके बताता हूँ।`, followed by vocabulary and dictionary
  hints. Other sets send no lead: they are unmeasured. The existing conservative
  prompt budget includes this lead first; explicit oversized vocabulary is
  refused, optional dictionary/domain hints fit into the remainder. This leaves
  the same prompt path available for #117. No first-language forcing, translation
  or local transliteration is added.
- OpenAI `openai-keywords`: repeated `languages[]` fields.
- Deepgram Nova-3: `language=multi` plus repeated `language_hint`. Nova-2 accepts
  only English + Spanish for `multi`. Unknown models/unsupported sets are refused.
- AssemblyAI Universal-3.5 Pro (async and sync): `language_codes`, mutually
  exclusive with `language_code` and `language_detection`. This first version
  conservatively accepts sets drawn from en/es/fr/de/pt/hi/zh.
- Gemini 3.5 Transcribe: `languageCodes` with the adapter's supported locales.
- ElevenLabs Scribe v2 can auto code-switch; this adapter has no preferred-set
  field. ElevenLabs, Mistral, OpenRouter, Parakeet and unrecognized models refuse
  multiple preferences. Single-language and Automatic behavior remain available.
- A configured backup must support the same set; saves reject incompatible backups.

Request behavior follows the primary references for
[Groq](https://console.groq.com/docs/speech-to-text),
[Deepgram code-switching](https://developers.deepgram.com/docs/multilingual-code-switching),
[Deepgram language coverage](https://developers.deepgram.com/docs/models-languages-overview),
[AssemblyAI language sets](https://www.assemblyai.com/docs/pre-recorded-audio/code-switching),
[Gemini transcription](https://ai.google.dev/gemini-api/docs/generate-content/transcribe),
and [ElevenLabs](https://elevenlabs.io/docs/api-reference/speech-to-text/convert).
Only Groq is smoke-tested against a live service in this PR; other request paths
have local HTTP contract tests. Capability reporting describes request support,
not measured accuracy.

## Quality gate still open

Native-script Hindi is the scope here. Whisper may write English terms in
Devanagari; preferred languages are hints, not an enforced allowlist or script
guarantee. Roman Hinglish, a script picker, quick cycling and the pill badge remain
follow-ups. The issue's ≥90% gate with **30 real, consenting human recordings and
light cleanup** is not met by this TTS corpus or the small raw-STT smoke check.
Do not close #40 or advertise a Hinglish accuracy guarantee from these results.

## Live smoke check (2026-11-02)

`smoke.json` records ten real Groq turbo requests: five newly generated Lekha
clips (m01–m05), each with the set off and on. Both conditions used the same six
vocabulary hints and raw STT (no cleanup). Both returned Devanagari plus Latin on
all five clips. The set improved some English casing/spelling and retained Hindi
sentence punctuation on four clips, but errors such as m01's “Next.js app” and
m05's “logs” remain. This sample establishes request acceptance and script
preservation, not an accuracy improvement or the issue's human-speech gate.
