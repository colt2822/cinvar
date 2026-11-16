//! Custom vocabulary: the prompt built for engines, and the fact that it reaches the engine
//! as one argument (src/captions/vocab.rs, captions::run_engine_with_vocab).

use cinvar::captions::{self, vocab, Engine};

fn terms(raw: &[&str]) -> Vec<String> {
    raw.iter().map(|s| s.to_string()).collect()
}

#[test]
fn an_empty_vocabulary_produces_no_prompt_and_no_hotwords() {
    assert_eq!(vocab::prompt_for(&[]), None);
    assert_eq!(vocab::prompt_for(&terms(&["", "  "])), None);
    assert_eq!(vocab::hotwords_for(&[]), None);
}

#[test]
fn the_prompt_names_each_term_once_and_is_reproducible() {
    let v = terms(&["ZBrush", "zbrush", "Octane Render", "Pinch"]);
    let a = vocab::prompt_for(&v).unwrap();
    let b = vocab::prompt_for(&v).unwrap();
    assert_eq!(a, b, "the same vocabulary must build the same prompt");
    assert_eq!(a.matches("ZBrush").count(), 1, "{a}");
    assert!(a.contains("Octane Render"), "{a}");
    assert!(vocab::hotwords_for(&v).unwrap().contains("Pinch"));
    assert_eq!(
        vocab::prompt_for(&terms(&["ZBrush"])).unwrap(),
        "Glossary of names and terms used in this recording: ZBrush."
    );
}

#[cfg(unix)]
mod command_line {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in transcription engine: records its argv (one per line), then writes an SRT
    /// to its second argument. Nothing about a real engine is mocked - this is the same
    /// CINVAR_TRANSCRIBE_CMD channel an operator can configure.
    struct Stand {
        program: std::path::PathBuf,
        args_file: std::path::PathBuf,
        work: std::path::PathBuf,
    }

    impl Stand {
        fn new(dir: &std::path::Path) -> Stand {
            let program = dir.join("fake_engine.sh");
            let args_file = dir.join("args.txt");
            let body = format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '1\\n00:00:01,000 --> 00:00:02,000\\nHello there\\n\\n' > \"$2\"\n",
                args_file.display()
            );
            std::fs::write(&program, body).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::create_dir_all(dir.join("work")).unwrap();
            std::fs::write(dir.join("speech.wav"), b"RIFF").unwrap();
            Stand {
                program,
                args_file,
                work: dir.join("work"),
            }
        }

        fn argv(&self) -> Vec<String> {
            std::fs::read_to_string(&self.args_file)
                .unwrap()
                .lines()
                .map(|l| l.to_string())
                .collect()
        }

        fn run(&self, template_suffix: &str, vocabulary: &[String]) -> Vec<String> {
            let _ = std::fs::remove_file(&self.args_file);
            let template = format!("{} {{audio}} {{srt}}{template_suffix}", self.program.display());
            let srt = captions::run_engine_with_vocab(
                &Engine::Custom(template),
                &self.work.join("speech.wav"),
                &self.work,
                None,
                vocabulary,
            )
            .expect("the stand-in engine runs");
            assert!(srt.contains("Hello there"), "{srt}");
            self.argv()
        }
    }

    #[test]
    fn the_vocabulary_reaches_the_engine_as_one_argument() {
        let dir = tempfile::tempdir().unwrap();
        let stand = Stand::new(dir.path());
        let argv = stand.run(" {vocab}", &terms(&["ZBrush", "Octane Render"]));
        assert_eq!(argv.len(), 3, "audio, srt and the prompt: {argv:?}");
        let prompt = &argv[2];
        assert!(prompt.contains("ZBrush"), "{prompt}");
        assert!(prompt.contains("Octane Render"), "spaces survive as one argument: {prompt}");
        assert!(!prompt.contains('\n'), "the prompt cannot inject arguments");
    }

    #[test]
    fn an_empty_vocabulary_is_exactly_the_old_command() {
        let dir = tempfile::tempdir().unwrap();
        let stand = Stand::new(dir.path());
        // A template that does not mention {vocab} keeps its argument count either way.
        let plain = stand.run("", &[]);
        assert_eq!(plain.len(), 2, "audio and srt only: {plain:?}");
        let with_empty_vocab = stand.run("", &terms(&["", "  "]));
        assert_eq!(plain, with_empty_vocab, "an empty vocabulary adds no argument");

        // run_engine (no vocabulary at all) is the same call.
        let _ = std::fs::remove_file(&stand.args_file);
        let template = format!("{} {{audio}} {{srt}}", stand.program.display());
        captions::run_engine(
            &Engine::Custom(template),
            &stand.work.join("speech.wav"),
            &stand.work,
            None,
        )
        .unwrap();
        assert_eq!(stand.argv(), plain);
    }

    #[test]
    fn a_vocab_slot_stays_empty_when_there_is_no_vocabulary() {
        let dir = tempfile::tempdir().unwrap();
        let stand = Stand::new(dir.path());
        let argv = stand.run(" {vocab}", &[]);
        assert_eq!(argv.len(), 3, "{argv:?}");
        assert_eq!(argv[2], "", "an empty vocabulary substitutes an empty argument");
    }
}
