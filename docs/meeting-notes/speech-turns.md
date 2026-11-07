# Meeting speech turns

New meeting captures end transcription requests after 400 ms of quiet following
speech, using the existing energy detector's soft edge floor. Shorter pauses
stay in the same request. Continuous speech is capped at 30 seconds of new
audio, with the existing two seconds of context after a forced cut. A pause
ends that context: the following turn never reads speech from the previous
turn. A known exception is a pause spanning a forced maximum-length cut:
for example, 29.8 s speech followed by 800 ms quiet crosses that cut and can
retain prior-turn context in the next request. This needs a follow-up; the
synthetic language proofs below do not exercise that boundary.
This changes meetings only. Dictation preparation, providers, forms and
language preferences are unchanged; nothing new runs when no meeting is active.

Schema 4 adds a per-source planning cursor. Jobs retain explicit sample ranges,
sequence numbers and independent three-dispatch budgets. A piece, its jobs and
its cursor commit in one transaction after the PCM file is durable. Accepted
text, provenance, attempt state and audio-release intent still commit together.
Retention uses the cursor's next possible read offset and every outstanding
job's actual range. Recovery keeps old captures without cursors on the legacy
30-second planner; it never replaces an accepted or attempted legacy job.
Under-100-ms tails remain retained. Only exactly zero PCM can be accepted
without a provider request; quiet nonzero samples are still sent.

The two-source, three-hour limit permits at most 54,002 job rows, including short
turns and tails. The no-audio three-hour planning test creates 720 bounded jobs
and two cursors without writing audio. Typical counts depend on pauses; 720 is
not the worst-case speech-turn count. No additional transcription dispatch is
hidden inside a job.

## Synthetic provider measurements

All measurements below used file-only `say` PCM through `xflow meeting process
--wait`, with isolated HOME and XDG directories. No microphone, tap, speakers,
desktop lock, installed daemon, LaunchAgent, real configuration or history was
used. The provider was Groq `whisper-large-v3-turbo`, auto language, no prompt.
The original implementation's binary was used for the comparison. The final
comparison passed upload bytes unchanged through a loopback proxy that paced
the shared free-tier API to one request every 3.2 seconds.

| Proof | Result |
| --- | --- |
| Original 83.272 s composite | Hindi line present; Spanish and French retained in their languages. 9 API requests, 10 durable ranges including an unsent digital-silence tail. Previously 3 requests omitted Hindi and translated Spanish/French. |
| Standalone English | Words preserved; punctuation and `three` → `3` differ. |
| Standalone Spanish | Spanish preserved; `cinco` → `5`. |
| Standalone French | Exact text, including accents. |
| Standalone Hindi | `कल सुबह 10 बजे हम deployment की समीक्षा करेंगे। रविवार test report भेजेगा।` Same known Ravi → रविवार error; loanwords in Latin script. Earlier mixed clips also romanized Hindi, which segmentation cannot guarantee to prevent. |
| Within-turn switch | `आज हम deployment की समीक्षा करेंगे and then Priya will check the PostgreSQL database before launch.` Both languages retained; spoken `lunch` became `launch`. One request; no general code-switch guarantee. |
| 197.509 s English meeting | 435 reference words across 23 varied utterances. WER 34/435 = 7.82% before, 24/435 = 5.52% after. Two tongue twisters, names, numbers, Kubernetes, PostgreSQL, OAuth, kubectl, useEffect and git rebase included. |

WER lowercases and splits Unicode alphanumeric words, ignoring punctuation;
numbers are not normalized (`three` and `3` differ). It includes actual stored
text, so overlapping duplicate words count as errors. This is one synthetic
comparison, not evidence about human speech or background noise. Both runs
misrecognized developer terms: `kubectl` → `kubectal`, PostgreSQL → `post-gur
SQL`, and `useEffect dependency` → `useEffectDependency`. The shell tongue
twister combines `sea shells` and `sea shore`; the Peter Piper line is preserved.

## Requests, billing and speed

[Groq documents a 10-second minimum per request and $0.04 per audio hour for
turbo](https://console.groq.com/docs/speech-to-text). These are rate-equivalent
estimates from exact uploaded durations with that minimum, not an invoice.
The normalized figures repeat each fixture's pause density for an hour of one
source. Real meetings and two active sources vary; retries add further cost.

| Fixture | Requests before → after | Billed seconds before → after | Requests/meeting hour before → after | Billed seconds/meeting hour before → after | Estimated $/meeting hour before → after |
| --- | --- | --- | --- | --- | --- |
| English, 197.509 s | 7 → 23 | 209.509 → 235.820 | 127.6 → 419.2 | 3818.7 → 4298.3 | $0.04243 → $0.04776 (+12.6%) |
| Mixed, 83.272 s | 3 → 9 | 87.272 → 106.120 | 129.7 → 389.1 | 3772.9 → 4587.8 | $0.04192 → $0.05098 (+21.6%) |

The paced English runs took 20.8 s before and 74.3 s after; the final mixed run
took 28.1 s. These include intentional quota waits, so they are not isolated
provider-latency benchmarks. More short requests cost more and slow file catchup.
Short gaps are merged within a turn. Across real pauses the energy detector
cannot establish that two turns use the same language; this change conservatively
keeps them separate. Language-aware short-turn coalescing is not implemented.

There were 101 reserved API dispatches in this investigation: 62 in the initial
batch (including 30 HTTP 429 failures), then 39 successful, paced requests.
The rate-limited partial English run is excluded from accuracy evidence. Its
failed ranges remained durable and available for retry. No further API runs
were used to tune the measured result.

The on-device path uses the same range and budget ledger and reuses its helper;
a file-only integration test verifies PCM framing with the existing fake helper.
Actual Core ML model quality and latency were not measured. A separate loopback
comparison of the old and new binaries' file-dictation requests matched all
154,316 multipart bytes after replacing only the random MIME delimiter
(SHA-256 `9194dc26bb7b6bdb9cb0f5f43e5836a09dfc3a1adc17369fb74e7ee228c4ba3a`). Pause detection can
split a sentence or fail to find a turn boundary in noise. A language switch
without a sufficient pause remains one request. Legacy unfinished captures
finish with their original window geometry and therefore retain the old
language limitation until a new meeting is started.
