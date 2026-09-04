//! HTML § 7.1 session history + `history.pushState` / `replaceState` — the
//! session-history entry-stack model the `History` host hook + the
//! navigation layer reduce to (Phase 6 prep). Pure given the entry tuple;
//! the document load / unload the navigation layer runs for each traversal
//! is the host hook.
//!
//! What lives here:
//! - [`ScrollRestoration`] — § 7.1 `history.scrollRestoration` (`auto` /
//!   `manual`).
//! - [`HistoryEntry`] — one session-history entry: the URL string + the
//!   opaque `state` blob (the structured-clone-serialised `pushState`
//!   state) + the `scrollRestoration` mode + the optional title.
//! - [`SessionHistory`] — the entry stack + the current-entry cursor +
//!   the § 7.1 `pushState` / `replaceState` / `back` / `forward` / `go`
//!   surface.
//!
//! What does *not* live here:
//! - The document load / unload for a traversal — the navigation layer
//!   runs the § 7.5 "traverse the history" algorithm (fetch the entry's
//!   URL, swap documents, fire `pageshow`/`pagehide`); this module is the
//!   pure entry-stack + cursor.
//! - The same-origin URL check for `pushState` / `replaceState` — the
//!   host hook enforces § 7.1's "the new URL must be same-origin as the
//!   document's URL" before constructing the entry; the pure model
//!   accepts any URL string.
//! - The structured-clone serialisation of the `state` value — the host
//!   hook serialises the JS value via [`crate::structured_clone`] before
//!   calling `pushState`; this module carries the opaque blob (so the
//!   state round-trips byte-for-byte across navigations).
//! - The cross-document navigation history (a session history may mix
//!   same-document `pushState` entries + cross-document navigations) —
//!   v1.0 models the same-document entry stack; the cross-document
//!   navigation entries land with the navigation layer.
//!
//! ## The cursor invariant
//!
//! A [`SessionHistory`] always has `≥ 1` entry (the initial entry) and
//! `cursor < entries.len()`. [`SessionHistory::push`] truncates every
//! entry after the current one (the § 7.1 "remove all the entries after
//! the current entry" rule) before appending, so a `back` + `push`
//! sequence drops the forward branch — matching every browser's
//! behaviour.
//!
//! Reference: <https://html.spec.whatwg.org/multipage/history.html>.

#![forbid(unsafe_code)]

// ---------------------------------------------------------------------------
// ScrollRestoration + HistoryEntry
// ---------------------------------------------------------------------------

/// HTML § 7.1 `history.scrollRestoration` — whether the user agent restores
/// the scroll position on traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScrollRestoration {
    /// `auto` (the default) — restore the persisted scroll position on
    /// traversal.
    #[default]
    Auto,
    /// `manual` — do not restore; the page handles scroll on `pageshow`.
    Manual,
}

impl ScrollRestoration {
    /// Parse the `scrollRestoration` keyword (ASCII-case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }

    /// The serialised form.
    pub fn to_keyword(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

/// One session-history entry (HTML § 7.1). The `state` is the opaque
/// structured-clone blob the host hook serialises the `pushState` value
/// into (so it round-trips byte-for-byte across navigations).
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    /// The entry's URL string (the § 7.1 `url`; same-origin with the
    /// document — the host hook enforces).
    pub url: String,
    /// The opaque structured-clone state blob (`None` for entries created
    /// by a cross-document navigation; `Some` for `pushState`/`replaceState`
    /// entries).
    pub state: Option<Vec<u8>>,
    /// The `scrollRestoration` mode for this entry.
    pub scroll_restoration: ScrollRestoration,
    /// Browser-owned scroll state captured when this entry stops being current.
    pub scroll_state: Option<HistoryScrollState>,
    /// The optional document title (the § 7.1 title the host hook sets on
    /// `pushState`/`replaceState` for the history UI).
    pub title: Option<String>,
}

impl HistoryEntry {
    /// Construct an entry with a URL + state, defaulting `scrollRestoration`
    /// to `auto` + no title.
    pub fn new(url: impl Into<String>, state: Option<Vec<u8>>) -> Self {
        Self {
            url: url.into(),
            state,
            scroll_restoration: ScrollRestoration::Auto,
            scroll_state: None,
            title: None,
        }
    }

    /// Construct a same-document entry for `pushState` with a state blob.
    pub fn push_state(url: impl Into<String>, state: Vec<u8>) -> Self {
        Self {
            url: url.into(),
            state: Some(state),
            scroll_restoration: ScrollRestoration::Auto,
            scroll_state: None,
            title: None,
        }
    }

    /// Construct a cross-document navigation entry (no state blob).
    pub fn navigation(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            state: None,
            scroll_restoration: ScrollRestoration::Auto,
            scroll_state: None,
            title: None,
        }
    }
}

/// Bounded browser-owned scroll state associated with one history entry.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryScrollState {
    pub root_offset: (f32, f32),
    pub element_offsets: Vec<HistoryElementScroll>,
}

/// Stable identity and offset for one retained element scrollport.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryElementScroll {
    pub node_id: usize,
    pub element_id: Option<String>,
    pub tag: String,
    pub offset: (f32, f32),
}

// ---------------------------------------------------------------------------
// SessionHistory
// ---------------------------------------------------------------------------

/// The HTML § 7.1 session history: a stack of [`HistoryEntry`]s + a cursor
/// to the current entry. The § 7.1 `pushState` / `replaceState` /
/// `back` / `forward` / `go` surface the `History` host hook reflects.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionHistory {
    entries: Vec<HistoryEntry>,
    cursor: usize,
}

impl SessionHistory {
    /// Construct a session history with one initial entry (the § 7.1
    /// invariant: every browsing context has at least one entry).
    pub fn new(initial: HistoryEntry) -> Self {
        Self {
            entries: vec![initial],
            cursor: 0,
        }
    }

    /// The number of entries in the session history (`history.length`).
    pub fn length(&self) -> usize {
        self.entries.len()
    }

    /// The current-entry index (`history`'s internal cursor; `0`-based).
    pub fn index(&self) -> usize {
        self.cursor
    }

    /// `true` iff `back()` would move the cursor (there is an entry before
    /// the current one).
    pub fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    /// `true` iff `forward()` would move the cursor (there is an entry after
    /// the current one).
    pub fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// The current entry, or `None` if the history is empty (only possible
    /// if the caller constructed an empty history via the
    /// [`Self::with_entries`] escape hatch).
    pub fn current(&self) -> Option<&HistoryEntry> {
        self.entries.get(self.cursor)
    }

    /// The current entry's URL string (`history`'s document URL).
    pub fn url(&self) -> Option<&str> {
        self.current().map(|e| e.url.as_str())
    }

    /// The current entry's opaque state blob (`history.state`; `None` for a
    /// cross-document navigation entry, `Some([])` for a `pushState` with an
    /// empty state).
    pub fn state(&self) -> Option<&[u8]> {
        self.current().and_then(|e| e.state.as_deref())
    }

    /// The current entry's `scrollRestoration` mode.
    pub fn scroll_restoration(&self) -> ScrollRestoration {
        self.current()
            .map(|e| e.scroll_restoration)
            .unwrap_or_default()
    }

    /// Update the current entry's restoration policy.
    pub fn set_scroll_restoration(&mut self, value: ScrollRestoration) {
        if let Some(entry) = self.entries.get_mut(self.cursor) {
            entry.scroll_restoration = value;
        }
    }

    /// Capture current browser-owned scroll state without changing the cursor.
    pub fn set_current_scroll_state(&mut self, state: HistoryScrollState) {
        if let Some(entry) = self.entries.get_mut(self.cursor) {
            entry.scroll_state = Some(state);
        }
    }

    /// Scroll state the user agent may restore for the current entry.
    pub fn restoration_scroll_state(&self) -> Option<&HistoryScrollState> {
        self.current()
            .filter(|entry| entry.scroll_restoration == ScrollRestoration::Auto)
            .and_then(|entry| entry.scroll_state.as_ref())
    }

    /// `history.pushState(state, unused, url)` — truncate every entry after
    /// the current one (the § 7.1 rule), append `entry`, advance the
    /// cursor. Returns the new cursor index.
    pub fn push(&mut self, entry: HistoryEntry) -> usize {
        // Drop the forward branch (§ 7.1: "remove all the entries after the
        // current entry").
        self.entries.truncate(self.cursor + 1);
        self.entries.push(entry);
        self.cursor = self.entries.len() - 1;
        self.cursor
    }

    /// `history.replaceState(state, unused, url)` — replace the current
    /// entry with `entry`. The length + cursor are unchanged.
    pub fn replace(&mut self, entry: HistoryEntry) {
        if let Some(slot) = self.entries.get_mut(self.cursor) {
            *slot = entry;
        }
    }

    /// `history.back()` — move the cursor one entry toward the start.
    /// Returns the new current entry, or `None` if already at the start.
    pub fn back(&mut self) -> Option<&HistoryEntry> {
        if self.cursor == 0 {
            None
        } else {
            self.cursor -= 1;
            self.current()
        }
    }

    /// `history.forward()` — move the cursor one entry toward the end.
    /// Returns the new current entry, or `None` if already at the end.
    pub fn forward(&mut self) -> Option<&HistoryEntry> {
        if self.cursor + 1 >= self.entries.len() {
            None
        } else {
            self.cursor += 1;
            self.current()
        }
    }

    /// `history.go(delta)` — move the cursor by `delta` entries (negative ⇒
    /// toward the start, positive ⇒ toward the end, `0` ⇒ no movement).
    /// Returns the new current entry, or `None` if the target index is out
    /// of range (the cursor is left unchanged in that case).
    pub fn go(&mut self, delta: i32) -> Option<&HistoryEntry> {
        if delta == 0 {
            return self.current();
        }
        let target = self.cursor as i64 + delta as i64;
        if target < 0 || target >= self.entries.len() as i64 {
            return None;
        }
        self.cursor = target as usize;
        self.current()
    }

    /// The full entry stack, in order (for the host hook's history UI +
    /// the devtools surface). The cursor is [`Self::index`].
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// Construct a session history from an explicit entry list + cursor
    /// (the escape hatch for the host hook that restores a persisted
    /// session history). The cursor is clamped to the valid range; an
    /// empty list is rejected (the § 7.1 invariant).
    pub fn with_entries(entries: Vec<HistoryEntry>, cursor: usize) -> Option<Self> {
        if entries.is_empty() {
            return None;
        }
        let cursor = cursor.min(entries.len() - 1);
        Some(Self { entries, cursor })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn nav(url: &str) -> HistoryEntry {
        HistoryEntry::navigation(url)
    }

    fn push(url: &str, state: &[u8]) -> HistoryEntry {
        HistoryEntry::push_state(url, state.to_vec())
    }

    // --- ScrollRestoration -------------------------------------------

    #[test]
    fn scroll_restoration_parse_round_trip() {
        assert_eq!(
            ScrollRestoration::parse("auto"),
            Some(ScrollRestoration::Auto)
        );
        assert_eq!(
            ScrollRestoration::parse("MANUAL"),
            Some(ScrollRestoration::Manual)
        );
        assert_eq!(
            ScrollRestoration::parse("auto").unwrap().to_keyword(),
            "auto"
        );
        assert_eq!(ScrollRestoration::parse("bogus"), None);
    }

    // --- basic surface -----------------------------------------------

    #[test]
    fn new_has_one_entry_at_cursor_zero() {
        let h = SessionHistory::new(nav("https://a.test/"));
        assert_eq!(h.length(), 1);
        assert_eq!(h.index(), 0);
        assert_eq!(h.url(), Some("https://a.test/"));
        assert_eq!(h.state(), None);
        assert!(!h.can_go_back());
        assert!(!h.can_go_forward());
    }

    #[test]
    fn push_appends_and_advances_cursor() {
        let mut h = SessionHistory::new(nav("https://a.test/"));
        h.push(push("https://a.test/#1", b"state-1"));
        assert_eq!(h.length(), 2);
        assert_eq!(h.index(), 1);
        assert_eq!(h.url(), Some("https://a.test/#1"));
        assert_eq!(h.state(), Some(&b"state-1"[..]));
        assert!(h.can_go_back());
        assert!(!h.can_go_forward());
    }

    #[test]
    fn push_truncates_forward_branch() {
        // Build a history: a → b → c, back to b, push d ⇒ the c entry is dropped.
        let mut h = SessionHistory::new(nav("a"));
        h.push(nav("b"));
        h.push(nav("c"));
        h.back(); // cursor → b (1)
        h.push(nav("d"));
        assert_eq!(h.length(), 3, "c dropped, d appended");
        assert_eq!(
            h.entries()
                .iter()
                .map(|e| e.url.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "d"]
        );
        assert_eq!(h.index(), 2);
        assert!(!h.can_go_forward(), "no forward branch after the push");
    }

    #[test]
    fn replace_swaps_current_entry_keeping_length() {
        let mut h = SessionHistory::new(nav("a"));
        h.push(nav("b"));
        h.replace(push("b2", b"state"));
        assert_eq!(h.length(), 2);
        assert_eq!(h.index(), 1);
        assert_eq!(h.url(), Some("b2"));
        assert_eq!(h.state(), Some(&b"state"[..]));
    }

    // --- back / forward / go -----------------------------------------

    #[test]
    fn back_and_forward_move_cursor() {
        let mut h = SessionHistory::new(nav("a"));
        h.push(nav("b"));
        h.push(nav("c"));
        assert_eq!(h.back().map(|e| e.url.as_str()), Some("b"));
        assert_eq!(h.back().map(|e| e.url.as_str()), Some("a"));
        assert_eq!(h.back(), None, "already at the start");
        assert_eq!(h.forward().map(|e| e.url.as_str()), Some("b"));
        assert_eq!(h.forward().map(|e| e.url.as_str()), Some("c"));
        assert_eq!(h.forward(), None, "already at the end");
    }

    #[test]
    fn go_negative_and_positive_delta() {
        let mut h = SessionHistory::new(nav("a"));
        h.push(nav("b"));
        h.push(nav("c"));
        h.push(nav("d"));
        assert_eq!(h.go(-2).map(|e| e.url.as_str()), Some("b"));
        assert_eq!(h.go(2).map(|e| e.url.as_str()), Some("d"));
    }

    #[test]
    fn go_zero_is_noop_returns_current() {
        let mut h = SessionHistory::new(nav("a"));
        h.push(nav("b"));
        assert_eq!(h.go(0).map(|e| e.url.as_str()), Some("b"));
        assert_eq!(h.index(), 1);
    }

    #[test]
    fn go_out_of_range_is_none_and_cursor_unchanged() {
        let mut h = SessionHistory::new(nav("a"));
        h.push(nav("b"));
        assert_eq!(h.go(-5), None);
        assert_eq!(h.index(), 1, "cursor unchanged on out-of-range go");
        assert_eq!(h.go(99), None);
        assert_eq!(h.index(), 1);
    }

    // --- scroll restoration ------------------------------------------

    #[test]
    fn scroll_restoration_defaults_auto_per_entry() {
        let h = SessionHistory::new(nav("a"));
        assert_eq!(h.scroll_restoration(), ScrollRestoration::Auto);
    }

    #[test]
    fn replace_can_set_scroll_restoration() {
        let mut h = SessionHistory::new(nav("a"));
        let mut e = push("a", b"s");
        e.scroll_restoration = ScrollRestoration::Manual;
        h.replace(e);
        assert_eq!(h.scroll_restoration(), ScrollRestoration::Manual);
    }

    #[test]
    fn current_scroll_state_restores_only_in_auto_mode() {
        let mut history = SessionHistory::new(nav("a"));
        let state = HistoryScrollState {
            root_offset: (12.5, 80.0),
            element_offsets: vec![HistoryElementScroll {
                node_id: 7,
                element_id: Some("inner".to_owned()),
                tag: "div".to_owned(),
                offset: (0.0, 35.5),
            }],
        };

        history.set_current_scroll_state(state.clone());
        assert_eq!(history.restoration_scroll_state(), Some(&state));
        history.set_scroll_restoration(ScrollRestoration::Manual);
        assert_eq!(history.restoration_scroll_state(), None);
        assert_eq!(history.current().unwrap().scroll_state, Some(state));
    }

    // --- with_entries ------------------------------------------------

    #[test]
    fn with_entries_clamps_cursor() {
        let entries = vec![nav("a"), nav("b"), nav("c")];
        let h = SessionHistory::with_entries(entries, 99).unwrap();
        assert_eq!(h.length(), 3);
        assert_eq!(h.index(), 2, "cursor clamped to last entry");
    }

    #[test]
    fn with_entries_rejects_empty() {
        assert!(SessionHistory::with_entries(vec![], 0).is_none());
    }
}
