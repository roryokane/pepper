#![macro_use]

use std::mem::Discriminant;

use crate::{
    buffer::BufferCollection,
    buffer_view::{BufferViewCollection, BufferViewHandle},
    client::{ClientCollection, TargetClient},
    client_event::Key,
    config::Config,
    editor::{KeysIterator, StatusMessageKind},
    keymap::KeyMapCollection,
    script::ScriptEngine,
    select::SelectEntryCollection,
};

macro_rules! unwrap_or_none {
    ($e:expr) => {
        match $e {
            Some(v) => v,
            None => return ModeOperation::None,
        }
    };
}

mod insert;
mod normal;
mod script;
mod search;
mod select;

pub enum ModeOperation {
    Pending,
    None,
    Quit,
    QuitAll,
    EnterMode(Mode),
}

pub struct ModeContext<'a> {
    pub target_client: TargetClient,
    pub clients: &'a mut ClientCollection,

    pub config: &'a mut Config,

    pub buffers: &'a mut BufferCollection,
    pub buffer_views: &'a mut BufferViewCollection,

    pub input: &'a mut String,
    pub selects: &'a mut SelectEntryCollection,

    pub status_message_kind: &'a mut StatusMessageKind,
    pub status_message: &'a mut String,

    pub keymaps: &'a mut KeyMapCollection,
    pub scripts: &'a mut ScriptEngine,
}

impl<'a> ModeContext<'a> {
    pub fn current_buffer_view_handle(&self) -> Option<BufferViewHandle> {
        self.clients
            .get(self.target_client)
            .and_then(|c| c.current_buffer_view_handle)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum FromMode {
    Normal,
    Select,
}

impl FromMode {
    pub fn as_mode(&self) -> Mode {
        match self {
            FromMode::Normal => Mode::Normal,
            FromMode::Select => Mode::Select,
        }
    }
}

pub enum Mode {
    Normal,
    Select,
    Insert,
    Search(FromMode),
    Script(FromMode),
}

impl Mode {
    pub fn discriminant(&self) -> Discriminant<Self> {
        std::mem::discriminant(self)
    }

    pub fn on_enter(&mut self, context: &mut ModeContext) {
        match self {
            Mode::Normal => normal::on_enter(context),
            Mode::Select => select::on_enter(context),
            Mode::Insert => insert::on_enter(context),
            Mode::Search(_) => search::on_enter(context),
            Mode::Script(_) => script::on_enter(context),
        }
    }

    pub fn on_exit(&mut self, context: &mut ModeContext) {
        match self {
            Mode::Normal => normal::on_exit(context),
            Mode::Select => select::on_exit(context),
            Mode::Insert => insert::on_exit(context),
            Mode::Search(_) => search::on_exit(context),
            Mode::Script(_) => script::on_exit(context),
        }
    }

    pub fn on_event(
        &mut self,
        context: &mut ModeContext,
        keys: &mut KeysIterator,
    ) -> ModeOperation {
        match self {
            Mode::Normal => normal::on_event(context, keys),
            Mode::Select => select::on_event(context, keys),
            Mode::Insert => insert::on_event(context, keys),
            Mode::Search(from_mode) => search::on_event(context, keys, *from_mode),
            Mode::Script(from_mode) => script::on_event(context, keys, *from_mode),
        }
    }
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Normal
    }
}

pub enum InputPollResult {
    Pending,
    Submited,
    Canceled,
}

pub fn poll_input(ctx: &mut ModeContext, keys: &mut KeysIterator) -> InputPollResult {
    match keys.next() {
        Key::Esc | Key::Ctrl('c') => {
            ctx.input.clear();
            InputPollResult::Canceled
        }
        Key::Ctrl('m') => InputPollResult::Submited,
        Key::Ctrl('u') => {
            ctx.input.clear();
            InputPollResult::Pending
        }
        Key::Ctrl('w') => {
            let mut found_space = false;
            let mut last_index = 0;
            for (i, c) in ctx.input.char_indices().rev() {
                if found_space {
                    if c != ' ' {
                        break;
                    }
                } else if c == ' ' {
                    found_space = true;
                }
                last_index = i;
            }

            ctx.input.truncate(last_index);
            InputPollResult::Pending
        }
        Key::Ctrl('h') => {
            if let Some((last_char_index, _)) = ctx.input.char_indices().rev().next() {
                ctx.input.truncate(last_char_index);
            }
            InputPollResult::Pending
        }
        Key::Char(c) => {
            ctx.input.push(c);
            InputPollResult::Pending
        }
        _ => InputPollResult::Pending,
    }
}
