//! Blender render-device discovery (Argon 5F): enumerate CUDA / OptiX / HIP / oneAPI / Metal and
//! fall back to CPU, reporting which backend was chosen and why.
//!
//! Nothing here guesses. Every candidate comes from a real "blender -b --factory-startup" run that
//! asks Cycles what it can see (see probe_script), so a machine with no driver, a build compiled
//! without a backend, or a runtime that claims a backend but enumerates no device all end up
//! distinguishable instead of silently rendering on the wrong processor.
//!
//! # Documented preference order
//!
//! Best first: OptiX, then CUDA, then HIP, then oneAPI, then Metal, and CPU last.
//!
//! * OptiX first: on NVIDIA RT hardware it is the fastest Cycles backend, and it drives the same
//!   cards CUDA would, through the RT cores.
//! * CUDA next: broadly available on NVIDIA, and the fast portable fallback when OptiX is not
//!   compiled into the build or the driver is older than OptiX requires.
//! * HIP then oneAPI then Metal: the vendor-native backends for AMD, Intel and Apple.
//! * CPU last: always correct, always slowest.
//!
//! Within the chosen backend the *first* device Blender enumerates wins, because Blender reports
//! devices in stable PCI probe order and CINVAR does not try to guess which GPU is faster (a bigger
//! card is not always the quicker one for Cycles). The CINVAR_BLENDER_DEVICE environment variable
//! overrides the pick by case-insensitive substring of the device name or id, which is how an
//! operator pins rendering away from a display GPU.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::util::proc::{self, ProcOpts};

/// Brackets that delimit the probe's JSON payload, so Blender's own chatter cannot corrupt it.
const BEGIN: &str = "CINVAR_GPU_BEGIN";
const END: &str = "CINVAR_GPU_END";

/// Environment override: case-insensitive substring of the device name or id to select.
pub const DEVICE_ENV: &str = "CINVAR_BLENDER_DEVICE";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    Optix,
    Cuda,
    Hip,
    OneApi,
    Metal,
    Cpu,
}

impl Backend {
    /// The compute_device_type identifier Blender uses for this backend.
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Optix => "OPTIX",
            Backend::Cuda => "CUDA",
            Backend::Hip => "HIP",
            Backend::OneApi => "ONEAPI",
            Backend::Metal => "METAL",
            Backend::Cpu => "CPU",
        }
    }

    /// The compute_device_type value Blender uses. CPU is not one of them: it is NONE.
    pub fn compute_type(self) -> &'static str {
        if self == Backend::Cpu {
            "NONE"
        } else {
            self.as_str()
        }
    }

    /// The device.type Cycles reports for a device of this backend.
    pub fn device_type(self) -> &'static str {
        match self {
            Backend::Optix => "OPTIX",
            Backend::Cuda => "CUDA",
            Backend::Hip => "HIP",
            Backend::OneApi => "ONEAPI",
            Backend::Metal => "METAL",
            Backend::Cpu => "CPU",
        }
    }

    /// Maps a device.type (or compute_device_type value) back to a backend.
    pub fn from_type(text: &str) -> Option<Backend> {
        match text.trim().to_ascii_uppercase().as_str() {
            "OPTIX" => Some(Backend::Optix),
            "CUDA" => Some(Backend::Cuda),
            "HIP" => Some(Backend::Hip),
            "ONEAPI" => Some(Backend::OneApi),
            "METAL" => Some(Backend::Metal),
            "CPU" => Some(Backend::Cpu),
            _ => None,
        }
    }

    pub fn is_gpu(self) -> bool {
        self != Backend::Cpu
    }
}

/// The documented preference order, best first.
pub const PREFERENCE: [Backend; 6] = [
    Backend::Optix,
    Backend::Cuda,
    Backend::Hip,
    Backend::OneApi,
    Backend::Metal,
    Backend::Cpu,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuDevice {
    pub backend: Backend,
    pub name: String,
    pub id: String,
    /// Whether Blender's preferences had the device ticked when it was enumerated.
    pub enabled: bool,
}

impl GpuDevice {
    fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_ascii_lowercase();
        !needle.is_empty()
            && (self.name.to_ascii_lowercase().contains(&needle)
                || self.id.to_ascii_lowercase().contains(&needle))
    }
}

/// What one machine's Cycles installation actually offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceProbe {
    pub blender_version: String,
    pub os: String,
    /// compute_device_type values this build exposes: a backend missing here is not compiled in.
    pub offered: Vec<String>,
    /// Every device Blender enumerated, deduplicated across backend buckets.
    pub devices: Vec<GpuDevice>,
    /// Backend buckets that failed to enumerate (informational, never fatal).
    pub errors: Vec<String>,
}

impl DeviceProbe {
    /// Devices Cycles reported for one backend.
    pub fn devices_for(&self, backend: Backend) -> Vec<&GpuDevice> {
        self.devices.iter().filter(|d| d.backend == backend).collect()
    }

    /// Whether the build exposes this backend at all (CPU always does).
    pub fn offers(&self, backend: Backend) -> bool {
        if backend == Backend::Cpu {
            return true;
        }
        self.offered
            .iter()
            .any(|o| o.eq_ignore_ascii_case(backend.as_str()))
    }

    /// A backend is usable only when the build offers it *and* real hardware showed up.
    pub fn usable(&self, backend: Backend) -> bool {
        if backend == Backend::Cpu {
            return true;
        }
        self.offers(backend) && !self.devices_for(backend).is_empty()
    }

    /// Why a backend cannot be used, or None when it can.
    pub fn rejection(&self, backend: Backend) -> Option<String> {
        if self.usable(backend) {
            return None;
        }
        if !self.offers(backend) {
            return Some(format!(
                "{} is not compiled into this Blender build (compute_device_type offers {})",
                backend.as_str(),
                if self.offered.is_empty() {
                    "nothing".to_string()
                } else {
                    self.offered.join(",")
                }
            ));
        }
        Some(format!(
            "{} is offered by this build but enumerated no {} device (no runtime/driver)",
            backend.as_str(),
            backend.device_type()
        ))
    }
}

/// The backend and device CINVAR will render with, plus why everything better was passed over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceChoice {
    pub backend: Backend,
    /// The chosen device; None only when falling back to CPU on a build that listed no CPU device.
    pub device: Option<GpuDevice>,
    pub preference: Vec<Backend>,
    /// Backends ranked above the winner, with the reason each was skipped.
    pub rejected: Vec<(Backend, String)>,
    /// Devices that were usable but not chosen.
    pub alternatives: Vec<String>,
    /// None when the first-choice backend was available.
    pub fallback_reason: Option<String>,
}

impl DeviceChoice {
    /// One line naming the decision and the fallback reason, for logs and CLI output.
    pub fn diagnosis(&self) -> String {
        let chosen = match &self.device {
            Some(device) => format!("{} device {:?}", self.backend.as_str(), device.name),
            None => format!("{} (no device list reported)", self.backend.as_str()),
        };
        let mut out = format!("chose {chosen}");
        if let Some(reason) = &self.fallback_reason {
            out.push_str("; fallback because ");
            out.push_str(reason);
        }
        if !self.alternatives.is_empty() {
            out.push_str(&format!("; also available: {}", self.alternatives.join(", ")));
        }
        out
    }

    /// Short label of the chosen device ("CUDA:Tesla P40", "CPU").
    pub fn label(&self) -> String {
        match &self.device {
            Some(device) if device.backend.is_gpu() => {
                format!("{}:{}", self.backend.as_str(), device.name)
            }
            _ => self.backend.as_str().to_string(),
        }
    }

    /// Python that applies this choice to an already-configured scene, for embedding in a render script.
    pub fn apply_script(&self) -> String {
        let mut out = String::new();
        out.push_str("import bpy as _cinvar_bpy\n");
        out.push_str(
            "_cinvar_prefs = _cinvar_bpy.context.preferences.addons['cycles'].preferences\n",
        );
        out.push_str(&format!(
            "_cinvar_prefs.compute_device_type = {}\n",
            py_str(self.backend.compute_type())
        ));
        out.push_str("try:\n");
        out.push_str(&format!(
            "    _cinvar_prefs.get_devices_for_type({})\n",
            py_str(self.backend.compute_type())
        ));
        out.push_str("except Exception:\n");
        out.push_str(
            "    try:\n        _cinvar_prefs.get_devices()\n    except Exception:\n        pass\n",
        );
        out.push_str("_cinvar_scene = _cinvar_bpy.context.scene\n");
        out.push_str("_cinvar_scene.render.engine = 'CYCLES'\n");
        if let Some(device) = self.device.as_ref().filter(|d| d.backend.is_gpu()) {
            out.push_str(&format!(
                "_cinvar_wanted = {}\n_cinvar_wanted_id = {}\n",
                py_str(&device.name),
                py_str(&device.id)
            ));
            out.push_str(
                "for _cinvar_dev in _cinvar_prefs.devices:\n    _cinvar_dev.use = (_cinvar_dev.name == _cinvar_wanted or getattr(_cinvar_dev, 'id', '') == _cinvar_wanted_id)\n",
            );
            out.push_str("_cinvar_scene.cycles.device = 'GPU'\n");
        } else {
            out.push_str("_cinvar_scene.cycles.device = 'CPU'\n");
        }
        out
    }
}

fn py_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// The Python Blender runs to enumerate Cycles devices: it asks the build which backends it has
/// (the bl_rna enum), enumerates each, and prints one JSON line between the markers.
pub fn probe_script() -> String {
    r#"import json
import sys

import bpy

# Every backend Blender has ever shipped. A build only accepts the ones it was compiled with:
# setting compute_device_type to a missing one raises TypeError, which is the honest signal that the
# backend does not exist here. The bl_rna enum cannot be trusted for this: it is dynamic and reports
# no items until something asks it to, so the list is discovered by asking.
CANDIDATES = ["CUDA", "OPTIX", "HIP", "ONEAPI", "METAL", "NONE"]

out = {"blender": bpy.app.version_string, "os": sys.platform, "offered": [], "devices": {}, "errors": {}}
try:
    prefs = bpy.context.preferences.addons["cycles"].preferences
except Exception as exc:
    out["errors"]["cycles"] = "%s: %s" % (type(exc).__name__, exc)
    prefs = None

if prefs is not None:
    declared = []
    for attr in ("enum_items_static", "enum_items"):
        try:
            items = getattr(prefs.bl_rna.properties["compute_device_type"], attr, None)
            declared = [i.identifier for i in items] if items else []
        except Exception:
            declared = []
        if declared:
            break
    for kind in CANDIDATES:
        try:
            prefs.compute_device_type = kind
        except Exception as exc:
            out["errors"][kind] = "not available in this build (%s)" % type(exc).__name__
            continue
        try:
            if hasattr(prefs, "get_devices_for_type"):
                prefs.get_devices_for_type(kind)
            else:
                prefs.get_devices()
        except Exception as exc:
            out["errors"][kind] = "%s: %s" % (type(exc).__name__, exc)
            continue
        out["offered"].append(kind)
        out["devices"][kind] = [
            {"type": str(d.type), "name": str(d.name), "id": str(getattr(d, "id", "")), "use": bool(d.use)}
            for d in prefs.devices
        ]
    for kind in declared:
        if kind not in out["offered"]:
            out["offered"].append(kind)
    # CPU is not a compute_device_type value: Cycles always reports a CPU device, so record one bucket.
    for _kind, listed in list(out["devices"].items()):
        if any(d["type"].upper() == "CPU" for d in listed):
            out["devices"]["CPU"] = [d for d in listed if d["type"].upper() == "CPU"]
            break

print("CINVAR_GPU_BEGIN")
print(json.dumps(out, sort_keys=True))
print("CINVAR_GPU_END")
"#
    .to_string()
}

#[derive(Deserialize)]
struct RawDevice {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    id: String,
    #[serde(rename = "use")]
    enabled: bool,
}

#[derive(Deserialize)]
struct RawProbe {
    blender: String,
    os: String,
    offered: Vec<String>,
    devices: std::collections::HashMap<String, Vec<RawDevice>>,
    #[serde(default)]
    errors: std::collections::HashMap<String, String>,
}

/// Extracts the JSON payload the probe printed between its markers.
///
/// Every non-empty line between the markers is joined: the probe prints one line, but a report that
/// was pretty-printed (a future probe, or a hand-made fixture) must parse the same way.
fn payload(stdout: &str) -> Option<String> {
    let mut inside = false;
    let mut payload = String::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed == BEGIN {
            inside = true;
            continue;
        }
        if trimmed == END {
            return if payload.is_empty() { None } else { Some(payload) };
        }
        if inside && !trimmed.is_empty() {
            payload.push_str(trimmed);
        }
    }
    None
}

/// Parses the probe's stdout. Pure, so the whole decision path is testable from captured output.
pub fn parse_probe(stdout: &str) -> Result<DeviceProbe> {
    let line = payload(stdout).ok_or_else(|| {
        Error::new(
            ErrorCode::DependencyMissing,
            "Blender did not report any Cycles device",
        )
        .with_hint("check that the Blender build includes Cycles and that the GPU driver loads")
    })?;
    let raw: RawProbe = serde_json::from_str(&line).map_err(|e| {
        Error::new(
            ErrorCode::Protocol,
            format!("unreadable Blender device report: {e}"),
        )
    })?;

    let mut devices: Vec<GpuDevice> = Vec::new();
    for listed in raw.devices.values() {
        for device in listed {
            // The device's own type is authoritative: a bucket can echo devices of another backend.
            let Some(backend) = Backend::from_type(&device.kind) else {
                continue;
            };
            let duplicated = devices
                .iter()
                .any(|d| d.backend == backend && d.id == device.id && d.name == device.name);
            if duplicated {
                continue;
            }
            devices.push(GpuDevice {
                backend,
                name: device.name.clone(),
                id: device.id.clone(),
                enabled: device.enabled,
            });
        }
    }

    // Preference order, then name: stable regardless of the map's iteration order.
    devices.sort_by_key(|d| {
        let rank = PREFERENCE
            .iter()
            .position(|b| *b == d.backend)
            .unwrap_or(PREFERENCE.len());
        (rank, d.name.clone())
    });

    let mut errors: Vec<String> = raw
        .errors
        .iter()
        .map(|(kind, message)| format!("{kind}: {message}"))
        .collect();
    errors.sort();

    Ok(DeviceProbe {
        blender_version: raw.blender,
        os: raw.os,
        offered: raw.offered,
        devices,
        errors,
    })
}

/// Walks PREFERENCE and returns the first usable backend, with why the better ones were not.
pub fn choose(probe: &DeviceProbe, override_name: Option<&str>) -> Result<DeviceChoice> {
    let preference: Vec<Backend> = PREFERENCE.to_vec();
    let mut rejected: Vec<(Backend, String)> = Vec::new();
    let mut winner: Option<(Backend, Option<GpuDevice>)> = None;

    for backend in &preference {
        if let Some(reason) = probe.rejection(*backend) {
            rejected.push((*backend, reason));
            continue;
        }
        let candidates = probe.devices_for(*backend);
        if let Some(needle) = override_name.map(str::trim).filter(|n| !n.is_empty()) {
            if !candidates.iter().any(|d| d.matches(needle)) {
                let names: Vec<&str> = candidates.iter().map(|d| d.name.as_str()).collect();
                return Err(Error::new(
                    ErrorCode::InvalidInput,
                    format!(
                        "{DEVICE_ENV}={needle:?} matches no {} device (available: {})",
                        backend.as_str(),
                        if names.is_empty() {
                            "none".to_string()
                        } else {
                            names.join(", ")
                        }
                    ),
                ));
            }
        }
        let picked = candidates
            .iter()
            .find(|d| d.matches(override_name.unwrap_or("")))
            .or_else(|| candidates.first())
            .map(|d| (*d).clone());
        winner = Some((*backend, picked));
        break;
    }

    let (backend, device) = winner.ok_or_else(|| {
        Error::new(
            ErrorCode::DependencyMissing,
            "Blender enumerated no usable render device, not even a CPU",
        )
        .with_hint("run blender -b --factory-startup by hand and check the Cycles add-on loads")
    })?;

    let winner_rank = preference.iter().position(|p| *p == backend);
    let alternatives: Vec<String> = probe
        .devices
        .iter()
        .filter(|d| match &device {
            Some(chosen) => !(d.backend == chosen.backend && d.id == chosen.id),
            None => true,
        })
        .filter(|d| probe.usable(d.backend))
        .map(|d| format!("{}:{}", d.backend.as_str(), d.name))
        .collect();

    let fallback_reason = rejected
        .iter()
        .filter(|(b, _)| preference.iter().position(|p| p == b) < winner_rank)
        .map(|(b, reason)| format!("{}:{}", b.as_str(), reason))
        .next();

    Ok(DeviceChoice {
        backend,
        device,
        preference,
        rejected,
        alternatives,
        fallback_reason,
    })
}

/// Runs the probe against a real Blender executable and decides the render device.
pub fn probe(blender: &Path, override_name: Option<&str>) -> Result<(DeviceProbe, DeviceChoice)> {
    let script = tempfile::Builder::new()
        .prefix("cinvar-gpu-probe-")
        .suffix(".py")
        .tempfile()
        .map_err(|e| Error::new(ErrorCode::Io, format!("temp probe script: {e}")))?;
    std::fs::write(script.path(), probe_script().as_bytes())
        .map_err(|e| Error::new(ErrorCode::Io, format!("write probe script: {e}")))?;
    let args = vec![
        "-b".to_string(),
        "--factory-startup".to_string(),
        "-P".to_string(),
        script.path().to_string_lossy().to_string(),
    ];
    let out = proc::run(blender, &args, &ProcOpts::default().with_timeout(180))?;
    let parsed = match parse_probe(&out.stdout) {
        Ok(parsed) => parsed,
        Err(err) => {
            return Err(err.with_hint(format!(
                "blender said: {}",
                crate::secrets::redact(&out.tail_stderr(8))
            )))
        }
    };
    let choice = choose(&parsed, override_name)?;
    Ok((parsed, choice))
}

/// Convenience: discover the local Blender and decide its render device.
pub fn detect() -> Result<(crate::blender::BlenderInfo, DeviceProbe, DeviceChoice)> {
    let info = crate::blender::detect()?;
    let override_name = std::env::var(DEVICE_ENV).ok();
    let (probe, choice) = probe(&info.path, override_name.as_deref())?;
    Ok((info, probe, choice))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from the development machine (Blender 4.0.2 on Ubuntu 24.04): an RTX 3060 Ti and a
    /// Tesla P40 on a build with no OptiX. The escaping of the probe's own error strings was folded
    /// onto one line so the whole payload is a single character-faithful JSON line, which is exactly
    /// the shape the probe prints.
    const CAPTURED_TWO_GPU: &str = r#"Blender 4.0.2
Read prefs: /home/u/.config/blender/4.0/config/userpref.blend
CINVAR_GPU_BEGIN
{"blender": "4.0.2", "devices": {"CPU": [{"id": "CPU", "name": "Intel Core i5-10600K CPU @ 4.10GHz", "type": "CPU", "use": false}], "CUDA": [{"id": "CUDA_NVIDIA GeForce RTX 3060 Ti_0000:01:00", "name": "NVIDIA GeForce RTX 3060 Ti", "type": "CUDA", "use": true}, {"id": "CUDA_Tesla P40_0000:03:00", "name": "Tesla P40", "type": "CUDA", "use": true}, {"id": "CPU", "name": "Intel Core i5-10600K CPU @ 4.10GHz", "type": "CPU", "use": false}], "HIP": [{"id": "CUDA_NVIDIA GeForce RTX 3060 Ti_0000:01:00", "name": "NVIDIA GeForce RTX 3060 Ti", "type": "CUDA", "use": true}, {"id": "CPU", "name": "Intel Core i5-10600K CPU @ 4.10GHz", "type": "CPU", "use": false}], "NONE": [{"id": "CPU", "name": "Intel Core i5-10600K CPU @ 4.10GHz", "type": "CPU", "use": false}]}, "errors": {"ONEAPI": "TypeError: enum ONEAPI not found in (NONE, CUDA, HIP)", "OPTIX": "TypeError: enum OPTIX not found in (NONE, CUDA, HIP)"}, "offered": ["CUDA", "HIP", "NONE"], "os": "linux"}
CINVAR_GPU_END
Blender quit
"#;

    /// A headless worker with no GPU at all.
    const CAPTURED_CPU_ONLY: &str = r#"CINVAR_GPU_BEGIN
{"blender": "4.0.2", "devices": {"NONE": [{"id": "CPU", "name": "Intel Core i5-10600K CPU @ 4.10GHz", "type": "CPU", "use": false}]}, "offered": ["NONE", "CUDA"], "os": "linux", "errors": {}}
CINVAR_GPU_END
"#;

    fn probe_of(text: &str) -> DeviceProbe {
        parse_probe(text).expect("captured output parses")
    }

    #[test]
    fn parses_real_two_gpu_report() {
        let probe = probe_of(CAPTURED_TWO_GPU);
        assert_eq!(probe.blender_version, "4.0.2");
        assert_eq!(probe.offered, vec!["CUDA", "HIP", "NONE"]);
        // Two distinct GPU devices plus one CPU device: GPUs are echoed into several buckets.
        assert_eq!(probe.devices.len(), 3);
        let gpus: Vec<&str> = probe
            .devices_for(Backend::Cuda)
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(gpus, vec!["NVIDIA GeForce RTX 3060 Ti", "Tesla P40"]);
        assert_eq!(probe.devices_for(Backend::Cpu).len(), 1);
        assert_eq!(probe.errors.len(), 2, "probe errors are kept for diagnostics");
    }

    #[test]
    fn chooses_cuda_when_build_has_no_optix() {
        let probe = probe_of(CAPTURED_TWO_GPU);
        let choice = choose(&probe, None).unwrap();
        assert_eq!(choice.backend, Backend::Cuda);
        assert_eq!(
            choice.device.as_ref().unwrap().name,
            "NVIDIA GeForce RTX 3060 Ti"
        );
        let reason = choice.fallback_reason.expect("a fallback must be explained");
        assert!(reason.starts_with("OPTIX:"), "reason was {}", reason);
        assert!(
            reason.contains("not compiled into this Blender build"),
            "reason was {}",
            reason
        );
        // HIP is offered by the enum but enumerates no HIP device: skipped, not chosen.
        assert!(!probe.usable(Backend::Hip));
        assert!(choice.alternatives.iter().any(|a| a.contains("Tesla P40")));
    }

    #[test]
    fn hip_without_hip_devices_is_rejected() {
        let probe = probe_of(CAPTURED_TWO_GPU);
        let reason = probe.rejection(Backend::Hip).unwrap();
        assert!(
            reason.contains("enumerated no HIP device"),
            "reason was {}",
            reason
        );
    }

    #[test]
    fn cpu_only_machine_falls_back_to_cpu() {
        let probe = probe_of(CAPTURED_CPU_ONLY);
        let choice = choose(&probe, None).unwrap();
        assert_eq!(choice.backend, Backend::Cpu);
        assert_eq!(
            choice.device.as_ref().unwrap().name,
            "Intel Core i5-10600K CPU @ 4.10GHz"
        );
        let reason = choice.fallback_reason.clone().unwrap();
        assert!(reason.starts_with("OPTIX:"), "reason was {}", reason);
        assert!(choice.diagnosis().contains("fallback because"));
        assert!(choice.apply_script().contains("device = 'CPU'"));
    }

    #[test]
    fn optix_wins_when_the_build_offers_it() {
        let text = r#"CINVAR_GPU_BEGIN
{"blender": "4.1.0", "offered": ["NONE", "CUDA", "OPTIX"], "os": "linux", "errors": {},
 "devices": {"OPTIX": [{"type": "OPTIX", "name": "Tesla P40", "id": "OPTIX_Tesla P40_0000:03:00", "use": true}],
             "CUDA": [{"type": "CUDA", "name": "Tesla P40", "id": "CUDA_Tesla P40_0000:03:00", "use": true}]}}
CINVAR_GPU_END"#;
        let probe = probe_of(text);
        let choice = choose(&probe, None).unwrap();
        assert_eq!(choice.backend, Backend::Optix);
        assert!(choice.fallback_reason.is_none());
        assert_eq!(choice.label(), "OPTIX:Tesla P40");
        let script = choice.apply_script();
        assert!(script.contains("compute_device_type = 'OPTIX'"));
        assert!(script.contains("device = 'GPU'"));
        assert!(script.contains("Tesla P40"));
    }

    #[test]
    fn device_override_selects_by_substring_and_fails_loudly() {
        let probe = probe_of(CAPTURED_TWO_GPU);
        let choice = choose(&probe, Some("p40")).unwrap();
        assert_eq!(choice.device.as_ref().unwrap().name, "Tesla P40");
        // Case-insensitive, and an id fragment works too.
        assert_eq!(
            choose(&probe, Some("0000:03:00"))
                .unwrap()
                .device
                .unwrap()
                .name,
            "Tesla P40"
        );
        let err = choose(&probe, Some("radeon")).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(
            err.message.contains("Tesla P40"),
            "message was {}",
            err.message
        );
    }

    #[test]
    fn missing_payload_is_an_error_not_a_guess() {
        let err = parse_probe("Blender 4.0.2\nError: Cycles add-on failed\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::DependencyMissing);
        // A payload cut off mid-line (killed probe) is rejected too.
        let truncated = "CINVAR_GPU_BEGIN\n{\"blender\": \"4.0.2\"\nCINVAR_GPU_END\n";
        assert!(parse_probe(truncated).is_err());
    }

    #[test]
    fn unknown_device_types_are_ignored() {
        let text = r#"CINVAR_GPU_BEGIN
{"blender": "4.2.0", "offered": ["NONE", "VULKAN"], "os": "linux", "errors": {},
 "devices": {"VULKAN": [{"type": "VULKAN", "name": "Fake GPU", "id": "V", "use": true},
                        {"type": "CPU", "name": "Ryzen", "id": "CPU", "use": false}]}}
CINVAR_GPU_END"#;
        let probe = probe_of(text);
        assert_eq!(
            probe.devices.len(),
            1,
            "the unknown VULKAN device is dropped, CPU kept"
        );
        assert_eq!(choose(&probe, None).unwrap().backend, Backend::Cpu);
    }

    /// Runs the probe against the Blender on this machine and prints what it found, including the
    /// device override path. Opt-in: it needs a GPU host (or at least a working Blender).
    ///
    /// cargo test --release --lib real_probe -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_probe_reports_this_machine() {
        let (info, probe, choice) = detect().expect("a local Blender answers the probe");
        println!("blender={} os={} offered={:?}", info.version, probe.os, probe.offered);
        for device in &probe.devices {
            println!(
                "DEVICE {}:{} id={} enabled={}",
                device.backend.as_str(),
                device.name,
                device.id,
                device.enabled
            );
        }
        println!("CHOSEN {}", choice.label());
        println!("DIAGNOSIS {}", choice.diagnosis());
        for (backend, reason) in &choice.rejected {
            println!("REJECTED {}", backend.as_str());
            println!("  reason {}", reason);
        }
        // The probe must have asked a real Blender, and every device must map to a known backend.
        assert!(!probe.offered.is_empty(), "the build reported its backend enum");
        assert!(!probe.devices.is_empty(), "at least the CPU device is enumerated");

        // An override selects the named device when one exists, on real hardware.
        if let Some(other) = probe
            .devices
            .iter()
            .find(|d| d.backend.is_gpu() && !d.name.contains("3060"))
        {
            let fragment = other.name.split_whitespace().next().unwrap_or("gpu");
            let pinned = choose(&probe, Some(fragment)).expect("override resolves");
            println!("OVERRIDE {:?} -> {}", fragment, pinned.label());
            assert_eq!(pinned.device.as_ref().map(|d| d.name.clone()), Some(other.name.clone()));
        }
    }

    #[test]
    fn preference_order_is_the_documented_one() {
        assert_eq!(
            PREFERENCE,
            [
                Backend::Optix,
                Backend::Cuda,
                Backend::Hip,
                Backend::OneApi,
                Backend::Metal,
                Backend::Cpu
            ]
        );
        // CPU is always usable; a GPU backend never is without a device.
        let probe = probe_of(CAPTURED_CPU_ONLY);
        assert!(probe.usable(Backend::Cpu));
        assert!(!probe.usable(Backend::Cuda));
    }
}
