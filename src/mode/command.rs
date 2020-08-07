use crate::{
    command::{CommandContext, CommandOperation},
    connection::TargetClient,
    editor::{EditorOperation, KeysIterator},
    mode::{poll_input, FromMode, InputPollResult, ModeContext, ModeOperation},
};

pub fn on_enter(ctx: &mut ModeContext) {
    ctx.input.clear();
    ctx.operations
        .send(TargetClient::All, EditorOperation::InputKeep(0));
}

pub fn on_event(
    mut ctx: &mut ModeContext,
    keys: &mut KeysIterator,
    from_mode: &FromMode,
) -> ModeOperation {
    match poll_input(&mut ctx, keys) {
        InputPollResult::Pending => ModeOperation::None,
        InputPollResult::Canceled => ModeOperation::EnterMode(from_mode.as_mode()),
        InputPollResult::Submited => {
            let command_context = CommandContext {
                target_client: ctx.target_client,
                operations: ctx.operations,

                config: ctx.config,
                keymaps: ctx.keymaps,
                buffers: ctx.buffers,
                buffer_views: ctx.buffer_views,
                current_buffer_view_handle: ctx.current_buffer_view_handle,
            };

            match ctx
                .commands
                .parse_and_execute(command_context, &ctx.input[..])
            {
                Ok(CommandOperation::Complete) => ModeOperation::EnterMode(from_mode.as_mode()),
                Ok(CommandOperation::Quit) => ModeOperation::Quit,
                Err(error) => ModeOperation::Error(error),
            }
        }
    }
}
