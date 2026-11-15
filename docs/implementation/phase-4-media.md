# Phase 4 media

The media worker still owns Anki's ordered AV queue, autoplay, replay and
interrupt rules. TTS, video and own-voice replay use that queue. No media test
opens a real profile, records a real microphone or speaks through the host's
sound device by default.

## Controls and configuration

In the reviewer, `v` starts capture and `v` again stops it. The status line shows
`● recording`; WAV finalization runs in a background job. `Shift+V` replays the
last completed recording and reports an error if capture is still finishing.
Recordings are temporary, survive repeated replay and disappear when their last
owner is dropped. They are never added to `collection.media` or synced.

`r` / F5 replays card AV tags, `5` pauses playback, and `6` / `7` seek by five
seconds. Speech cannot be seeked. Opening another screen stops the card queue.

| CLI option | Values / behavior |
|---|---|
| `--tts` | `auto` (default), `speech-dispatcher`, `espeak`, `say`, `piper`, `off` |
| `--piper-model FILE` | Local ONNX model for `--tts=piper`; tanki downloads no model |
| `--video-output` | `window` (default), `kitty`, `sixel`, `blocks` |

Linux auto TTS prefers speech-dispatcher when `spd-say` is installed, then
`espeak-ng`. macOS prefers `say`. A running speech-dispatcher service is required;
its local Unix socket comes from `SPEECHD_ADDRESS=unix_socket:/path/to/socket` or
`$XDG_RUNTIME_DIR/speech-dispatcher/speechd.sock` (with `/run/user/UID` fallback).
Auto falls back to espeak-ng when the speech-dispatcher connection fails and
espeak-ng is installed. Remote speech-dispatcher TCP addresses are unsupported.

The speech-dispatcher client uses SSIP with per-connection `CANCEL SELF`,
`PAUSE SELF` and `RESUME SELF`. Card text cannot become SSIP commands: message
terminators are escaped, and language/voice command parameters are validated.
The espeak-ng, say and Piper CLIs receive separate arguments and text on stdin;
no shell evaluates card content. The low-level explicit executable override for
speech-dispatcher uses a literal `spd-say --wait -- TEXT` operand, never its
command-capable pipe mode. macOS maps an unspecified voice to an installed voice
for the requested language. Piper synthesizes a temporary WAV asynchronously,
then passes it to the ordinary audio backend; Piper therefore also needs a
working audio output device.

Video requires installed mpv. The default opens its own window. In-terminal
modes select `--vo=kitty`, `--vo=sixel` or `--vo=tct`; the installed mpv must
support the selected output driver, and the terminal must support that protocol.
These modes are explicit choices and bypass automatic graphics detection.
TUI input remains active for replay, pause, seek and navigation while mpv owns
stdout. The TUI suspends drawing and clears/repaints after playback. Audio-only
files retain the ordinary reviewer display; unknown containers reserve stdout
because they may contain video. The built-in rodio backend does not display
video.

Recording uses installed `pw-record`, otherwise ffmpeg. The default ffmpeg input
is PulseAudio on Linux, AVFoundation device `:0` on macOS and DirectShow
`audio=default` on Windows. Device-specific configurations may need an external
recorder; there is no device picker in this phase. `pw-record` receives SIGINT
and ffmpeg receives `q` to finalize the WAV header. Finalization has a two-second
limit; invalid or empty WAVs are discarded with an error. Dropping an unfinished
recorder kills and reaps its child.

## APIs and terminal ownership

`AudioConfig` keeps its existing fields. `Audio::start_with_options` adds
`PlaybackOptions { tts: TtsConfig, video: VideoOutput }`; `Audio::start` uses the
defaults. The public `TtsBackend` and `VideoOutput` enums match the CLI choices.

`Audio::toggle_recording() -> Result<bool, String>` returns whether capture is
active after the action. `is_recording()` supplies the status indicator, and
`replay_recording()` queues the last WAV. Low-level callers can use
`VoiceRecorder::start(RecordingConfig)`, `finish() -> Recording`,
`Recording::path()` and `Audio::play_recording`. Cloned `Recording` handles retain
the temporary WAV through queued playback.

The audio worker takes `terminal_output_lock()` while changing ownership and
starting video. The terminal loop takes the same mutex across
`owns_terminal()` and drawing, preventing the first mpv frame from racing a TUI
frame. On stop, in-terminal mpv playback waits for end/idle notification (bounded
to 250 ms). `stop_and_wait()` adds a worker acknowledgement before terminal
cleanup; ordinary UI stops stay asynchronous. The shutdown acknowledgement has
a two-second timeout.

## Validation and remaining manual checks

The media stream passed:

```sh
cargo test --offline -p tanki-media
cargo clippy --offline -p tanki-media --all-targets -- -D warnings
cargo fmt -p tanki-media -- --check
```

There are 38 unit tests: queue ordering/interruption, file and symlink safety,
synthetic WAV decoding, fake recording/TTS executables, local SSIP escaping and
SELF controls, Piper generation, macOS voice mapping, asynchronous recording
finalization, terminal ownership locking, output-driver selection and shutdown
acknowledgement. The SSIP fixture binds a temporary Unix socket, requiring local
socket permission in restricted test environments. The hardware smoke test is
opt-in with `TANKI_TEST_AUDIO_DEVICE=1`; otherwise it returns without probing
ALSA/JACK/PulseAudio. The mpv roundtrip checks start/stop only when mpv is present.

Actual spoken output, microphone capture and terminal/video appearance still
need a user check. For a generated profile only:

```sh
media_base=$(mktemp -d)
cargo run --offline -p xtask --features fixtures -- study-fixtures --base "$media_base"
cargo run --offline -p tanki-cli -- --base "$media_base" --tts=auto review 'Study::Media'
```

Check card playback, then `v`, a short spoken phrase, `v`, and `Shift+V`. For a
local video card, try `--video-output=window`, then a supported explicit terminal
mode; verify pause/seek/navigation and that exiting restores the terminal. The
stock study fixture supplies audio, not a video clip or a TTS note, so those
checks require adding synthetic local test content. Audio always plays on the
host running tanki; SSH does not transport it to the client.
