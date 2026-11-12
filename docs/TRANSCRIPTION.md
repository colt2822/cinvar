# Transcription (faster-whisper) and caption export

    python3 -m venv ~/cinvar-fw && ~/cinvar-fw/bin/pip install faster-whisper "av<17"
    export CINVAR_FASTER_WHISPER_PYTHON=~/cinvar-fw/bin/python
    export CINVAR_FASTER_WHISPER_MODEL=/path/to/local/faster-whisper-model-dir   # e.g. a downloaded Systran/faster-whisper-small

- CINVAR **never downloads models** unless you set `CINVAR_FASTER_WHISPER_ALLOW_DOWNLOAD=1`. Model licences are the
  model's own (the tested tiny.en conversion's licence was not verified from its files - check yours).
- `cinvar.captions.transcribe {clip_id, language?, ...style}` runs on CPU (int8) and puts cues on the timeline as an
  editable caption track. `cinvar.captions.export {output, track_id?}` writes `.srt` or `.vtt` inside the project.
- Without `CINVAR_FASTER_WHISPER_PYTHON` CINVAR tries whisper.cpp/whisper if installed.
- Verified: one real faster-whisper 1.2.1 test (`CINVAR_TEST_SPEECH=short.wav cargo test --test transcribe_real -- --ignored`).
- Not integrated: WhisperX (word alignment), DeepFilterNet (denoise).
