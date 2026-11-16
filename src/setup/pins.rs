//! Pins and platform decisions behind `cinvar setup --pro-ai`.
//!
//! torch, torchaudio and torchvision must come from one CUDA channel. A torchaudio cu128 wheel on
//! top of a torch cu126 build aborts at import time ("Detected that PyTorch and TorchAudio were
//! compiled with different CUDA versions") and takes every torch-dependent component down with it,
//! so the stack is pinned together, installed in a single pip pass, and verified by import before a
//! component is reported ready.
//!
//! The pin follows whisperx, the most demanding consumer: whisperx 3.8 requires torch~=2.8.0 and
//! torchvision~=0.23.0, which demucs (torch>=2.1) accepts as well. One pair serves both venvs.

use std::path::{Path, PathBuf};

use crate::util::paths;
use crate::util::proc::{self, ProcOpts};

/// Data mount that holds model weights on this network's machines, when it is present.
const MODEL_MOUNT: &str = "/mnt/elyreon-data";
const MODEL_DIR: &str = "cinvar-models";

/// One mutually compatible torch stack, resolved from a single wheel channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TorchPin {
    pub torch: &'static str,
    pub torchaudio: &'static str,
    pub torchvision: &'static str,
    pub index: &'static str,
}

impl TorchPin {
    /// The exact pip requirements, all of which resolve from [TorchPin::index].
    pub fn requirements(self) -> Vec<String> {
        vec![
            format!("torch=={}", self.torch),
            format!("torchaudio=={}", self.torchaudio),
            format!("torchvision=={}", self.torchvision),
        ]
    }
}

/// CUDA 12.8 build: runs on any driver that supports CUDA 12.x (525.60.13 or newer).
pub const CUDA_PIN: TorchPin = TorchPin {
    torch: "2.8.0+cu128",
    torchaudio: "2.8.0+cu128",
    torchvision: "0.23.0+cu128",
    index: "https://download.pytorch.org/whl/cu128",
};

/// CPU-only build: no CUDA runtime, no nvidia-* wheels, works on every x86_64 Linux.
pub const CPU_PIN: TorchPin = TorchPin {
    torch: "2.8.0+cpu",
    torchaudio: "2.8.0+cpu",
    torchvision: "0.23.0+cpu",
    index: "https://download.pytorch.org/whl/cpu",
};

/// Which wheel channel the installer pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Cuda,
    Cpu,
}

impl Device {
    pub fn as_str(self) -> &'static str {
        match self {
            Device::Cuda => "cuda",
            Device::Cpu => "cpu",
        }
    }

    pub fn pin(self) -> TorchPin {
        match self {
            Device::Cuda => CUDA_PIN,
            Device::Cpu => CPU_PIN,
        }
    }

    /// `CINVAR_PRO_AI_DEVICE=cpu|cuda` overrides detection (CI, or a driver older than the pin).
    pub fn detect() -> Device {
        match std::env::var("CINVAR_PRO_AI_DEVICE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "cpu" => return Device::Cpu,
            "cuda" | "gpu" => return Device::Cuda,
            _ => {}
        }
        if nvidia_smi_ok() {
            Device::Cuda
        } else {
            Device::Cpu
        }
    }
}

fn nvidia_smi_ok() -> bool {
    let Some(exe) = proc::which("nvidia-smi") else {
        return false;
    };
    let args = vec![
        "--query-gpu=name".to_string(),
        "--format=csv,noheader".to_string(),
    ];
    matches!(
        proc::run(&exe, &args, &ProcOpts::default().with_timeout(20)),
        Ok(out) if out.success && !out.stdout.trim().is_empty()
    )
}

/// Root every component's weights are stored under: the data mount when it is mounted, else the
/// per-user data directory.
pub fn model_root() -> PathBuf {
    choose_model_root(Path::new(MODEL_MOUNT), paths::data_dir().join("models"))
}

pub fn choose_model_root(mount: &Path, fallback: PathBuf) -> PathBuf {
    if mount.is_dir() {
        mount.join(MODEL_DIR)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(version: &str) -> &str {
        version.rsplit_once('+').map(|(_, tag)| tag).unwrap_or("")
    }

    #[test]
    fn torch_torchaudio_and_torchvision_are_one_channel() {
        for pin in [CUDA_PIN, CPU_PIN] {
            let torch_core = pin.torch.split('+').next().unwrap();
            assert_eq!(torch_core, pin.torchaudio.split('+').next().unwrap());
            assert_eq!(tag(pin.torch), tag(pin.torchaudio));
            assert_eq!(tag(pin.torch), tag(pin.torchvision));
            assert!(pin.index.ends_with(tag(pin.torch)), "{pin:?} index/tag mismatch");
            let requirements = pin.requirements();
            assert_eq!(requirements.len(), 3);
            assert!(requirements[0].ends_with(pin.torch));
        }
        assert_ne!(CUDA_PIN.index, CPU_PIN.index);
        assert_ne!(tag(CUDA_PIN.torch), tag(CPU_PIN.torch));
    }

    #[test]
    fn model_root_prefers_the_data_mount() {
        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().join("mnt");
        let fallback = dir.path().join("home-models");
        assert_eq!(choose_model_root(&mount, fallback.clone()), fallback);
        std::fs::create_dir(&mount).unwrap();
        assert_eq!(choose_model_root(&mount, fallback), mount.join(MODEL_DIR));
    }
}
