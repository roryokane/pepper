use crate::{
    buffer_view::{BufferViewHandle, CursorMovement, CursorMovementKind},
    client_event::Key,
    editor::KeysIterator,
    mode::{Mode, ModeContext, ModeOperation},
};

pub fn on_enter(ctx: &mut ModeContext) {
    ctx.picker.reset();
}

pub fn on_exit(ctx: &mut ModeContext) {
    ctx.picker.reset();
}

pub fn on_event(ctx: &mut ModeContext, keys: &mut KeysIterator) -> ModeOperation {
    let handle = match ctx.current_buffer_view_handle() {
        Some(handle) => handle,
        None => return ModeOperation::EnterMode(Mode::default()),
    };

    match keys.next() {
        Key::Esc => {
            unwrap_or_none!(ctx.buffer_views.get_mut(handle)).commit_edits(ctx.buffers);
            return ModeOperation::EnterMode(Mode::default());
        }
        Key::Tab => ctx.buffer_views.insert_text(
            ctx.buffers,
            ctx.word_database,
            &ctx.config.syntaxes,
            handle,
            "\t",
        ),
        Key::Enter => ctx.buffer_views.insert_text(
            ctx.buffers,
            ctx.word_database,
            &ctx.config.syntaxes,
            handle,
            "\n",
        ),
        Key::Char(c) => {
            let mut buf = [0; std::mem::size_of::<char>()];
            let s = c.encode_utf8(&mut buf);
            ctx.buffer_views.insert_text(
                ctx.buffers,
                ctx.word_database,
                &ctx.config.syntaxes,
                handle,
                s,
            );
        }
        Key::Backspace => {
            unwrap_or_none!(ctx.buffer_views.get_mut(handle)).move_cursors(
                ctx.buffers,
                CursorMovement::ColumnsBackward(1),
                CursorMovementKind::PositionOnly,
            );
            ctx.buffer_views.delete_in_selection(
                ctx.buffers,
                ctx.word_database,
                &ctx.config.syntaxes,
                handle,
            );
        }
        Key::Delete => {
            unwrap_or_none!(ctx.buffer_views.get_mut(handle)).move_cursors(
                ctx.buffers,
                CursorMovement::ColumnsForward(1),
                CursorMovementKind::PositionOnly,
            );
            ctx.buffer_views.delete_in_selection(
                ctx.buffers,
                ctx.word_database,
                &ctx.config.syntaxes,
                handle,
            );
        }
        Key::Ctrl('w') => {
            unwrap_or_none!(ctx.buffer_views.get_mut(handle)).move_cursors(
                ctx.buffers,
                CursorMovement::WordsBackward(1),
                CursorMovementKind::PositionOnly,
            );
            ctx.buffer_views.delete_in_selection(
                ctx.buffers,
                ctx.word_database,
                &ctx.config.syntaxes,
                handle,
            );
        }
        Key::Ctrl('n') => {
            apply_completion(ctx, handle, 1);
            return ModeOperation::None;
        }
        Key::Ctrl('p') => {
            apply_completion(ctx, handle, -1);
            return ModeOperation::None;
        }
        _ => (),
    }

    let buffer_view = unwrap_or_none!(ctx.buffer_views.get(handle));
    let buffer = unwrap_or_none!(ctx.buffers.get(buffer_view.buffer_handle));
    let mut word_position = buffer_view.cursors.main_cursor().position;
    word_position.column_index = word_position.column_index.saturating_sub(1);
    let (word_range, word) = buffer.content.find_word_at(word_position);
    if word.is_empty() || word_position.column_index < word_range.to.column_index.saturating_sub(1)
    {
        ctx.picker.clear_filtered();
    } else {
        ctx.picker.filter(&ctx.word_database, word);
        if ctx.picker.height(usize::MAX) == 1 {
            ctx.picker.clear_filtered();
        }
    }

    ModeOperation::None
}

fn apply_completion(ctx: &mut ModeContext, handle: BufferViewHandle, cursor_movement: isize) {
    ctx.picker.move_cursor(cursor_movement);
    let entry_name = ctx.picker.current_entry_name(&ctx.word_database);
    ctx.buffer_views.apply_completion(
        ctx.buffers,
        ctx.word_database,
        &ctx.config.syntaxes,
        handle,
        &entry_name,
    );
}
