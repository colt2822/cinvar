# OpenColorIO and LUTs

Needs a Python with `opencolorio` (`pip install opencolorio` in a venv) and `CINVAR_OCIO_PYTHON=/path/to/venv/bin/python`.
Verified with OpenColorIO 2.6.0.

- `cinvar.color.inspect {config?}` lists colour spaces (project `color/config.ocio`, `$OCIO`, else the builtin config).
- `cinvar.color.apply_transform {clip_id, source, destination}` bakes **one named transform** into a `.cube` LUT inside
  the project and applies it as a visible `lut` effect. Remove the effect to undo. Unknown names are errors.
- `cinvar.color.import_lut {path}` stages an existing `.cube`.
- **If you request no transform, pixels are untouched** - CINVAR never reinterprets colour silently.
- Limits: LUT values clamp to 0..1 (no HDR range); no per-shot display transforms or ACES pipeline management.
