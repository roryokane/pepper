use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{
    buffer::{Buffer, BufferCollection, BufferContent},
    buffer_view::{BufferView, BufferViewCollection, BufferViewHandle},
    config::Config,
    connection::TargetClient,
    editor::{EditorOperation, EditorOperationSender},
    keymap::KeyMapCollection,
    mode::Mode,
};

type CommandResult = Result<CommandOperation, String>;

pub enum CommandOperation {
    Complete,
    Quit,
}

pub struct CommandContext<'a> {
    pub target_client: TargetClient,
    pub operations: &'a mut EditorOperationSender,

    pub config: &'a mut Config,
    pub keymaps: &'a mut KeyMapCollection,
    pub buffers: &'a mut BufferCollection,
    pub buffer_views: &'a mut BufferViewCollection,
    pub current_buffer_view_handle: &'a mut Option<BufferViewHandle>,
}

type CommandBody = fn(CommandContext, CommandArgs) -> CommandResult;

pub struct CommandArgs<'a> {
    raw: &'a str,
}

impl<'a> CommandArgs<'a> {
    pub fn new(args: &'a str) -> Self {
        Self { raw: args }
    }

    pub fn assert_empty(&self) -> Result<(), String> {
        if self.raw.trim_start().len() > 0 {
            Err("command expected less arguments".into())
        } else {
            Ok(())
        }
    }

    pub fn next(&mut self) -> Result<&'a str, String> {
        self.try_next()
            .ok_or_else(|| String::from("command expected more arguments"))
    }

    pub fn try_next(&mut self) -> Option<&'a str> {
        self.raw = self.raw.trim_start();
        if self.raw.len() == 0 {
            return None;
        }

        let arg = match self.raw.find(|c: char| c.is_whitespace()) {
            Some(index) => {
                let (before, after) = self.raw.split_at(index);
                self.raw = after;
                before
            }
            None => {
                let arg = self.raw;
                self.raw = "";
                arg
            }
        };

        Some(arg)
    }
}

pub struct CommandCollection {
    commands: HashMap<String, CommandBody>,
}

impl Default for CommandCollection {
    fn default() -> Self {
        let mut this = Self {
            commands: HashMap::new(),
        };

        this.register("quit".into(), commands::quit);
        this.register("edit".into(), commands::edit);
        this.register("close".into(), commands::close);
        this.register("write".into(), commands::write);
        this.register("write-all".into(), commands::write_all);

        this.register("nmap".into(), commands::nmap);
        this.register("smap".into(), commands::smap);
        this.register("imap".into(), commands::imap);

        this
    }
}

impl CommandCollection {
    pub fn register(&mut self, name: String, body: CommandBody) {
        self.commands.insert(name, body);
    }

    pub fn parse_and_execute(&self, ctx: CommandContext, command: &str) -> CommandResult {
        let command = command.trim();
        let name;
        let args;
        if let Some(index) = command.find(' ') {
            name = &command[..index];
            args = CommandArgs::new(&command[(index + 1)..]);
        } else {
            name = command;
            args = CommandArgs::new("");
        }

        if let Some(command) = self.commands.get(name) {
            command(ctx, args)
        } else {
            Err(format!("command '{}' not found", name))
        }
    }
}

mod helper {
    use super::*;

    pub fn new_buffer_from_content(
        ctx: &mut CommandContext,
        path: Option<PathBuf>,
        content: BufferContent,
    ) {
        ctx.operations.send_content(ctx.target_client, &content);
        ctx.operations
            .send(ctx.target_client, EditorOperation::Path(path.clone()));

        let buffer_handle = ctx.buffers.add(Buffer::new(path, content));
        let buffer_view = BufferView::new(ctx.target_client, buffer_handle);
        let buffer_view_handle = ctx.buffer_views.add(buffer_view);
        *ctx.current_buffer_view_handle = Some(buffer_view_handle);
    }

    pub fn new_buffer_from_file(ctx: &mut CommandContext, path: &Path) -> Result<(), String> {
        if let Some(buffer_handle) = ctx.buffers.find_with_path(path) {
            let mut iter = ctx
                .buffer_views
                .iter_with_handles()
                .filter_map(|(handle, view)| {
                    if view.buffer_handle == buffer_handle
                        && view.target_client == ctx.target_client
                    {
                        Some((handle, view))
                    } else {
                        None
                    }
                });

            let view = match iter.next() {
                Some((handle, view)) => {
                    *ctx.current_buffer_view_handle = Some(handle);
                    view
                }
                None => {
                    drop(iter);
                    let view = BufferView::new(ctx.target_client, buffer_handle);
                    let view_handle = ctx.buffer_views.add(view);
                    let view = ctx.buffer_views.get(&view_handle);
                    *ctx.current_buffer_view_handle = Some(view_handle);
                    view
                }
            };

            ctx.operations.send_content(
                ctx.target_client,
                &ctx.buffers.get(buffer_handle).unwrap().content,
            );
            ctx.operations
                .send(ctx.target_client, EditorOperation::Path(Some(path.into())));
            ctx.operations
                .send_cursors(ctx.target_client, &view.cursors);
        } else if path.to_str().map(|s| s.trim().len()).unwrap_or(0) > 0 {
            let content = match File::open(&path) {
                Ok(mut file) => {
                    let mut content = String::new();
                    match file.read_to_string(&mut content) {
                        Ok(_) => (),
                        Err(error) => {
                            return Err(format!(
                                "could not read contents from file {:?}: {:?}",
                                path, error
                            ))
                        }
                    }
                    BufferContent::from_str(&content[..])
                }
                Err(_) => BufferContent::from_str(""),
            };

            new_buffer_from_content(ctx, Some(path.into()), content);
        } else {
            return Err(format!("invalid path {:?}", path));
        }

        Ok(())
    }

    pub fn write_buffer_to_file(buffer: &Buffer, path: &Path) -> Result<(), String> {
        let mut file =
            File::create(path).map_err(|e| format!("could not create file {:?}: {:?}", path, e))?;

        buffer
            .content
            .write(&mut file)
            .map_err(|e| format!("could not write to file {:?}: {:?}", path, e))
    }
}

mod commands {
    use super::*;

    pub fn quit(_ctx: CommandContext, args: CommandArgs) -> CommandResult {
        args.assert_empty()?;
        Ok(CommandOperation::Quit)
    }

    pub fn edit(mut ctx: CommandContext, mut args: CommandArgs) -> CommandResult {
        let path = Path::new(args.next()?);
        args.assert_empty()?;
        helper::new_buffer_from_file(&mut ctx, path)?;
        Ok(CommandOperation::Complete)
    }

    pub fn close(ctx: CommandContext, args: CommandArgs) -> CommandResult {
        args.assert_empty()?;
        if let Some(handle) = ctx
            .current_buffer_view_handle
            .take()
            .map(|h| ctx.buffer_views.get(&h).buffer_handle)
        {
            for view in ctx.buffer_views.iter() {
                if view.buffer_handle == handle {
                    ctx.operations.send_empty_content(view.target_client);
                    ctx.operations
                        .send(view.target_client, EditorOperation::Path(None));
                }
            }
            ctx.buffer_views
                .remove_where(|view| view.buffer_handle == handle);
        }

        Ok(CommandOperation::Complete)
    }

    pub fn write(ctx: CommandContext, mut args: CommandArgs) -> CommandResult {
        let view_handle = ctx
            .current_buffer_view_handle
            .as_ref()
            .ok_or_else(|| String::from("no buffer opened"))?;

        let buffer_handle = ctx.buffer_views.get(view_handle).buffer_handle;
        let buffer = ctx
            .buffers
            .get_mut(buffer_handle)
            .ok_or_else(|| String::from("no buffer opened"))?;

        let path = args.try_next();
        args.assert_empty()?;
        match path {
            Some(path) => {
                let path = PathBuf::from(path);
                helper::write_buffer_to_file(buffer, &path)?;
                for view in ctx.buffer_views.iter() {
                    if view.buffer_handle == buffer_handle {
                        ctx.operations.send(
                            view.target_client,
                            EditorOperation::Path(Some(path.clone())),
                        );
                    }
                }
                buffer.path = Some(path.clone());
                Ok(CommandOperation::Complete)
            }
            None => {
                let path = buffer
                    .path
                    .as_ref()
                    .ok_or_else(|| String::from("buffer has no path"))?;
                helper::write_buffer_to_file(buffer, path)?;
                Ok(CommandOperation::Complete)
            }
        }
    }

    pub fn write_all(ctx: CommandContext, args: CommandArgs) -> CommandResult {
        args.assert_empty()?;
        for buffer in ctx.buffers.iter() {
            if let Some(ref path) = buffer.path {
                helper::write_buffer_to_file(buffer, path)?;
            }
        }

        Ok(CommandOperation::Complete)
    }

    pub fn nmap(ctx: CommandContext, args: CommandArgs) -> CommandResult {
        mode_map(ctx, args, Mode::Normal)
    }

    pub fn smap(ctx: CommandContext, args: CommandArgs) -> CommandResult {
        mode_map(ctx, args, Mode::Select)
    }

    pub fn imap(ctx: CommandContext, args: CommandArgs) -> CommandResult {
        mode_map(ctx, args, Mode::Insert)
    }

    fn mode_map(ctx: CommandContext, mut args: CommandArgs, mode: Mode) -> CommandResult {
        let from = args.next()?;
        let to = args.next()?;
        args.assert_empty()?;

        ctx.keymaps.parse_map(mode.discriminant(), from, to)?;
        Ok(CommandOperation::Complete)
    }
}
