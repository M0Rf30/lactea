//! Preset playlist: the behaviour of projectM's playlist library (`projectm-4-playlist`:
//! `projectm_playlist_*`) as pure data, so it works in any host and on wasm.
//!
//! What it has: an ordered list of preset names/paths with duplicate control, a `.gitignore`-like
//! glob [`Filter`], shuffle, a 1000-entry playback history, `set_position` / `play_next` /
//! `play_previous` / `play_last`, sorting, and the retry loop that skips presets that fail to load
//! (default 500 retries, failed items are removed from the list), with the *switched*, *switch failed*
//! and *preset load* events.
//!
//! What it does not do: touch the filesystem or the engine. Scanning directories (`add_path`) is the
//! host's job (`lactea_host::PlaylistFs::add_path` / `insert_path`, or feed any list to
//! [`Playlist::add_presets`]); loading is a
//! closure the host passes to the `play_*` methods ([`Playlist::play_next`] ...), which either loads
//! the preset (`Ok`) or reports why it could not (`Err(message)`); that is the equivalent of
//! `projectm_playlist_set_preset_load_event_callback` plus projectM's failure event.
//! Shuffle is seeded (determinism contract): same seed, same sequence.
//!
//! The index conventions are projectM's: operations on an empty list return 0 / `None`.

use crate::rng::StreamRng;
use std::collections::{HashSet, VecDeque};

/// Maximum entries of the playback history (`Playlist::MaxHistoryItems`).
pub const MAX_HISTORY: usize = 1000;
/// Default number of failed loads in a row before the failure event is raised.
pub const DEFAULT_RETRY_COUNT: u32 = 500;
/// Pass as `index` to insert at the end (`Playlist::InsertAtEnd`).
pub const INSERT_AT_END: u32 = u32::MAX;

/// What [`Playlist::sort`] compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortPredicate {
    /// The whole string.
    FullPath,
    /// Only the part after the last `/` or `\`.
    FilenameOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Sort direction for [`Playlist::sort`].
pub enum SortOrder {
    /// Smallest first.
    Ascending,
    /// Largest first.
    Descending,
}

/// How the last navigation call moved (decides how a failed preset is replaced).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Next,
    Previous,
    Last,
}

// ---------------------------------------------------------------------------------------------
// filter
// ---------------------------------------------------------------------------------------------

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok {
    Lit(char),
    /// `?`: one character except a separator.
    Any,
    /// `*`: zero or more characters except separators.
    Star,
    /// `**/`: zero or more whole directories.
    DirGlob,
    /// `**` at the end: everything that is left.
    Rest,
    /// `**` followed by anything but a separator: matches nothing.
    Never,
}

fn parse_glob(pat: &str) -> Vec<Tok> {
    let c: Vec<char> = pat.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '?' => out.push(Tok::Any),
            '*' if c.get(i + 1) == Some(&'*') => {
                i += 1;
                match c.get(i + 1) {
                    None => {
                        out.push(Tok::Rest);
                        break;
                    }
                    Some(&n) if is_sep(n) => {
                        out.push(Tok::DirGlob);
                        i += 1;
                    }
                    Some(_) => {
                        out.push(Tok::Never);
                        break;
                    }
                }
            }
            '*' => out.push(Tok::Star),
            ch => out.push(Tok::Lit(ch)),
        }
        i += 1;
    }
    out
}

/// Full match of `toks` against `text` (memoised on failures, so patterns with many stars stay
/// polynomial).
fn glob_match(toks: &[Tok], text: &[char]) -> bool {
    fn go(
        toks: &[Tok],
        pi: usize,
        text: &[char],
        ti: usize,
        dead: &mut HashSet<(usize, usize)>,
    ) -> bool {
        if pi == toks.len() {
            return ti == text.len();
        }
        if dead.contains(&(pi, ti)) {
            return false;
        }
        let ok = match toks[pi] {
            Tok::Lit(c) => {
                ti < text.len()
                    && (text[ti] == c || (is_sep(c) && is_sep(text[ti])))
                    && go(toks, pi + 1, text, ti + 1, dead)
            }
            Tok::Any => {
                ti < text.len() && !is_sep(text[ti]) && go(toks, pi + 1, text, ti + 1, dead)
            }
            Tok::Star => {
                let mut k = ti;
                loop {
                    if go(toks, pi + 1, text, k, dead) {
                        break true;
                    }
                    if k < text.len() && !is_sep(text[k]) {
                        k += 1;
                    } else {
                        break false;
                    }
                }
            }
            Tok::DirGlob => {
                // zero directories, or everything up to and including a separator
                go(toks, pi + 1, text, ti, dead)
                    || (ti..text.len())
                        .any(|k| is_sep(text[k]) && go(toks, pi + 1, text, k + 1, dead))
            }
            Tok::Rest => true,
            Tok::Never => false,
        };
        if !ok {
            dead.insert((pi, ti));
        }
        ok
    }
    go(toks, 0, text, 0, &mut HashSet::new())
}

/// The playlist's include/exclude list (`projectm_playlist_set_filter`).
///
/// Each entry is a glob (`?` one char but `/`, `*` any run but `/`, `**/` any directories, `/**` at
/// the end everything below; `\` is a path separator too, there is no escaping) optionally prefixed
/// with `+` (include) or `-` (exclude; the default). Entries are checked in order; the first that
/// matches decides; nothing matching means included. A pattern without a separator is matched against
/// the file name only; one starting with a separator against the path with its leading `./` and
/// separators removed; others against the whole string. Empty entries match nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    list: Vec<String>,
}

impl Filter {
    /// Replaces the filter expressions (`+pattern` keeps, `-pattern` drops).
    pub fn set_list(&mut self, list: Vec<String>) {
        self.list = list;
    }

    /// The filter expressions in order.
    pub fn list(&self) -> &[String] {
        &self.list
    }

    /// Does `expr` (with its `+`/`-` prefix) match `filename`?
    pub fn expression_matches(filename: &str, expr: &str) -> bool {
        if filename.is_empty() || expr.is_empty() {
            return false;
        }
        let pat = expr.strip_prefix(['+', '-']).unwrap_or(expr);
        let name: Vec<char> = filename.chars().collect();
        let (pat, text): (&str, &[char]) = if pat.starts_with(is_sep) {
            let mut s = 0;
            while name.get(s) == Some(&'.') && name.get(s + 1).is_some_and(|&c| is_sep(c)) {
                s += 2;
            }
            while name.get(s).is_some_and(|&c| is_sep(c)) {
                s += 1;
            }
            (&pat[1..], &name[s..])
        } else if !pat.contains(is_sep) {
            let s = name.iter().rposition(|&c| is_sep(c)).map_or(0, |p| p + 1);
            (pat, &name[s..])
        } else {
            (pat, &name[..])
        };
        glob_match(&parse_glob(pat), text)
    }

    /// `true` if `filename` is kept.
    pub fn passes(&self, filename: &str) -> bool {
        for expr in &self.list {
            if !expr.is_empty() && Self::expression_matches(filename, expr) {
                return expr.starts_with('+');
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------------------------
// playlist
// ---------------------------------------------------------------------------------------------

/// What the host's loader is asked to do (`projectm_playlist_preset_load_event`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadRequest<'a> {
    /// Playlist index of the item being loaded.
    pub index: u32,
    /// The item's preset name or path.
    pub filename: &'a str,
    /// `false`: the host should blend to the new preset (soft cut).
    pub hard_cut: bool,
}

/// Callback of [`Playlist::set_preset_switched_callback`]: `(is_hard_cut, index)`.
pub type SwitchedFn = Box<dyn FnMut(bool, u32)>;
/// Callback of [`Playlist::set_preset_switch_failed_callback`]: `(filename, message)`.
pub type FailedFn = Box<dyn FnMut(&str, &str)>;

/// An ordered preset list with projectM's navigation rules. See the module docs.
pub struct Playlist {
    items: Vec<String>,
    position: u32,
    history: VecDeque<u32>,
    shuffle: bool,
    filter: Filter,
    retry_count: u32,
    direction: Direction,
    rng: StreamRng,
    on_switched: Option<SwitchedFn>,
    on_failed: Option<FailedFn>,
}

impl Playlist {
    /// Empty playlist; `seed` drives shuffle.
    pub fn new(seed: u64) -> Self {
        Self {
            items: Vec::new(),
            position: 0,
            history: VecDeque::new(),
            shuffle: false,
            filter: Filter::default(),
            retry_count: DEFAULT_RETRY_COUNT,
            direction: Direction::Next,
            rng: StreamRng::new(seed, "playlist_shuffle"),
            on_switched: None,
            on_failed: None,
        }
    }

    // ---- items ----------------------------------------------------------------------------

    /// Number of items.
    pub fn len(&self) -> u32 {
        self.items.len() as u32
    }

    /// Whether the playlist has no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Removes every item and the history.
    pub fn clear(&mut self) {
        self.history.clear();
        self.items.clear();
    }

    /// All items in order.
    pub fn items(&self) -> &[String] {
        &self.items
    }

    /// `projectm_playlist_items`: items `start..start+count` (clamped).
    pub fn items_range(&self, start: u32, count: u32) -> &[String] {
        let s = (start as usize).min(self.items.len());
        let e = (start as usize)
            .saturating_add(count as usize)
            .min(self.items.len());
        &self.items[s..e]
    }

    /// The item at `index`, if in range.
    pub fn item(&self, index: u32) -> Option<&str> {
        self.items.get(index as usize).map(String::as_str)
    }

    /// `projectm_playlist_insert_preset` (use [`INSERT_AT_END`] to append). Rejected when empty,
    /// filtered out, or a duplicate with `allow_duplicates == false`. History indices at or after
    /// `index` shift by one.
    pub fn insert_preset(&mut self, filename: &str, index: u32, allow_duplicates: bool) -> bool {
        if filename.is_empty() || !self.filter.passes(filename) {
            return false;
        }
        if !allow_duplicates && self.items.iter().any(|i| i == filename) {
            return false;
        }
        if index as usize >= self.items.len() {
            self.items.push(filename.to_string());
        } else {
            for h in &mut self.history {
                if *h >= index {
                    *h += 1;
                }
            }
            self.items.insert(index as usize, filename.to_string());
        }
        true
    }

    /// Appends `filename` if it passes the filter; returns whether it was added.
    pub fn add_preset(&mut self, filename: &str, allow_duplicates: bool) -> bool {
        self.insert_preset(filename, INSERT_AT_END, allow_duplicates)
    }

    /// Appends the presets that pass; returns how many were added.
    pub fn add_presets<S: AsRef<str>>(&mut self, filenames: &[S], allow_duplicates: bool) -> u32 {
        filenames
            .iter()
            .filter(|f| self.add_preset(f.as_ref(), allow_duplicates))
            .count() as u32
    }

    /// `projectm_playlist_insert_presets`: keeps the given order starting at `index`.
    pub fn insert_presets<S: AsRef<str>>(
        &mut self,
        filenames: &[S],
        index: u32,
        allow_duplicates: bool,
    ) -> u32 {
        let mut added = 0u32;
        for f in filenames {
            let at = if index == INSERT_AT_END {
                INSERT_AT_END
            } else {
                index.saturating_add(added)
            };
            if self.insert_preset(f.as_ref(), at, allow_duplicates) {
                added += 1;
            }
        }
        added
    }

    /// Removes one item; history entries for it disappear, later ones shift down.
    pub fn remove_preset(&mut self, index: u32) -> bool {
        if index as usize >= self.items.len() {
            return false;
        }
        self.history.retain(|&h| h != index);
        for h in &mut self.history {
            if *h > index {
                *h -= 1;
            }
        }
        self.items.remove(index as usize);
        true
    }

    /// Removes up to `count` items starting at `index`; returns how many were removed.
    pub fn remove_presets(&mut self, index: u32, count: u32) -> u32 {
        let mut removed = 0;
        for _ in 0..count {
            if !self.remove_preset(index) {
                break;
            }
            removed += 1;
        }
        removed
    }

    /// Sorts `count` items from `start` (clamped); clears the history.
    pub fn sort(&mut self, start: u32, count: u32, predicate: SortPredicate, order: SortOrder) {
        let start = start as usize;
        if start >= self.items.len() {
            return;
        }
        let end = start.saturating_add(count as usize).min(self.items.len());
        self.history.clear();
        let key = |s: &String| -> String {
            match predicate {
                SortPredicate::FullPath => s.clone(),
                SortPredicate::FilenameOnly => {
                    s.rsplit(['/', '\\']).next().unwrap_or("").to_string()
                }
            }
        };
        self.items[start..end].sort_by(|a, b| match order {
            SortOrder::Ascending => key(a).cmp(&key(b)),
            SortOrder::Descending => key(b).cmp(&key(a)),
        });
    }

    // ---- filter ---------------------------------------------------------------------------

    /// Sets the filter list; only affects items added afterwards (see [`Playlist::apply_filter`]).
    pub fn set_filter(&mut self, list: Vec<String>) {
        self.filter.set_list(list);
    }

    /// The filter expressions in order.
    pub fn filter(&self) -> &[String] {
        self.filter.list()
    }

    /// Removes the items the current filter rejects (items filtered out earlier do not come back).
    /// Clears the history if anything was removed. Returns the number removed.
    pub fn apply_filter(&mut self) -> u32 {
        let before = self.items.len();
        let f = &self.filter;
        self.items.retain(|i| f.passes(i));
        let removed = (before - self.items.len()) as u32;
        if removed != 0 {
            self.history.clear();
        }
        removed
    }

    // ---- playback settings ----------------------------------------------------------------

    /// Enables or disables shuffled navigation.
    pub fn set_shuffle(&mut self, shuffle: bool) {
        self.shuffle = shuffle;
    }

    /// Whether shuffle is on.
    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// Failed loads in a row after which [`Playlist::play_next`] ... give up and raise the failure
    /// event; `0` raises it on the first failure. Default [`DEFAULT_RETRY_COUNT`].
    pub fn set_retry_count(&mut self, retries: u32) {
        self.retry_count = retries;
    }

    /// Failed loads in a row tolerated before the failure event.
    pub fn retry_count(&self) -> u32 {
        self.retry_count
    }

    /// `projectm_playlist_set_preset_switched_event_callback`: `(is_hard_cut, index)`.
    pub fn set_preset_switched_callback(&mut self, cb: Option<SwitchedFn>) {
        self.on_switched = cb;
    }

    /// `projectm_playlist_set_preset_switch_failed_event_callback`: `(filename, message)`, raised when
    /// the retries are exhausted.
    pub fn set_preset_switch_failed_callback(&mut self, cb: Option<FailedFn>) {
        self.on_failed = cb;
    }

    // ---- navigation -----------------------------------------------------------------------

    /// Current position (0 for an empty list).
    pub fn position(&self) -> u32 {
        if self.items.is_empty() {
            0
        } else {
            self.position
        }
    }

    /// Played positions, oldest first (at most [`MAX_HISTORY`]).
    pub fn history(&self) -> Vec<u32> {
        self.history.iter().copied().collect()
    }

    fn push_history(&mut self) {
        if self.history.back() == Some(&self.position) {
            return;
        }
        self.history.push_back(self.position);
        if self.history.len() > MAX_HISTORY {
            self.history.pop_front();
        }
    }

    fn random_index(&mut self) -> u32 {
        (self.rng.next64() % self.items.len() as u64) as u32
    }

    /// Moves to the next item (random with shuffle, else wrapping) and returns it; the old
    /// position goes to the history. `None` when empty.
    pub fn next_index(&mut self) -> Option<u32> {
        if self.items.is_empty() {
            return None;
        }
        self.push_history();
        self.position = if self.shuffle {
            self.random_index()
        } else if self.position as usize + 1 >= self.items.len() {
            0
        } else {
            self.position + 1
        };
        Some(self.position)
    }

    /// Like [`Playlist::next_index`] backwards (random with shuffle).
    pub fn previous_index(&mut self) -> Option<u32> {
        if self.items.is_empty() {
            return None;
        }
        self.push_history();
        self.position = if self.shuffle {
            self.random_index()
        } else if self.position == 0 {
            self.items.len() as u32 - 1
        } else {
            self.position - 1
        };
        Some(self.position)
    }

    /// The position played before this one (pops the history); with an empty history it behaves as
    /// [`Playlist::previous_index`] without recording the step (no ping-pong).
    pub fn last_index(&mut self) -> Option<u32> {
        if self.items.is_empty() {
            return None;
        }
        if let Some(p) = self.history.pop_back() {
            self.position = p;
        } else {
            self.position = self.previous_index()?;
            self.history.pop_back();
        }
        Some(self.position)
    }

    /// Jumps to `index` (ignores shuffle; out of range wraps to 0) and returns the new position.
    pub fn set_index(&mut self, index: u32) -> Option<u32> {
        if self.items.is_empty() {
            return None;
        }
        self.push_history();
        if index == self.position {
            return Some(self.position);
        }
        self.position = if index as usize >= self.items.len() {
            0
        } else {
            index
        };
        Some(self.position)
    }

    /// projectM's `PlayPresetIndex`: ask `load` for the preset at `index`; on `Err` drop the item
    /// from the list, pick the replacement the way the last navigation moved, and retry, up to the
    /// retry count; then raise the failure event. On success raises the switched event.
    fn play_index<L>(&mut self, mut index: u32, hard_cut: bool, load: &mut L)
    where
        L: FnMut(LoadRequest<'_>) -> Result<(), String>,
    {
        let mut failed = 0u32;
        loop {
            let Some(name) = self.items.get(index as usize).cloned() else {
                return;
            };
            match load(LoadRequest {
                index,
                filename: &name,
                hard_cut,
            }) {
                Ok(()) => break,
                Err(message) => {
                    // the failure event of projectM: never let "back" land on a broken preset
                    if self.direction != Direction::Last {
                        self.history.pop_back();
                    }
                    failed += 1;
                    if failed >= self.retry_count {
                        if let Some(cb) = &mut self.on_failed {
                            cb(&name, &message);
                        }
                        return;
                    }
                    self.remove_preset(index);
                    if self.items.is_empty() {
                        return;
                    }
                    let next = match self.direction {
                        Direction::Last => self.last_index(),
                        Direction::Next => self.next_index(),
                        Direction::Previous => self.previous_index(),
                    };
                    match next {
                        Some(n) => index = n,
                        None => return,
                    }
                }
            }
        }
        if let Some(cb) = &mut self.on_switched {
            cb(hard_cut, index);
        }
    }

    /// `projectm_playlist_set_position`: play `index` and return the position afterwards.
    pub fn set_position<L>(&mut self, index: u32, hard_cut: bool, mut load: L) -> u32
    where
        L: FnMut(LoadRequest<'_>) -> Result<(), String>,
    {
        self.direction = Direction::Next;
        if let Some(i) = self.set_index(index) {
            self.play_index(i, hard_cut, &mut load);
        }
        self.position()
    }

    /// `projectm_playlist_play_next`; returns the position afterwards (0 when empty).
    pub fn play_next<L>(&mut self, hard_cut: bool, mut load: L) -> u32
    where
        L: FnMut(LoadRequest<'_>) -> Result<(), String>,
    {
        self.direction = Direction::Next;
        if let Some(i) = self.next_index() {
            self.play_index(i, hard_cut, &mut load);
        }
        self.position()
    }

    /// `projectm_playlist_play_previous`.
    pub fn play_previous<L>(&mut self, hard_cut: bool, mut load: L) -> u32
    where
        L: FnMut(LoadRequest<'_>) -> Result<(), String>,
    {
        self.direction = Direction::Previous;
        if let Some(i) = self.previous_index() {
            self.play_index(i, hard_cut, &mut load);
        }
        self.position()
    }

    /// `projectm_playlist_play_last`: back through the history.
    pub fn play_last<L>(&mut self, hard_cut: bool, mut load: L) -> u32
    where
        L: FnMut(LoadRequest<'_>) -> Result<(), String>,
    {
        self.direction = Direction::Last;
        if let Some(i) = self.last_index() {
            self.play_index(i, hard_cut, &mut load);
        }
        self.position()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn list(names: &[&str]) -> Playlist {
        let mut p = Playlist::new(1);
        assert_eq!(p.add_presets(names, false), names.len() as u32);
        p
    }

    fn ok(_: LoadRequest<'_>) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn glob_filter_semantics() {
        let m = |name: &str, expr: &str| Filter::expression_matches(name, expr);
        // no separator in the pattern: file name only; * and ? stop at separators
        assert!(m("/p/a/Foo.milk", "Foo.milk"));
        assert!(m("/p/a/Foo.milk", "*.milk"));
        assert!(m("/p/a/Foo.milk", "F?o.*"));
        assert!(!m("/p/a/Foo.milk", "Fo.milk"));
        assert!(!m("/p/a/Foo.milk", "*.prjm"));
        // with a separator the whole string is matched, `*` does not cross `/`
        assert!(m("a/b/c.milk", "a/b/*.milk"));
        assert!(!m("a/b/c.milk", "a/*.milk"));
        assert!(!m("a/b/c.milk", "b/*.milk"));
        // `**/` = any directories (also none), `/**` = everything below
        assert!(m("a/b/c.milk", "a/**/c.milk"));
        assert!(m("a/c.milk", "a/**/c.milk"));
        assert!(m("a/b/c/d.milk", "a/**/d.milk"));
        assert!(!m("a/b/c/d.milk", "a/**/x.milk"));
        assert!(m("a/b/c/d.milk", "a/**"));
        assert!(!m("x/b/c/d.milk", "a/**"));
        assert!(m("a/b/c.milk", "**/c.milk"));
        // `\` is a separator as well
        assert!(m("a\\b\\c.milk", "a/b/*.milk"));
        assert!(m("a/b/c.milk", "a\\b\\*.milk"));
        // a leading separator anchors at the root of the path (leading ./ and / are ignored)
        assert!(m("./Foo.milk", "/Foo.milk"));
        assert!(m("Foo.milk", "/Foo.milk"));
        assert!(!m("dir/Foo.milk", "/Foo.milk"));
        assert!(m("dir/Foo.milk", "/dir/*.milk"));
        // prefixes are stripped before matching; empty things match nothing
        assert!(m("a/b.milk", "+b.milk") && m("a/b.milk", "-b.milk"));
        assert!(!m("", "*") && !m("a", "") && !m("a", "+"));
        // `**` followed by a non-separator never matches
        assert!(!m("abc", "a**c"));
        // pathological stars stay fast
        assert!(!m(&"a".repeat(60), &format!("{}b", "*a".repeat(30))));
    }

    #[test]
    fn filter_list_first_match_decides() {
        let mut p = Playlist::new(1);
        p.set_filter(vec!["+good/**".into(), "-*bad*".into(), "-/**".into()]);
        assert!(p.add_preset("good/nice.milk", false));
        assert!(!p.add_preset("good2/nice.milk", false)); // "-/**" excludes everything else
        assert!(!p.add_preset("other/bad one.milk", false));
        assert!(p.add_preset("good/bad.milk", false)); // "+good/**" is first
        assert_eq!(p.len(), 2);
        // with only an exclude list, everything that does not match is kept
        p.set_filter(vec!["-*.prjm".into()]);
        assert!(p.add_preset("x.milk", false) && !p.add_preset("y.prjm", false));
        // apply_filter removes retroactively, clears the history, and reports the count
        p.set_filter(vec!["-x.milk".into(), "-good/bad.milk".into()]);
        p.next_index();
        assert!(!p.history().is_empty());
        assert_eq!(p.apply_filter(), 2);
        assert_eq!(p.items(), &["good/nice.milk".to_string()]);
        assert!(p.history().is_empty());
        assert_eq!(
            p.filter(),
            &["-x.milk".to_string(), "-good/bad.milk".to_string()]
        );
    }

    #[test]
    fn add_insert_remove_keep_history_consistent() {
        let mut p = list(&["a", "b", "c", "d"]);
        assert!(!p.add_preset("a", false) && p.add_preset("a", true));
        assert!(!p.add_preset("", true));
        assert_eq!(p.len(), 5);
        assert_eq!(p.remove_presets(4, 5), 1);
        // history: visit b, c, d
        for _ in 0..3 {
            p.next_index();
        }
        assert_eq!(p.history(), vec![0, 1, 2]);
        p.insert_preset("x", 1, false);
        assert_eq!(p.history(), vec![0, 2, 3]); // entries >= 1 shifted
        assert_eq!(p.items(), &["a", "x", "b", "c", "d"].map(String::from));
        assert!(p.remove_preset(2)); // "b"
        assert_eq!(p.history(), vec![0, 2]); // entry 2 gone, 3 -> 2
        assert_eq!(p.insert_presets(&["m", "n", "o"], 1, false), 3);
        assert_eq!(p.items_range(1, 3), &["m", "n", "o"].map(String::from));
        assert_eq!(p.items_range(40, 3), &[] as &[String]);
        assert_eq!(p.item(0), Some("a"));
        assert_eq!(p.item(99), None);
    }

    #[test]
    fn next_previous_wrap_and_history_deduplicates() {
        let mut p = list(&["a", "b", "c"]);
        assert_eq!(p.position(), 0);
        assert_eq!(p.next_index(), Some(1));
        assert_eq!(p.next_index(), Some(2));
        assert_eq!(p.next_index(), Some(0)); // wraps
        assert_eq!(p.previous_index(), Some(2)); // wraps backwards
        assert_eq!(p.history(), vec![0, 1, 2, 0]);
        // set_index records the old position, jumps, and out of range wraps to 0
        assert_eq!(p.set_index(1), Some(1));
        assert_eq!(p.set_index(77), Some(0));
        // the same position twice in a row is recorded once
        let mut q = list(&["a", "b"]);
        assert_eq!(q.set_index(0), Some(0));
        assert_eq!(q.set_index(0), Some(0));
        assert_eq!(q.history(), vec![0]);
    }

    #[test]
    fn last_walks_back_and_never_ping_pongs() {
        let mut p = list(&["a", "b", "c", "d"]);
        for _ in 0..3 {
            p.next_index();
        }
        assert_eq!(p.position(), 3);
        assert_eq!(p.last_index(), Some(2));
        assert_eq!(p.last_index(), Some(1));
        assert_eq!(p.last_index(), Some(0));
        // history exhausted: acts as "previous" but is not recorded
        assert_eq!(p.last_index(), Some(3));
        assert!(p.history().is_empty());
        assert_eq!(p.last_index(), Some(2));
    }

    #[test]
    fn history_is_capped() {
        let mut p = list(&["a", "b"]);
        for _ in 0..(MAX_HISTORY + 50) {
            p.next_index();
        }
        assert_eq!(p.history().len(), MAX_HISTORY);
    }

    #[test]
    fn shuffle_is_seeded_covers_the_list_and_set_position_ignores_it() {
        let names: Vec<String> = (0..10).map(|i| format!("p{i}")).collect();
        let run = |seed: u64| {
            let mut p = Playlist::new(seed);
            p.add_presets(&names, false);
            p.set_shuffle(true);
            assert!(p.shuffle());
            (0..200)
                .map(|_| p.next_index().unwrap())
                .collect::<Vec<_>>()
        };
        let (a, b, c) = (run(5), run(5), run(6));
        assert_eq!(a, b, "same seed, same shuffle");
        assert_ne!(a, c);
        assert!(a.iter().all(|&i| i < 10));
        assert_eq!(
            a.iter().copied().collect::<HashSet<_>>().len(),
            10,
            "every item shows up"
        );
        let mut p = Playlist::new(5);
        p.add_presets(&names, false);
        p.set_shuffle(true);
        assert_eq!(p.set_position(7, true, ok), 7);
        assert_eq!(p.position(), 7);
    }

    #[test]
    fn empty_playlist_answers_zero() {
        let mut p = Playlist::new(1);
        assert_eq!(
            (
                p.next_index(),
                p.previous_index(),
                p.last_index(),
                p.set_index(3)
            ),
            (None, None, None, None)
        );
        let calls = Rc::new(RefCell::new(0));
        let c = calls.clone();
        assert_eq!(
            p.play_next(false, move |_| {
                *c.borrow_mut() += 1;
                Ok(())
            }),
            0
        );
        assert_eq!(*calls.borrow(), 0);
        assert_eq!(p.position(), 0);
        assert_eq!(p.apply_filter(), 0);
    }

    #[test]
    fn sort_by_path_or_filename_both_orders() {
        let mut p = list(&["z/a.milk", "a/z.milk", "m/m.milk", "B/b.milk"]);
        p.next_index();
        p.sort(0, 99, SortPredicate::FullPath, SortOrder::Ascending);
        assert_eq!(
            p.items(),
            &["B/b.milk", "a/z.milk", "m/m.milk", "z/a.milk"].map(String::from)
        );
        assert!(p.history().is_empty());
        p.sort(0, 4, SortPredicate::FilenameOnly, SortOrder::Ascending);
        assert_eq!(
            p.items(),
            &["z/a.milk", "B/b.milk", "m/m.milk", "a/z.milk"].map(String::from)
        );
        p.sort(1, 2, SortPredicate::FilenameOnly, SortOrder::Descending);
        assert_eq!(
            p.items(),
            &["z/a.milk", "m/m.milk", "B/b.milk", "a/z.milk"].map(String::from)
        );
        p.sort(9, 2, SortPredicate::FullPath, SortOrder::Ascending); // out of range: no-op
        assert_eq!(p.len(), 4);
    }

    #[test]
    fn play_next_loads_and_fires_the_switched_event() {
        let mut p = list(&["a", "b", "c"]);
        let events = Rc::new(RefCell::new(Vec::new()));
        let e = events.clone();
        p.set_preset_switched_callback(Some(Box::new(move |hard, idx| {
            e.borrow_mut().push((hard, idx))
        })));
        let loaded = Rc::new(RefCell::new(Vec::new()));
        let l = loaded.clone();
        let pos = p.play_next(true, move |r| {
            l.borrow_mut()
                .push((r.index, r.filename.to_string(), r.hard_cut));
            Ok(())
        });
        assert_eq!(pos, 1);
        assert_eq!(*loaded.borrow(), vec![(1, "b".to_string(), true)]);
        assert_eq!(*events.borrow(), vec![(true, 1)]);
        assert_eq!(p.play_previous(false, ok), 0);
        assert_eq!(*events.borrow(), vec![(true, 1), (false, 0)]);
        assert_eq!(p.play_last(false, ok), 1); // history: [0, 1] -> back to 1
    }

    #[test]
    fn failed_presets_are_skipped_and_removed() {
        // projectM quirk kept on purpose: after the failed item is removed the walk continues from the
        // *same position*, so the item that slides into the freed slot ("c") is skipped.
        let mut p = list(&["a", "bad1", "c", "d"]);
        let failures = Rc::new(RefCell::new(Vec::new()));
        let f = failures.clone();
        p.set_preset_switch_failed_callback(Some(Box::new(move |n, m| {
            f.borrow_mut().push((n.to_string(), m.to_string()))
        })));
        let switched = Rc::new(RefCell::new(Vec::new()));
        let s = switched.clone();
        p.set_preset_switched_callback(Some(Box::new(move |h, i| s.borrow_mut().push((h, i)))));
        let tried = Rc::new(RefCell::new(Vec::new()));
        let t = tried.clone();
        let pos = p.play_next(false, move |r| {
            t.borrow_mut().push(r.filename.to_string());
            if r.filename.starts_with("bad") {
                Err(format!("{} is broken", r.filename))
            } else {
                Ok(())
            }
        });
        assert_eq!(*tried.borrow(), vec!["bad1", "d"]);
        assert_eq!(p.items(), &["a", "c", "d"].map(String::from)); // the broken one is gone
        assert_eq!(*switched.borrow(), vec![(false, pos)]);
        assert_eq!(p.item(pos), Some("d"));
        assert!(failures.borrow().is_empty());
        // the broken presets are not in the history (going "last" cannot land on them)
        assert!(!p.history().iter().any(|&h| p.item(h).is_none()));
    }

    #[test]
    fn retry_count_limits_the_attempts_and_raises_the_failure_event() {
        let mut p = list(&["a", "b", "c", "d", "e"]);
        p.set_retry_count(2);
        assert_eq!(p.retry_count(), 2);
        let failures = Rc::new(RefCell::new(Vec::new()));
        let f = failures.clone();
        p.set_preset_switch_failed_callback(Some(Box::new(move |n, m| {
            f.borrow_mut().push((n.to_string(), m.to_string()))
        })));
        let switched = Rc::new(RefCell::new(0));
        let s = switched.clone();
        p.set_preset_switched_callback(Some(Box::new(move |_, _| *s.borrow_mut() += 1)));
        let attempts = Rc::new(RefCell::new(0));
        let a = attempts.clone();
        p.play_next(true, move |_| {
            *a.borrow_mut() += 1;
            Err("nope".into())
        });
        assert_eq!(*attempts.borrow(), 2, "two failed loads, then it gives up");
        assert_eq!(failures.borrow().len(), 1);
        assert_eq!(failures.borrow()[0].1, "nope");
        assert_eq!(*switched.borrow(), 0);
        // retry count 0: the very first failure is forwarded and nothing is removed
        let mut q = list(&["a", "b"]);
        q.set_retry_count(0);
        let fired = Rc::new(RefCell::new(0));
        let fi = fired.clone();
        q.set_preset_switch_failed_callback(Some(Box::new(move |_, _| *fi.borrow_mut() += 1)));
        q.play_next(true, |_| Err("x".into()));
        assert_eq!((*fired.borrow(), q.len()), (1, 2));
        // everything broken and a big retry count: the list empties, no event, no panic
        let mut r = list(&["a", "b", "c"]);
        r.play_next(true, |_| Err("x".into()));
        assert!(r.is_empty());
        assert_eq!(r.position(), 0);
    }

    #[test]
    fn failed_preset_while_going_back_uses_the_previous_direction() {
        let mut p = list(&["a", "b", "bad", "d"]);
        p.set_index(3);
        // going back from "d" lands on "bad": it is removed and the walk continues backwards
        let pos = p.play_previous(true, |r| {
            if r.filename == "bad" {
                Err("x".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(p.items(), &["a", "b", "d"].map(String::from));
        assert_eq!(p.item(pos), Some("b"));
    }
}
