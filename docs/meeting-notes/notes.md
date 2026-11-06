# Notes from stored meetings

Import a transcript (`xflow meeting import file.json`) or use an already
transcribed meeting. `xflow meeting list` gives its `store_id:id`.

```
xflow meeting notes STORE:ID --generate
xflow meeting notes STORE:ID
xflow meeting ask STORE:ID 'What decisions were made?'
xflow meeting term 'post grass' PostgreSQL
xflow meeting apply-terms STORE:ID
```

Generation and ask use the existing AI provider, offline setting, fallback
policy and transcript consent. `--yes` grants consent for that run; otherwise
the gateway prompts for this meeting run. Saved dictation consent does not
authorize meeting transcripts. Notes never run automatically. MCP exposure
still requires its separate meeting-source setting. No audio is read or deleted.

In the TUI press `=` (or find Meetings in the command palette), arrows to select,
Enter for notes, then Enter on Ask or Generate. Ask opens the existing chat
editor with a summary question ready to edit. Generation and each meeting ask require fresh transcript consent, even for a
previously allowed provider. Escape returns
to the meeting list. Notes and citations use plain text and work with NO_COLOR.

Notes contain summary, decision and action claims, each with a segment ID,
revision and exact supporting quote. Imported timing and source labels remain
unverified. Gaps are preserved. Inspect the supporting transcript before relying
on a generated claim: a valid quote does not prove the model interpreted it
correctly. Revisions invalidate notes; regenerate after applying dictionary terms.
Terms are literal, case-sensitive substitutions, longest first, and only apply
when explicitly requested. Original revisions are retained. Dictation is separate.

Meetings are read in at most ten-minute windows and bounded transcript pages,
with one model request at a time. Input is conservatively budgeted by UTF-8 byte,
output is capped at 2,048 tokens/request and 16 KiB/response, saved notes at
128 KiB. Oversized, malformed, interrupted, stale or uncited results preserve
previous notes. Notes are page-wise; cross-page retractions and contradictions
need human verification. The read-only context tool returns a bounded excerpt
with an omitted-claim count for large notes. CLI and TUI expose the full notes.
Global transcript edits/appends can invalidate notes for other meetings too.

Tests use varied authored conversations in English, Hindi, Spanish, Portuguese,
Japanese and Hinglish and deterministic fake providers. They check script
preservation, citations, revisions, storage deletion and bounded three-hour
paging; they do not measure real provider quality or ASR accuracy.

Founder real-provider check: on a machine with an authorized configured gateway,
import a consenting multilingual transcript, generate notes, then ask about a
retracted decision. Verify every included claim, owner and deadline against its
quoted segment/revision, inspect gaps, and confirm the provider consent prompt.
No key or provider call is needed for the automated tests.

The additional public excerpt fixture uses OPUS TED2020 transcript/translations
(English, Hindi, Spanish, Portuguese and Japanese) and a short verbatim Hinglish
read-speech example from Singh, Singh and Kadyan's
[HiACC paper, section 3.2](https://pmc.ncbi.nlm.nih.gov/articles/PMC12329218/)
(CC BY 4.0). Each fixture entry records its source URL. These are short public
speech excerpts, not a consented meeting-quality corpus. Test segment timing
is synthetic; the conversations with decisions/retractions are authored fixtures.
