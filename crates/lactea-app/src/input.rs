//! Keyboard bindings, kept free of window state so they can be tested.

use winit::keyboard::{Key, NamedKey};

use crate::presets::Nav;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    Navigate(Nav),
    ToggleLock,
    ToggleFullscreen,
    /// Write every pipeline stage of the last frame as `lactea-debug-<frame>-<stage>.png` into
    /// `$LACTEA_DEBUG_DIR` (default: `<temp dir>/lactea-debug`).
    DumpStages,
    /// Cycle the MD3 auto hard cut mode off, 1..=7, off (MD3's F8; needs a MilkDrop 3 preset: `--compat milkdrop3` or auto).
    CycleAutoHardCut,
}

/// N/Right/Space next, P/Left previous, R random, L lock, F/F11 fullscreen, D dump pipeline stages, F8 auto hard cut mode, Esc/Q quit.
pub fn action_for_key(key: &Key) -> Option<Action> {
    match key {
        Key::Named(NamedKey::Escape) => Some(Action::Quit),
        Key::Named(NamedKey::ArrowRight | NamedKey::Space) => Some(Action::Navigate(Nav::Next)),
        Key::Named(NamedKey::ArrowLeft) => Some(Action::Navigate(Nav::Prev)),
        Key::Named(NamedKey::F11) => Some(Action::ToggleFullscreen),
        Key::Named(NamedKey::F8) => Some(Action::CycleAutoHardCut),
        Key::Character(c) => match c.to_ascii_lowercase().as_str() {
            "q" => Some(Action::Quit),
            "n" => Some(Action::Navigate(Nav::Next)),
            "p" => Some(Action::Navigate(Nav::Prev)),
            "r" => Some(Action::Navigate(Nav::Random)),
            "l" => Some(Action::ToggleLock),
            "f" => Some(Action::ToggleFullscreen),
            "d" => Some(Action::DumpStages),
            _ => None,
        },
        _ => None,
    }
}

/// Scripted key presses for tests: `"30:n,60:p,90:l"` = frame 30 -> `n`, ...
/// Key names: a single character or one of `space right left esc f11 f8`.
pub fn parse_script(s: &str) -> Result<Vec<(u64, Key)>, String> {
    let mut out = Vec::new();
    for item in s.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        let (frame, key) = item
            .split_once(':')
            .ok_or_else(|| format!("'{item}': expected FRAME:KEY"))?;
        let frame: u64 = frame
            .trim()
            .parse()
            .map_err(|e| format!("'{item}': frame: {e}"))?;
        let key = match key.trim().to_ascii_lowercase().as_str() {
            "space" => Key::Named(NamedKey::Space),
            "right" => Key::Named(NamedKey::ArrowRight),
            "left" => Key::Named(NamedKey::ArrowLeft),
            "esc" | "escape" => Key::Named(NamedKey::Escape),
            "f11" => Key::Named(NamedKey::F11),
            "f8" => Key::Named(NamedKey::F8),
            k if k.chars().count() == 1 => Key::Character(k.into()),
            k => return Err(format!("'{item}': unknown key '{k}'")),
        };
        out.push((frame, key));
    }
    out.sort_by_key(|(f, _)| *f);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    #[test]
    fn bindings_match_the_documented_keys() {
        use Action::*;
        let cases = [
            (Key::Named(NamedKey::Escape), Quit),
            (ch("q"), Quit),
            (ch("Q"), Quit),
            (Key::Named(NamedKey::ArrowRight), Navigate(Nav::Next)),
            (Key::Named(NamedKey::Space), Navigate(Nav::Next)),
            (ch("n"), Navigate(Nav::Next)),
            (ch("N"), Navigate(Nav::Next)),
            (Key::Named(NamedKey::ArrowLeft), Navigate(Nav::Prev)),
            (ch("p"), Navigate(Nav::Prev)),
            (ch("r"), Navigate(Nav::Random)),
            (ch("l"), ToggleLock),
            (ch("f"), ToggleFullscreen),
            (ch("d"), DumpStages),
            (Key::Named(NamedKey::F11), ToggleFullscreen),
            (Key::Named(NamedKey::F8), CycleAutoHardCut),
        ];
        for (k, want) in cases {
            assert_eq!(action_for_key(&k), Some(want), "{k:?}");
        }
        assert_eq!(action_for_key(&ch("x")), None);
        assert_eq!(action_for_key(&Key::Named(NamedKey::Enter)), None);
    }

    #[test]
    fn script_parsing() {
        let s = parse_script("60:p, 30:n,90:space").unwrap();
        assert_eq!(s.iter().map(|(f, _)| *f).collect::<Vec<_>>(), [30, 60, 90]);
        assert_eq!(action_for_key(&s[0].1), Some(Action::Navigate(Nav::Next)));
        assert_eq!(action_for_key(&s[1].1), Some(Action::Navigate(Nav::Prev)));
        assert_eq!(action_for_key(&s[2].1), Some(Action::Navigate(Nav::Next)));
        assert!(parse_script("x").is_err());
        assert!(parse_script("1:foo").is_err());
        assert_eq!(parse_script("5:f8").unwrap()[0].1, Key::Named(NamedKey::F8));
        assert!(parse_script("").unwrap().is_empty());
    }
}
