//! Bounded buffers: cursor ring for events/logs and a keyed retention store.

use serde::Serialize;
use serde_json::Value;
use std::collections::VecDeque;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    /// Monotonically increasing, starting at 0; never reused.
    pub cursor: u64,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Read {
    /// Entries with `cursor >= requested`, oldest first.
    pub entries: Vec<Entry>,
    /// Pass this as the next `since` cursor.
    pub next_cursor: u64,
    /// Entries at or after the requested cursor that were evicted before this read.
    pub dropped: u64,
    /// Entries evicted over the ring's lifetime.
    pub dropped_total: u64,
}

#[derive(Debug)]
pub struct Ring {
    cap: usize,
    items: VecDeque<Entry>,
    next: u64,
    dropped_total: u64,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self { cap: cap.max(1), items: VecDeque::new(), next: 0, dropped_total: 0 }
    }

    pub fn push(&mut self, value: Value) -> u64 {
        let cursor = self.next;
        self.next += 1;
        if self.items.len() == self.cap {
            self.items.pop_front();
            self.dropped_total += 1;
        }
        self.items.push_back(Entry { cursor, value });
        cursor
    }

    pub fn since(&self, cursor: u64) -> Read {
        let first = self.items.front().map_or(self.next, |e| e.cursor);
        Read {
            entries: self.items.iter().filter(|e| e.cursor >= cursor).cloned().collect(),
            next_cursor: self.next,
            dropped: first.saturating_sub(cursor).min(self.next.saturating_sub(cursor)),
            dropped_total: self.dropped_total,
        }
    }
}

/// Insertion-ordered keyed store that evicts the oldest entry beyond `cap`.
#[derive(Debug)]
pub struct Store<T> {
    cap: usize,
    items: VecDeque<(String, T)>,
}

impl<T> Store<T> {
    pub fn new(cap: usize) -> Self {
        Self { cap, items: VecDeque::new() }
    }

    pub fn insert(&mut self, id: String, value: T) {
        self.items.retain(|(k, _)| *k != id);
        self.items.push_back((id, value));
        while self.items.len() > self.cap {
            self.items.pop_front();
        }
    }

    pub fn get(&self, id: &str) -> Option<&T> {
        self.items.iter().find(|(k, _)| k == id).map(|(_, v)| v)
    }
}
