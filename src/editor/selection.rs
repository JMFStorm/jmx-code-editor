/// A selection over the rope, in char indices. `head` is where the cursor is drawn;
/// `anchor` is the fixed end. When they are equal the selection is just a cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn point(pos: usize) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Ordered `(start, end)`.
    pub fn range(&self) -> (usize, usize) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Move the head, either extending the selection or collapsing it to a cursor.
    pub fn moved_to(self, pos: usize, extend: bool) -> Self {
        if extend {
            Self {
                anchor: self.anchor,
                head: pos,
            }
        } else {
            Self::point(pos)
        }
    }

    pub fn clamp(self, len: usize) -> Self {
        Self {
            anchor: self.anchor.min(len),
            head: self.head.min(len),
        }
    }
}
