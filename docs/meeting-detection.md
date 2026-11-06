# Meeting prompts on macOS

On macOS 14.2 and newer Xflow can ask **“Turn on Xflow for this meeting?”**
for Zoom, Google Meet, Microsoft Teams and Amazon Chime. Detection opens no
microphone, captures no audio, and sends no window titles or URLs anywhere.
Opening an app or its pre-join screen is insufficient: its microphone must be
active for two seconds and readable call controls must identify a joined call.
Accessibility permission is needed to read those controls. If evidence is
unavailable, use manual recording.

Yes opens the recorder's consent dialog. That dialog explains the microphone,
selected app and helper audio (including other tabs in that browser), local
storage and the configured transcription provider. Recording starts only after
Record. Not now, Escape and an expired prompt suppress it for that call.

Disable prompts with `xflow config set meetings.detect false`, or choose
**Turn off prompts** on a card. Config reload closes the observer; turning the
setting back on starts it again. Manual recording remains available:

```sh
xflow meeting record --app com.google.Chrome
xflow meeting stop
```

The separately shipped live recorder supplies these two commands. Before it is
installed, Yes reports that recording is unavailable and captures nothing.

A native call ends automatically after both its input and call window have
been gone for eight seconds, or its app has quit for eight seconds. A browser
call ends after an explicit post-call Rejoin screen plus eight seconds without
input, or after its browser quits. A hidden tab and a muted call are ambiguous:
Xflow keeps recording and offers **Still recording. Stop?** instead of silently
cutting off the meeting. The recorder's persistent Stop control always remains.
Two visible meetings in one browser are ambiguous and do not prompt.

Idle detection sleeps on Core Audio listeners. A one-second read runs only
while the input device is active, a meeting is pending/live, or a listener has
failed. Detection lives in the `xflow meeting-watch` client; `xflowd` only owns
its control pipe. No audio taps, calendar access, Automation permission or new
Screen Recording prompt are used by detection.

## Recorder integration

Start: spawn the sibling executable with
`xflow --config PATH meeting record --app BUNDLE --control-stdin` and keep its
stdin open. Stop: close that stdin (or write `q`). The recorder finalizes
saved audio gracefully, including cancellation during its consent dialog.
Never terminate it forcibly to stop a meeting. Exit 0 means recorded or
consent declined; nonzero means failure, explained on stderr. Detector shutdown
and daemon shutdown also close the control pipe. No recording restarts itself.

Known limits: English call-control labels; Accessibility denial or unreadable
controls require manual start. Safari's shared WebKit microphone process is
not attributed, so use manual recording there. Browser mute with hidden tabs
may require manual Stop. Native Zoom/Teams/Chime call controls still need
validation in real joined calls. Amazon Chime service support ended in 2026;
its adapter is retained for compatible existing clients.
