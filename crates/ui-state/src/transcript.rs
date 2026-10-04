//! The transcript window and its run index.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::RangeInclusive;

use wire::{Append, Item, Kind};

use crate::Key;
use crate::body::{Explore, ItemBody, ItemClass};

/// One held item, decoded once per revision.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub item: Item,
    pub body: ItemBody,
    pub class: ItemClass,
}

impl Held {
    pub(crate) fn new(kind: Kind, item: Item) -> Held {
        let kind = wire::kind_from_tag(&item.kind).unwrap_or(kind);
        let body = ItemBody::decode(kind, &item.body);
        let class = body.class();
        Held { item, body, class }
    }
}

/// A contiguous window of items by order: every item the client holds with
/// order in `[oldest_held, head]`. A row above the head appends; inside the
/// window it upserts by key if newer, else is ignored, which absorbs the
/// overlap between a store read and the broadcast; below `oldest_held` it is
/// dropped, since a page brings the current version if it is ever scrolled
/// to. Pages only extend the low edge. The session trims the oldest rows
/// while the reader follows the newest, so the window never grows past its
/// cap then; the rows trimmed stay in the store, a page away.
#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    kind: Kind,
    items: BTreeMap<u64, Held>,
    by_key: HashMap<Key, u64>,
    by_input: HashMap<Vec<u8>, Key>,
    exhausted: bool,
    runs: RunIndex,
}

/// What an append did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Appended {
    Applied,
    /// The held revision already covers it.
    Stale,
    /// The base is not held; the driver answers with Get.
    NeedGet,
}

impl Transcript {
    pub fn new(kind: Kind) -> Transcript {
        Transcript {
            kind,
            items: BTreeMap::new(),
            by_key: HashMap::new(),
            by_input: HashMap::new(),
            exhausted: false,
            runs: RunIndex::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The oldest held order: the page-older cursor.
    pub fn oldest_held(&self) -> Option<u64> {
        self.items.keys().next().copied()
    }

    pub fn head(&self) -> Option<u64> {
        self.items.keys().next_back().copied()
    }

    /// Whether history older than the window exists. Orders start at one and
    /// are assigned densely, so a window reaching order one has everything; a
    /// page that came back exhausted says the rest is gone.
    pub fn has_older(&self) -> bool {
        !self.exhausted && self.oldest_held().is_some_and(|oldest| oldest > 1)
    }

    pub fn get(&self, key: &str) -> Option<&Held> {
        self.by_key.get(key).and_then(|order| self.items.get(order))
    }

    pub fn at(&self, order: u64) -> Option<&Held> {
        self.items.get(&order)
    }

    /// The key of the item carrying this input id, a prompt's reflection.
    pub fn key_for_input(&self, input_id: &[u8]) -> Option<&Key> {
        self.by_input.get(input_id)
    }

    /// Held items, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Held> {
        self.items.values()
    }

    pub fn range(&self, range: RangeInclusive<u64>) -> impl DoubleEndedIterator<Item = &Held> {
        self.items.range(range).map(|(_, held)| held)
    }

    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.items.values().map(|held| &held.item.key)
    }

    pub fn runs(&self) -> &RunIndex {
        &self.runs
    }

    /// The run an order belongs to, if it is in one.
    pub fn run_at(&self, order: u64) -> Option<Run> {
        self.runs.run_at(order, self)
    }

    /// Applies one full item under the window rules and returns the keys whose
    /// rows may differ.
    pub(crate) fn upsert(&mut self, item: Item, changed: &mut Changed) {
        let order = item.order;
        match self.by_key.get(&item.key) {
            Some(&held_order) => {
                let held = &self.items[&held_order];
                if held.item.revision >= item.revision {
                    return;
                }
                // Order is assigned once per key; trust the held position.
                let mut item = item;
                item.order = held_order;
                self.replace(held_order, item, changed);
            }
            None => {
                let below = self.oldest_held().is_some_and(|oldest| order < oldest);
                if below {
                    return;
                }
                if self.items.contains_key(&order) {
                    // One key per order; a second key at a held order is not
                    // this agent's history.
                    return;
                }
                self.insert(item, changed);
            }
        }
    }

    /// Extends the low edge with an older page. Rows inside the window upsert
    /// by key if newer; rows below it insert; nothing lands above the head.
    pub(crate) fn page(&mut self, items: Vec<Item>, exhausted: bool, changed: &mut Changed) {
        let head = self.head();
        let mut items = items;
        // Newest first from the store; merge downward so each row lands below
        // the one merged before it.
        items.sort_by_key(|item| std::cmp::Reverse(item.order));
        for item in items {
            if head.is_some_and(|head| item.order > head) {
                continue;
            }
            match self.by_key.get(&item.key) {
                Some(&held_order) => {
                    if self.items[&held_order].item.revision < item.revision {
                        let mut item = item;
                        item.order = held_order;
                        self.replace(held_order, item, changed);
                    }
                }
                None if !self.items.contains_key(&item.order) => self.insert(item, changed),
                None => {}
            }
        }
        if exhausted && !self.exhausted {
            // Ruling out older history clears the open-below mark of the run
            // at the low edge.
            let before = self.oldest_held().and_then(|oldest| self.run_at(oldest));
            self.exhausted = true;
            let after = self.oldest_held().and_then(|oldest| self.run_at(oldest));
            if before != after
                && let Some(run) = after
            {
                self.runs.members(&run, self, changed);
            }
        }
    }

    pub(crate) fn append(&mut self, append: &Append, changed: &mut Changed) -> Appended {
        let Some(&order) = self.by_key.get(&append.key) else {
            return Appended::NeedGet;
        };
        let held = &self.items[&order];
        if held.item.revision >= append.revision {
            return Appended::Stale;
        }
        if held.item.revision != append.base_revision {
            return Appended::NeedGet;
        }
        let mut item = held.item.clone();
        item.text.push_str(&append.text);
        item.revision = append.revision;
        self.replace(order, item, changed);
        Appended::Applied
    }

    /// Drops the oldest rows until at most `cap` remain. Older history then
    /// exists again, and the run at the new low edge reads open below; its
    /// indexes forget the dropped rows, so a later revision of one is
    /// ignored like any row below the window.
    pub(crate) fn trim(&mut self, cap: usize, changed: &mut Changed) {
        if self.items.len() <= cap {
            return;
        }
        while self.items.len() > cap {
            let Some((_, held)) = self.items.pop_first() else {
                break;
            };
            let key = &held.item.key;
            self.by_key.remove(key);
            if self.by_input.get(&held.item.input_id) == Some(key) {
                self.by_input.remove(&held.item.input_id);
            }
            changed.key(key);
        }
        self.exhausted = false;
        let Some(oldest) = self.oldest_held() else {
            self.runs = RunIndex::default();
            return;
        };
        // A run the cut went through loses members; whatever run starts at
        // the new low edge now reads open below.
        let cut_run = self
            .runs
            .containing(oldest)
            .filter(|(start, segment)| **start < oldest && segment.len >= 2)
            .map(|(_, segment)| segment.end);
        let mut runs = std::mem::take(&mut self.runs);
        runs.cut_below(oldest, self);
        self.runs = runs;
        if let Some(end) = cut_run {
            for held in self.items.range(oldest..=end).map(|(_, held)| held) {
                changed.key(&held.item.key);
            }
        }
        if let Some(run) = self.run_at(oldest) {
            self.runs.members(&run, self, changed);
        }
    }

    /// Checks the block invariant: orders contiguous, and every index names
    /// only held rows.
    pub fn check(&self) -> Result<(), String> {
        if let (Some(oldest), Some(head)) = (self.oldest_held(), self.head())
            && head - oldest + 1 != self.items.len() as u64
        {
            return Err(format!(
                "{} rows held between {oldest} and {head}",
                self.items.len()
            ));
        }
        for (key, order) in &self.by_key {
            if self.items.get(order).map(|held| &held.item.key) != Some(key) {
                return Err(format!("key {key} indexed at {order}, which holds another"));
            }
        }
        if self.by_key.len() != self.items.len() {
            return Err("a held row is not indexed by key".into());
        }
        for key in self.by_input.values() {
            if !self.by_key.contains_key(key) {
                return Err(format!("input id indexes {key}, which is not held"));
            }
        }
        if self.runs != RunIndex::rebuild(self) {
            return Err("run index drifted from a rebuild".into());
        }
        Ok(())
    }

    fn reindex(&mut self, order: u64, inserted: bool) {
        let mut runs = std::mem::take(&mut self.runs);
        runs.reindex(order, inserted, self);
        self.runs = runs;
    }

    fn insert(&mut self, item: Item, changed: &mut Changed) {
        let order = item.order;
        let held = Held::new(self.kind, item);
        let before = self.runs.capture(order, self);
        self.by_key.insert(held.item.key.clone(), order);
        if !held.item.input_id.is_empty() {
            self.by_input
                .insert(held.item.input_id.clone(), held.item.key.clone());
        }
        changed.key(&held.item.key);
        self.items.insert(order, held);
        self.reindex(order, true);
        self.runs.compare(before, self, changed);
    }

    fn replace(&mut self, order: u64, item: Item, changed: &mut Changed) {
        let held = Held::new(self.kind, item);
        let old = &self.items[&order];
        if old.item.input_id != held.item.input_id {
            self.by_input.remove(&old.item.input_id);
        }
        if !held.item.input_id.is_empty() {
            self.by_input
                .insert(held.item.input_id.clone(), held.item.key.clone());
        }
        let explore_moved = old.class.explore() != held.class.explore();
        changed.key(&held.item.key);
        if explore_moved {
            let before = self.runs.capture(order, self);
            self.items.insert(order, held);
            self.reindex(order, false);
            self.runs.compare(before, self, changed);
        } else {
            self.items.insert(order, held);
        }
    }
}

/// Keys whose rows may differ, in first-touched order, without repeats.
#[derive(Debug, Default)]
pub(crate) struct Changed {
    keys: Vec<Key>,
    seen: BTreeSet<Key>,
}

impl Changed {
    pub(crate) fn key(&mut self, key: &str) {
        if self.seen.insert(key.to_owned()) {
            self.keys.push(key.to_owned());
        }
    }

    pub(crate) fn into_keys(self) -> Vec<Key> {
        self.keys
    }
}

/// A run: two or more consecutive held exploration calls. The newest member
/// is the visible summary. `open_below` marks a run that starts at the
/// oldest held item while older history exists, so it may continue below.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub oldest: u64,
    pub newest: u64,
    pub oldest_key: Key,
    pub newest_key: Key,
    pub len: u32,
    pub reads: u32,
    pub searches: u32,
    pub open_below: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Segment {
    end: u64,
    len: u32,
    reads: u32,
    searches: u32,
}

/// Run membership, derived from the transcript and kept current on each
/// message: maximal segments of consecutive held exploration items, keyed by
/// their oldest order. A head append extends the last segment in place; a
/// change inside the window rebuilds only the segments around it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunIndex {
    segments: BTreeMap<u64, Segment>,
}

/// The runs around one order before a change, to compare after it.
pub(crate) struct Captured {
    runs: Vec<(u64, Option<Run>)>,
}

impl RunIndex {
    /// Rebuilds the index from a transcript: the oracle the incremental
    /// upkeep is tested against.
    pub fn rebuild(transcript: &Transcript) -> RunIndex {
        let mut index = RunIndex::default();
        if let (Some(lo), Some(hi)) = (transcript.oldest_held(), transcript.head()) {
            index.scan(lo..=hi, transcript);
        }
        index
    }

    pub fn run_at(&self, order: u64, transcript: &Transcript) -> Option<Run> {
        let (&start, segment) = self.containing(order)?;
        (segment.len >= 2).then(|| Run {
            oldest: start,
            newest: segment.end,
            oldest_key: transcript.items[&start].item.key.clone(),
            newest_key: transcript.items[&segment.end].item.key.clone(),
            len: segment.len,
            reads: segment.reads,
            searches: segment.searches,
            open_below: transcript.oldest_held() == Some(start) && transcript.has_older(),
        })
    }

    /// Every run, oldest first, as (oldest order, newest order).
    pub fn spans(&self) -> impl Iterator<Item = RangeInclusive<u64>> + '_ {
        self.segments
            .iter()
            .filter(|(_, segment)| segment.len >= 2)
            .map(|(&start, segment)| start..=segment.end)
    }

    fn containing(&self, order: u64) -> Option<(&u64, &Segment)> {
        self.segments
            .range(..=order)
            .next_back()
            .filter(|(_, segment)| segment.end >= order)
    }

    /// The span a change at `order` can affect: the segments of its held
    /// neighbours and of the order itself.
    fn span(&self, order: u64, transcript: &Transcript) -> RangeInclusive<u64> {
        let below = transcript.items.range(..order).next_back().map(|(&o, _)| o);
        let above = transcript.items.range(order + 1..).next().map(|(&o, _)| o);
        let mut lo = below.unwrap_or(order).min(order);
        let mut hi = above.unwrap_or(order).max(order);
        for probe in [below, Some(order), above].into_iter().flatten() {
            if let Some((&start, segment)) = self.containing(probe) {
                lo = lo.min(start);
                hi = hi.max(segment.end);
            }
        }
        lo..=hi
    }

    fn capture(&self, order: u64, transcript: &Transcript) -> Captured {
        let span = self.span(order, transcript);
        let runs = transcript
            .items
            .range(span.clone())
            .map(|(&o, _)| (o, self.run_at(o, transcript)))
            .collect();
        Captured { runs }
    }

    fn reindex(&mut self, order: u64, inserted: bool, transcript: &Transcript) {
        // Fast path: a new head item that continues or starts the last run.
        if inserted && transcript.head() == Some(order) {
            let below = transcript.items.range(..order).next_back().map(|(&o, _)| o);
            let explore = transcript.items[&order].class.explore();
            let last = self.segments.iter_mut().next_back();
            match (explore, last) {
                (None, _) => return,
                (Some(kind), Some((_, segment))) if Some(segment.end) == below => {
                    segment.end = order;
                    segment.len += 1;
                    count(segment, kind, 1);
                    return;
                }
                (Some(kind), _) => {
                    let mut segment = Segment {
                        end: order,
                        len: 1,
                        reads: 0,
                        searches: 0,
                    };
                    count(&mut segment, kind, 1);
                    self.segments.insert(order, segment);
                    return;
                }
            }
        }
        let span = self.span(order, transcript);
        self.scan(span, transcript);
    }

    /// Forgets rows below `oldest`: segments that started there go, and one
    /// that reached past it is scanned again from `oldest`.
    fn cut_below(&mut self, oldest: u64, transcript: &Transcript) {
        let cut: Vec<u64> = self
            .segments
            .range(..oldest)
            .map(|(&start, _)| start)
            .collect();
        for start in cut {
            if let Some(segment) = self.segments.remove(&start)
                && segment.end >= oldest
            {
                self.scan(oldest..=segment.end, transcript);
            }
        }
    }

    /// Replaces every segment inside `span` with a fresh scan of it.
    fn scan(&mut self, span: RangeInclusive<u64>, transcript: &Transcript) {
        let starts: Vec<u64> = self
            .segments
            .range(span.clone())
            .map(|(&start, _)| start)
            .collect();
        for start in starts {
            self.segments.remove(&start);
        }
        let mut open: Option<(u64, Segment)> = None;
        for (&order, held) in transcript.items.range(span) {
            match held.class.explore() {
                Some(kind) => {
                    let (_, segment) = open.get_or_insert((
                        order,
                        Segment {
                            end: order,
                            len: 0,
                            reads: 0,
                            searches: 0,
                        },
                    ));
                    segment.end = order;
                    segment.len += 1;
                    count(segment, kind, 1);
                }
                None => {
                    if let Some((start, segment)) = open.take() {
                        self.segments.insert(start, segment);
                    }
                }
            }
        }
        if let Some((start, segment)) = open {
            self.segments.insert(start, segment);
        }
    }

    /// Every row whose run changed. A change at one order only touches the
    /// segments of that order and its held neighbours, all captured before it.
    fn compare(&self, before: Captured, transcript: &Transcript, changed: &mut Changed) {
        for (order, run) in &before.runs {
            if transcript.items.contains_key(order) && self.run_at(*order, transcript) != *run {
                changed.key(&transcript.items[order].item.key);
            }
        }
    }

    fn members(&self, run: &Run, transcript: &Transcript, changed: &mut Changed) {
        for held in transcript.range(run.oldest..=run.newest) {
            changed.key(&held.item.key);
        }
    }
}

fn count(segment: &mut Segment, kind: Explore, by: u32) {
    match kind {
        Explore::Read => segment.reads += by,
        Explore::Search => segment.searches += by,
        Explore::Other => {}
    }
}
