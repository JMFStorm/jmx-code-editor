use std::time::{Duration, Instant};

use super::selection::Selection;

/// One replace operation: at `start`, `removed` was replaced by `inserted`.
/// Changes inside a transaction are applied in order, each in the coordinates
/// left by the previous one, so undo replays them in reverse.
#[derive(Debug, Clone)]
pub struct Change {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Plain typing; consecutive inserts merge into one undo step.
    Insert,
    /// Backspace/delete of single characters; consecutive deletes merge.
    Delete,
    /// Anything else (paste, line ops, indent...) is always its own step.
    Other,
}

#[derive(Debug, Clone)]
pub struct Transaction {
    id: u64,
    kind: EditKind,
    pub changes: Vec<Change>,
    pub before: Selection,
    pub after: Selection,
}

const COALESCE_WINDOW: Duration = Duration::from_millis(1000);

#[derive(Debug)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    next_id: u64,
    /// Id of the transaction on top of the undo stack when the file was last saved (0 = none).
    saved_id: u64,
    last_edit: Option<Instant>,
    coalesce: bool,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 1,
            saved_id: 0,
            last_edit: None,
            coalesce: false,
        }
    }
}

impl History {
    fn current_id(&self) -> u64 {
        self.undo.last().map_or(0, |t| t.id)
    }

    pub fn is_dirty(&self) -> bool {
        self.current_id() != self.saved_id
    }

    pub fn mark_saved(&mut self) {
        self.saved_id = self.current_id();
        self.break_coalescing();
    }

    /// The next edit starts a new undo step (called on cursor moves, saves, etc.).
    pub fn break_coalescing(&mut self) {
        self.coalesce = false;
    }

    pub fn record(
        &mut self,
        kind: EditKind,
        changes: Vec<Change>,
        before: Selection,
        after: Selection,
    ) {
        if changes.is_empty() {
            return;
        }
        self.redo.clear();
        let now = Instant::now();
        let recent = self
            .last_edit
            .is_some_and(|t| now.duration_since(t) < COALESCE_WINDOW);
        self.last_edit = Some(now);

        if self.coalesce
            && recent
            && kind != EditKind::Other
            && changes.len() == 1
            && let Some(top) = self.undo.last_mut()
            && top.kind == kind
            && top.changes.len() == 1
            && merge(&mut top.changes[0], &changes[0])
        {
            top.after = after;
            return;
        }

        let id = self.next_id;
        self.next_id += 1;
        self.undo.push(Transaction {
            id,
            kind,
            changes,
            before,
            after,
        });
        self.coalesce = kind != EditKind::Other;
    }

    pub fn pop_undo(&mut self) -> Option<Transaction> {
        let t = self.undo.pop()?;
        self.redo.push(t.clone());
        self.break_coalescing();
        Some(t)
    }

    pub fn pop_redo(&mut self) -> Option<Transaction> {
        let t = self.redo.pop()?;
        self.undo.push(t.clone());
        self.break_coalescing();
        Some(t)
    }
}

/// Try to fold `new` into `top`. Returns false if they are not adjacent.
fn merge(top: &mut Change, new: &Change) -> bool {
    let top_ins = top.inserted.chars().count();
    // Typing: pure insert right after the previous insert. A newline ends the word-ish group.
    if top.removed.is_empty()
        && new.removed.is_empty()
        && !new.inserted.contains('\n')
        && new.start == top.start + top_ins
    {
        top.inserted.push_str(&new.inserted);
        return true;
    }
    // Backspace: pure delete ending where the previous delete started.
    if top.inserted.is_empty() && new.inserted.is_empty() {
        let new_rem = new.removed.chars().count();
        if new.start + new_rem == top.start {
            top.start = new.start;
            top.removed.insert_str(0, &new.removed);
            return true;
        }
        // Forward delete: same start position.
        if new.start == top.start {
            top.removed.push_str(&new.removed);
            return true;
        }
    }
    false
}
