use crate::{
    buffer::{Buffer, Text},
    buffer_position::{BufferOffset, BufferPosition, BufferRange},
};

#[derive(Clone, Copy)]
pub enum EditKind {
    Insert,
    Delete,
}

pub struct Edit {
    pub kind: EditKind,
    pub range: BufferRange,
    pub text: Text,
}

impl Edit {
    pub fn new(kind: EditKind, position: BufferPosition, text: Text) -> Self {
        let range = match &text {
            Text::Char(_c) => BufferRange::between(
                position,
                position.offset_by(BufferOffset {
                    column_offset: 1,
                    line_offset: 0,
                }),
            ),
            Text::String(s) => BufferRange::from_str_position(position, &s[..]),
        };
        Self { kind, text, range }
    }

    pub fn apply(&self, buffer: &mut Buffer) {
        match self.kind {
            EditKind::Insert => {
                buffer.insert_text(self.range.from, self.text.as_text_ref());
            }
            EditKind::Delete => {
                buffer.delete_range(self.range);
            }
        }
    }

    pub fn revert(&self, buffer: &mut Buffer) {
        match self.kind {
            EditKind::Delete => {
                buffer.insert_text(self.range.from, self.text.as_text_ref());
            }
            EditKind::Insert => {
                buffer.delete_range(self.range);
            }
        }
    }
}

pub struct Undo {
    history: Vec<Edit>,
    group_end_indexes: Vec<usize>,
    current_group_index: usize,
}

impl Undo {
    pub fn new() -> Self {
        Self {
            history: Vec::new(),
            group_end_indexes: vec![0, 0],
            current_group_index: 1,
        }
    }

    pub fn push_edit(&mut self, edit: Edit) {
        self.history
            .truncate(self.group_end_indexes[self.current_group_index]);
        self.group_end_indexes
            .truncate(self.current_group_index + 1);

        self.history.push(edit);
        self.group_end_indexes[self.current_group_index] += 1;
    }

    pub fn commit_edits(&mut self) {
        let current_group_size = self.group_end_indexes[self.current_group_index]
            - self.group_end_indexes[self.current_group_index - 1];
        if current_group_size > 0 {
            self.current_group_index = self.group_end_indexes.len();
            self.group_end_indexes.push(self.history.len());
        }
    }

    pub fn undo(&mut self) -> impl Iterator<Item = &Edit> {
        self.commit_edits();

        let start = self.group_end_indexes[self.current_group_index - 1];
        let end = self.group_end_indexes[self.current_group_index];
        self.history[start..end].iter().rev()
    }

    pub fn redo(&mut self) -> impl Iterator<Item = &Edit> {
        self.commit_edits();

        let start = self.group_end_indexes[self.current_group_index - 1];
        let end = self.group_end_indexes[self.current_group_index];
        self.history[start..end].iter()
    }
}
