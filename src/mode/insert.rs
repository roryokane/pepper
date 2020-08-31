use crate::{
    buffer::TextRef,
    buffer_position::BufferOffset,
    buffer_view::MovementKind,
    client_event::Key,
    editor::KeysIterator,
    mode::{Mode, ModeContext, ModeOperation},
};

pub fn on_enter(_ctx: &mut ModeContext) {}

pub fn on_event(ctx: &mut ModeContext, keys: &mut KeysIterator) -> ModeOperation {
    let handle = match ctx.current_buffer_view_handle() {
        Some(handle) => handle,
        None => return ModeOperation::EnterMode(Mode::Normal),
    };

    match keys.next() {
        Key::Esc | Key::Ctrl('c') => {
            unwrap_or_none!(ctx.buffer_views.get_mut(handle)).commit_edits(ctx.buffers);
            return ModeOperation::EnterMode(Mode::Normal);
        }
        Key::Tab => ctx.buffer_views.insert_text(
            ctx.buffers,
            &ctx.config.syntaxes,
            handle,
            TextRef::Char('\t'),
        ),
        Key::Ctrl('m') => ctx.buffer_views.insert_text(
            ctx.buffers,
            &ctx.config.syntaxes,
            handle,
            TextRef::Char('\n'),
        ),
        Key::Char(c) => ctx.buffer_views.insert_text(
            ctx.buffers,
            &ctx.config.syntaxes,
            handle,
            TextRef::Char(c),
        ),
        Key::Ctrl('h') => {
            unwrap_or_none!(ctx.buffer_views.get_mut(handle)).move_cursors(
                ctx.buffers,
                BufferOffset::line_col(0, -1),
                MovementKind::PositionOnly,
            );
            ctx.buffer_views
                .delete_in_selection(ctx.buffers, &ctx.config.syntaxes, handle);
        }
        Key::Delete => {
            unwrap_or_none!(ctx.buffer_views.get_mut(handle)).move_cursors(
                ctx.buffers,
                BufferOffset::line_col(0, 1),
                MovementKind::PositionOnly,
            );
            ctx.buffer_views
                .delete_in_selection(ctx.buffers, &ctx.config.syntaxes, handle);
        }
        _ => (),
    }

    ModeOperation::None
}
