# CINVAR WhisperX helper: argv = audio_wav out_json [language] [initial_prompt].
# initial_prompt is the custom vocabulary (captions::vocab::prompt_for); it biases the
# decoder's spelling of names and jargon and is absent unless the caller asked for it.
# Writes {"language", "words": [{word,start,end}], "segments": [{text,start,end}]} to out_json.
# Model gate: CINVAR_WHISPERX_MODEL (default "base"), CINVAR_WHISPERX_CACHE (download root).
# Network is off unless CINVAR_WHISPERX_ALLOW_DOWNLOAD=1 (passed to load_model as local_files_only).
# CINVAR_WHISPERX_DEVICE: cpu (default, int8) or cuda (float16).
import inspect
import json
import os
import sys

audio, out = sys.argv[1], sys.argv[2]
lang = sys.argv[3] if len(sys.argv) > 3 and sys.argv[3] else None
prompt = sys.argv[4] if len(sys.argv) > 4 and sys.argv[4] else None
model_name = os.environ.get("CINVAR_WHISPERX_MODEL", "base")
root = os.environ.get("CINVAR_WHISPERX_CACHE") or None
allow_download = os.environ.get("CINVAR_WHISPERX_ALLOW_DOWNLOAD") == "1"
device = os.environ.get("CINVAR_WHISPERX_DEVICE", "cpu")
compute_type = "float16" if device == "cuda" else "int8"


def fail(msg):
    print("whisperx helper: " + msg, file=sys.stderr)
    sys.exit(3)


try:
    import whisperx
except Exception as e:  # pragma: no cover - environment problem
    fail("whisperx is not importable in this Python environment: %s" % e)

params = inspect.signature(whisperx.load_model).parameters
kwargs = {
    "compute_type": compute_type,
    "language": lang,
    "download_root": root,
    "local_files_only": not allow_download,
}
if "vad_method" in params:
    kwargs["vad_method"] = "silero"
kwargs = {k: v for k, v in kwargs.items() if k in params}

try:
    model = whisperx.load_model(model_name, device, **kwargs)
except Exception as e:
    fail("cannot load model %r (set CINVAR_WHISPERX_ALLOW_DOWNLOAD=1 to fetch it): %s" % (model_name, e))

audio_arr = whisperx.load_audio(audio)


def transcribe_with_prompt(prompt_text):
    """WhisperX versions differ in their transcribe signature; an engine that does not
    accept a prompt must still run, so a TypeError falls back to the plain call."""
    kwargs = {"batch_size": 8, "language": lang}
    if prompt_text:
        kwargs["initial_prompt"] = prompt_text
    try:
        return model.transcribe(audio_arr, **kwargs)
    except TypeError:
        if "initial_prompt" not in kwargs:
            raise
        kwargs.pop("initial_prompt")
        return model.transcribe(audio_arr, **kwargs)


result = transcribe_with_prompt(prompt)
language = result.get("language") or lang or "en"

try:
    align_model, align_meta = whisperx.load_align_model(language_code=language, device=device)
except Exception as e:
    fail("cannot load the alignment model for %r (word timings need it): %s" % (language, e))

aligned = whisperx.align(
    result["segments"], align_model, align_meta, audio_arr, device, return_char_alignments=False
)

words, segments = [], []
for s in aligned.get("segments", []):
    text = (s.get("text") or "").strip()
    if text and "start" in s and "end" in s:
        segments.append({"text": text, "start": round(float(s["start"]), 3), "end": round(float(s["end"]), 3)})
    for w in s.get("words", []) or []:
        token = (w.get("word") or "").strip()
        # Alignment leaves numerals and some punctuation without times; those are skipped.
        if token and "start" in w and "end" in w:
            words.append({"word": token, "start": round(float(w["start"]), 3), "end": round(float(w["end"]), 3)})

with open(out, "w", encoding="utf-8") as f:
    json.dump({"language": language, "words": words, "segments": segments}, f, ensure_ascii=False)
print("whisperx helper: %d words, %d segments, device=%s" % (len(words), len(segments), device))
