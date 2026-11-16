//! Automatic skill selection. The assistant should not have to be told which skill to use: what the
//! user says ("i need this podcast edited", "make a short", "20 minute youtube video", "use blender")
//! picks the skill(s) deterministically, and their guidance is loaded into the model's context.
//!
//! Scoring is plain keyword/phrase matching, so it is predictable, testable, and costs no model call.
//! Skills the user installs themselves are matched on their name, title and tags.

use super::Skill;

#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub name: String,
    pub reason: String,
}

/// (skill name, [(phrase, weight)]). Single words match whole words (plural allowed); phrases with a
/// space, hyphen, colon or digit match as text.
const TRIGGERS: &[(&str, &[(&str, i32)])] = &[
    ("blender-animation", &[("blender", 8), ("3d", 4), ("3d title", 6), ("logo animation", 5), ("animated logo", 5), ("logo reveal", 5), ("render in blender", 8)]),
    ("podcast", &[("podcast", 8), ("episode", 2), ("interview", 3), ("talking head", 3), ("audiogram", 5), ("multicam", 4), ("multi cam", 4), ("two speakers", 3), ("guest", 2), ("host", 1)]),
    ("gaming", &[("gaming", 8), ("gameplay", 8), ("game footage", 7), ("highlight", 3), ("montage", 3), ("clutch", 4), ("kills", 3), ("twitch", 4), ("stream vod", 4), ("ranked", 3), ("fortnite", 5), ("valorant", 5), ("minecraft", 5), ("warzone", 5), ("apex", 4), ("call of duty", 5), ("league of legends", 5), ("counter strike", 5), ("cs2", 5), ("fps", 3), ("speedrun", 4)]),
    ("short-form", &[("make a short", 8), ("a short", 3), ("shorts", 7), ("tiktok", 8), ("reel", 6), ("vertical", 5), ("9:16", 7), ("social clip", 5), ("teaser", 3), ("viral", 3), ("instagram", 4), ("short form", 8)]),
    ("youtube", &[("youtube", 7), ("long form", 6), ("vlog", 5), ("video essay", 7), ("tutorial", 4), ("retention", 4), ("chapters", 3), ("thumbnail", 2), ("review video", 4), ("explainer", 4)]),
    ("documentary", &[("documentary", 8), ("mini doc", 7), ("b roll", 3), ("archival", 3), ("oral history", 5)]),
    ("cinematic", &[("cinematic", 8), ("film look", 5), ("short film", 7), ("trailer", 5), ("movie", 2), ("color grade", 3), ("colour grade", 3), ("anamorphic", 4)]),
    ("commercial", &[("commercial", 8), ("advert", 7), ("advertisement", 7), ("promo", 6), ("product video", 6), ("brand video", 5), ("ad", 3), ("sizzle", 4), ("spot", 2)]),
    ("video-restoration", &[("restore", 6), ("restoration", 8), ("old footage", 6), ("vhs", 8), ("denoise", 5), ("upscale", 4), ("damaged", 3), ("grainy", 4), ("deinterlace", 5), ("film scan", 5)]),
    ("motion-graphics", &[("motion graphics", 8), ("lower third", 8), ("kinetic", 6), ("animated title", 6), ("title card", 4), ("intro", 2), ("outro", 2), ("typography", 4), ("end screen", 4), ("animated text", 5)]),
];

fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push(' ');
    let mut last_space = true;
    for c in text.to_lowercase().chars() {
        let keep = c.is_alphanumeric() || c == ':';
        if keep {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

/// Whole-word / whole-phrase match on normalized text, so "shorter" never matches "short".
fn has(norm: &str, phrase: &str) -> bool {
    let p = normalize(phrase);
    norm.contains(&p) || (!p.trim().contains(' ') && norm.contains(&format!("{}s ", p.trim_end().to_string() + "")) && norm.contains(&format!(" {}s ", p.trim())))
}

fn minutes_mentioned(norm: &str) -> Option<u32> {
    let words: Vec<&str> = norm.split_whitespace().collect();
    for w in words.windows(2) {
        if matches!(w[1], "minute" | "minutes" | "min" | "mins") {
            if let Ok(n) = w[0].parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

fn seconds_mentioned(norm: &str) -> Option<u32> {
    let words: Vec<&str> = norm.split_whitespace().collect();
    for w in words.windows(2) {
        if matches!(w[1], "second" | "seconds" | "sec" | "secs") {
            if let Ok(n) = w[0].parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

/// Choose up to `limit` skills for a request. Names a user typed ("use blender", "the podcast skill") win outright.
pub fn route(skills: &[Skill], request: &str, limit: usize) -> Vec<Pick> {
    let norm = normalize(request);
    let mut scored: Vec<(i32, Pick)> = Vec::new();
    for skill in skills {
        if skill.name == "decision-framework" {
            continue; // a planning aid, loaded only when asked for by name
        }
        let mut score = 0;
        let mut why: Vec<String> = Vec::new();
        let name_words = normalize(&skill.name);
        let title = normalize(&skill.title);
        let first = name_words.trim().split(' ').next().unwrap_or("").to_string();
        // "Named" means the user asked for it ("use blender", "the podcast skill"); merely saying the word is a normal keyword hit.
        let asked = norm.contains(" use ") || norm.contains(" using ") || norm.contains(" skill ") || norm.contains(" skills ");
        if asked && (has(&norm, &skill.name) || (title.trim().len() > 3 && norm.contains(&title)) || (first.len() >= 4 && has(&norm, &first))) {
            score += 100;
            why.push("you named it".into());
        }
        if let Some((_, table)) = TRIGGERS.iter().find(|(n, _)| *n == skill.name) {
            for (phrase, weight) in table.iter() {
                if has(&norm, phrase) {
                    score += weight;
                    why.push(format!("\"{}\"", phrase));
                }
            }
        } else {
            for tag in skill.tags.iter().chain(std::iter::once(&skill.name)) {
                if tag.len() >= 3 && has(&norm, tag) {
                    score += 4;
                    why.push(format!("\"{}\"", tag));
                }
            }
        }
        // length cues
        match skill.name.as_str() {
            "youtube" => {
                if minutes_mentioned(&norm).map(|m| m >= 8).unwrap_or(false) {
                    score += 4;
                    why.push("a long video".into());
                }
            }
            "short-form" => {
                if seconds_mentioned(&norm).map(|s| s <= 90).unwrap_or(false) || minutes_mentioned(&norm).map(|m| m <= 1).unwrap_or(false) {
                    score += 3;
                    why.push("a short runtime".into());
                }
            }
            _ => {}
        }
        if score >= 3 {
            let mut seen = std::collections::BTreeSet::new();
            why.retain(|w| seen.insert(w.clone()));
            scored.push((score, Pick { name: skill.name.clone(), reason: format!("matched {}", why.join(", ")) }));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    let top = scored.first().map(|s| s.0).unwrap_or(0);
    scored.into_iter().filter(|(s, _)| *s * 2 >= top || *s >= 100).take(limit.max(1)).map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(req: &str) -> Vec<String> {
        route(&super::super::embedded_skills(), req, 2).into_iter().map(|p| p.name).collect()
    }

    #[test]
    fn the_examples_from_real_use() {
        assert_eq!(names("use blender- for the logo intro")[0], "blender-animation");
        assert_eq!(names("i want a 20 minute youtube video")[0], "youtube");
        assert_eq!(names("i need this podcast edited")[0], "podcast");
        assert_eq!(names("edit this gaming footage")[0], "gaming");
        assert_eq!(names("make a short")[0], "short-form");
    }

    #[test]
    fn combined_intent_picks_both_with_the_output_format_first() {
        let n = names("make a short vertical clip from my podcast");
        assert_eq!(n[0], "short-form");
        assert!(n.contains(&"podcast".to_string()), "{n:?}");
    }

    #[test]
    fn unrelated_or_ambiguous_words_do_not_trigger() {
        assert!(names("hello").is_empty());
        assert!(names("make it shorter and tighter").is_empty(), "shorter is not 'short'");
        assert!(names("add a fade").is_empty());
        assert!(names("what time is it").is_empty());
    }

    #[test]
    fn naming_a_skill_wins_and_plural_and_case_are_handled() {
        assert_eq!(names("Use the podcast skill")[0], "podcast");
        assert_eq!(names("Cut me some TikToks")[0], "short-form");
        assert_eq!(names("restore this old VHS tape")[0], "video-restoration");
        assert_eq!(names("add lower thirds and a kinetic title")[0], "motion-graphics");
        assert_eq!(names("cut a 45 second teaser trailer for the product")[0], "short-form");
    }

    #[test]
    fn decision_framework_is_never_auto_loaded() {
        for req in ["plan my edit", "make a youtube video", "edit gameplay"] {
            assert!(!names(req).contains(&"decision-framework".to_string()));
        }
    }
}
