//! Font files for drawtext.
//!
//! A drawtext with a font *family* (`font=Arial`) can only be resolved through fontconfig, and
//! the FFmpeg builds shipped on Windows have no fontconfig configuration file: instead of failing
//! with a message they die with an access violation (exit status 0xC0000005 / -1073741819) after
//! `Fontconfig error: Cannot load default config file`. An explicit `fontfile=` never touches
//! fontconfig, so Windows always gets a real font file.

use std::path::{Path, PathBuf};

/// What drawtext is told about the font.
pub(super) enum FontChoice {
    /// An existing font file: `fontfile=`, no fontconfig involved.
    File(PathBuf),
    /// A family name for fontconfig: `font=`.
    Family(String),
}

/// Quotes a path for a filtergraph option value.
///
/// Paths are single-quoted so commas and semicolons stay literal, backslashes become forward
/// slashes (Windows accepts both, and a filtergraph backslash is an escape), and the drive colon
/// is escaped because `:` separates options. Returns `None` for a path ffmpeg cannot express; the
/// caller must then stage the file (see [`fontfile_option`]) instead of dropping the option.
pub(super) fn filter_path(path: &Path) -> Option<String> {
    if crate::util::paths::filtergraph_inexpressible(path) {
        return None;
    }
    let text = path.to_string_lossy().replace('\\', "/");
    Some(format!("'{}'", text.replace(':', "\\:")))
}

/// The `fontfile=` value for a font file.
///
/// A path a filtergraph cannot carry is copied to a quote-free name first, because dropping the
/// option is not a neutral fallback: drawtext then resolves the family through fontconfig, which
/// renders a *different* face on Linux and aborts the process on the FFmpeg builds shipped for
/// Windows. `None` only when the file cannot be staged either.
pub(super) fn fontfile_option(path: &Path) -> Option<String> {
    if let Some(quoted) = filter_path(path) {
        return Some(quoted);
    }
    let staged = crate::util::paths::stage_for_filtergraph(path).ok()?;
    filter_path(&staged)
}

/// Windows font file names (as installed in the system font directory) for the families people
/// actually ask for. Compared case- and space-insensitively.
pub(super) fn windows_font_file_for_family(family: &str) -> Option<&'static str> {
    let key: String = family
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    let file = match key.as_str() {
        "arial" => "arial.ttf",
        "arialblack" => "ariblk.ttf",
        "calibri" => "calibri.ttf",
        "cambria" => "cambria.ttc",
        "comicsansms" | "comicsans" => "comic.ttf",
        "consolas" => "consola.ttf",
        "couriernew" | "courier" => "cour.ttf",
        "georgia" => "georgia.ttf",
        "impact" => "impact.ttf",
        "segoeui" | "segoe" => "segoeui.ttf",
        "tahoma" => "tahoma.ttf",
        "timesnewroman" | "times" => "times.ttf",
        "trebuchetms" | "trebuchet" => "trebuc.ttf",
        "verdana" => "verdana.ttf",
        _ => return None,
    };
    Some(file)
}

/// The default font files tried when nothing was asked for.
const DEFAULT_FILES: &[&str] = &["segoeui.ttf", "arial.ttf", "tahoma.ttf", "calibri.ttf"];

fn looks_like_path(font: &str) -> bool {
    font.contains('/')
        || font.contains('\\')
        || font.ends_with(".ttf")
        || font.ends_with(".otf")
        || font.ends_with(".ttc")
}

/// Chooses a drawtext font. `prefer_file` is true where fontconfig cannot be trusted (Windows);
/// `font_dir` is that platform's font directory. Kept parameterised so both branches are testable
/// on one machine.
pub(super) fn choose(font: Option<&str>, prefer_file: bool, font_dir: &Path) -> Option<FontChoice> {
    let default_file = || {
        if !prefer_file {
            return None;
        }
        DEFAULT_FILES
            .iter()
            .map(|name| font_dir.join(name))
            .find(|path| path.is_file())
            .map(FontChoice::File)
    };
    let Some(font) = font.map(str::trim).filter(|f| !f.is_empty()) else {
        return default_file();
    };
    if looks_like_path(font) {
        let path = Path::new(font);
        if path.is_file() {
            return Some(FontChoice::File(path.to_path_buf()));
        }
        // A path that does not exist is reported by the render itself; keep it as given.
        return Some(FontChoice::Family(font.to_string()));
    }
    if prefer_file {
        if let Some(name) = windows_font_file_for_family(font) {
            let path = font_dir.join(name);
            if path.is_file() {
                return Some(FontChoice::File(path));
            }
        }
        // An unmapped family would send drawtext to fontconfig, which crashes there.
        if let Some(choice) = default_file() {
            return Some(choice);
        }
    }
    Some(FontChoice::Family(font.to_string()))
}

/// The Windows font directory, empty (so no file is ever found) elsewhere.
fn platform_font_dir() -> PathBuf {
    if cfg!(windows) {
        let windir = std::env::var("WINDIR")
            .or_else(|_| std::env::var("SystemRoot"))
            .unwrap_or_else(|_| "C:\\Windows".to_string());
        PathBuf::from(windir).join("Fonts")
    } else {
        PathBuf::new()
    }
}

/// The font drawtext should use on this platform.
pub(super) fn drawtext_font(font: Option<&str>) -> Option<FontChoice> {
    choose(font, cfg!(windows), &platform_font_dir())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_become_filtergraph_safe() {
        let quoted = filter_path(Path::new("C:\\Windows\\Fonts\\arial.ttf")).unwrap();
        assert_eq!(quoted, "'C\\:/Windows/Fonts/arial.ttf'");
        assert_eq!(
            filter_path(Path::new("/usr/share/fonts/x.ttf")).unwrap(),
            "'/usr/share/fonts/x.ttf'"
        );
        assert!(filter_path(Path::new("/tmp/it's.ttf")).is_none());
    }

    /// A path holding an apostrophe used to lose `fontfile=` entirely, which silently renders a
    /// different face (and aborts the Windows FFmpeg builds inside fontconfig). It is staged now.
    #[test]
    fn an_apostrophe_font_path_is_staged_instead_of_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let font_dir = dir.path().join("owner's fonts");
        std::fs::create_dir_all(&font_dir).unwrap();
        let font = font_dir.join("DejaVu Serif.ttf");
        std::fs::write(&font, b"pretend font bytes").unwrap();

        assert!(
            filter_path(&font).is_none(),
            "ffmpeg cannot express an apostrophe inside '...', so the raw path has no form"
        );
        let option = fontfile_option(&font).expect("the option is staged, never silently dropped");
        assert_eq!(
            option.matches('\'').count(),
            2,
            "only the wrapping quotes remain, so the path itself is quote-free: {option}"
        );
        assert!(option.starts_with('\''), "{option}");
        assert!(option.contains("filterpaths"), "{option}");
        assert!(option.ends_with(".ttf'"), "{option}");
        let staged = &option[1..option.len() - 1];
        assert_eq!(
            std::fs::read(staged).expect("the staged copy exists"),
            b"pretend font bytes",
            "the stand-in holds the font the caller asked for"
        );
    }

    /// Everything ffmpeg can carry keeps its real path: staging is for the inexpressible only.
    #[test]
    fn expressible_font_paths_are_not_staged() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["Fonts With Spaces", "a,b", "hellö", "视频"] {
            let path = dir.path().join(name).join("face.ttf");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
            let option = fontfile_option(&path).unwrap();
            assert!(
                !option.contains("filterpaths"),
                "{name} is used as given: {option}"
            );
            assert!(
                option.starts_with('\'') && option.ends_with('\''),
                "{option}"
            );
        }
        // A drive-letter path keeps its escaped colon instead of being copied.
        let windows_style = Path::new("C:\\Windows\\Fonts\\arial.ttf");
        assert_eq!(
            filter_path(windows_style).as_deref(),
            Some("'C\\:/Windows/Fonts/arial.ttf'")
        );
    }

    #[test]
    fn family_names_map_to_windows_font_files() {
        assert_eq!(windows_font_file_for_family("Arial"), Some("arial.ttf"));
        assert_eq!(
            windows_font_file_for_family("Times New Roman"),
            Some("times.ttf")
        );
        assert_eq!(
            windows_font_file_for_family("Segoe UI"),
            Some("segoeui.ttf")
        );
        assert_eq!(
            windows_font_file_for_family("Courier New"),
            Some("cour.ttf")
        );
        assert_eq!(windows_font_file_for_family("No Such Font"), None);
    }

    #[test]
    fn windows_always_gets_a_font_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("arial.ttf"), b"").unwrap();
        match choose(None, true, dir.path()) {
            Some(FontChoice::File(path)) => assert!(path.ends_with("arial.ttf")),
            _ => panic!("a default font file is chosen when nothing was asked for"),
        }
        match choose(Some("Arial"), true, dir.path()) {
            Some(FontChoice::File(path)) => assert!(path.ends_with("arial.ttf")),
            _ => panic!("a family name maps to its font file"),
        }
        // An unmapped family must not reach fontconfig, which is what crashes there.
        match choose(Some("Exotic Face"), true, dir.path()) {
            Some(FontChoice::File(path)) => assert!(path.ends_with("arial.ttf")),
            _ => panic!("an unmapped family falls back to a real file"),
        }
    }

    #[test]
    fn fontconfig_platforms_keep_family_names() {
        let none = Path::new("/nonexistent-fonts");
        match choose(Some("DejaVu Sans"), false, none) {
            Some(FontChoice::Family(name)) => assert_eq!(name, "DejaVu Sans"),
            _ => panic!("a family name stays a family name"),
        }
        assert!(choose(None, false, none).is_none());
    }

    // Unverified on Windows in this session (no Windows runner): asserts the platform really has
    // the font file the Windows path resolves to.
    #[cfg(windows)]
    #[test]
    fn the_default_windows_font_file_exists() {
        match drawtext_font(None) {
            Some(FontChoice::File(path)) => assert!(path.is_file(), "{}", path.display()),
            _ => panic!("a font file is resolved on Windows"),
        }
    }
}
