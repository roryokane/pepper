use std::path::Path;

use crate::{
    buffer::BufferCollection,
    buffer_view::BufferViewCollection,
    client::{ClientCollection, ClientTargetMap, TargetClient},
    client_event::{ClientEvent, Key},
    config::Config,
    connection::ConnectionWithClientHandle,
    cursor::Cursor,
    keymap::{KeyMapCollection, MatchResult},
    mode::{Mode, ModeContext, ModeOperation},
    script::{ScriptContext, ScriptEngine},
    select::SelectEntryCollection,
};

#[derive(Clone, Copy)]
pub enum EditorLoop {
    Quit,
    QuitAll,
    Continue,
}

impl EditorLoop {
    pub fn is_quit(self) -> bool {
        matches!(self, EditorLoop::Quit | EditorLoop::QuitAll)
    }
}

pub struct KeysIterator<'a> {
    keys: &'a [Key],
    index: usize,
}

impl<'a> KeysIterator<'a> {
    fn new(keys: &'a [Key]) -> Self {
        Self { keys, index: 0 }
    }

    pub fn next(&mut self) -> Key {
        if self.index < self.keys.len() {
            let next = self.keys[self.index];
            self.index += 1;
            next
        } else {
            Key::None
        }
    }

    pub fn put_back(&mut self) {
        if self.index > 0 {
            self.index -= 1;
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum StatusMessageKind {
    Info,
    Error,
}

pub struct Editor {
    pub config: Config,
    pub mode: Mode,

    pub buffered_keys: Vec<Key>,
    pub input: String,
    pub selects: SelectEntryCollection,

    pub focused_client: TargetClient,
    pub status_message: String,
    pub status_message_kind: StatusMessageKind,

    pub buffers: BufferCollection,
    pub buffer_views: BufferViewCollection,

    keymaps: KeyMapCollection,
    scripts: ScriptEngine,

    client_target_map: ClientTargetMap,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            config: Config::default(),
            mode: Mode::default(),
            keymaps: KeyMapCollection::default(),
            buffered_keys: Vec::new(),
            input: String::new(),
            selects: SelectEntryCollection::default(),
            scripts: ScriptEngine::new(),

            buffers: Default::default(),
            buffer_views: BufferViewCollection::default(),

            focused_client: TargetClient::Local,
            client_target_map: ClientTargetMap::default(),

            status_message: String::new(),
            status_message_kind: StatusMessageKind::Info,
        }
    }

    pub fn status_message(&mut self, kind: StatusMessageKind, message: &str) {
        self.status_message_kind = kind;
        self.status_message.clear();
        self.status_message.push_str(message);
    }

    pub fn load_config(&mut self, clients: &mut ClientCollection, path: &Path) {
        let mut editor_loop = EditorLoop::Continue;
        let ctx = ScriptContext {
            editor_loop: &mut editor_loop,
            target_client: TargetClient::Local,

            config: &mut self.config,
            keymaps: &mut self.keymaps,
            buffers: &mut self.buffers,
            buffer_views: &mut self.buffer_views,
            clients,

            status_message_kind: &mut self.status_message_kind,
            status_message: &mut self.status_message,
        };

        if let Err(e) = self.scripts.eval_entry_file(ctx, path) {
            let message = e.to_string();
            self.status_message(StatusMessageKind::Error, &message);
        }
    }

    pub fn on_client_joined(
        &mut self,
        clients: &mut ClientCollection,
        client_handle: ConnectionWithClientHandle,
    ) {
        clients.on_client_joined(client_handle);
        self.client_target_map.on_client_joined(client_handle);

        let target_client = TargetClient::Remote(client_handle);

        let buffer_view_handle = clients
            .get(self.focused_client)
            .and_then(|c| c.current_buffer_view_handle)
            .and_then(|h| self.buffer_views.get(h))
            .map(|v| v.clone_with_target_client(target_client))
            .map(|b| self.buffer_views.add(b));

        if let Some(client) = clients.get_mut(self.focused_client) {
            client.current_buffer_view_handle = buffer_view_handle;
        }

        self.buffered_keys.clear();
    }

    pub fn on_client_left(
        &mut self,
        clients: &mut ClientCollection,
        client_handle: ConnectionWithClientHandle,
    ) {
        clients.on_client_left(client_handle);
        self.client_target_map.on_client_left(client_handle);

        if self.focused_client == TargetClient::Remote(client_handle) {
            self.focused_client = TargetClient::Local;
            self.mode = Mode::default();
            self.buffered_keys.clear();
            self.input.clear();
        }
    }

    pub fn on_event(
        &mut self,
        clients: &mut ClientCollection,
        target_client: TargetClient,
        event: ClientEvent,
    ) -> EditorLoop {
        let result = match event {
            ClientEvent::AsFocusedClient => {
                self.client_target_map
                    .map(target_client, self.focused_client);
                EditorLoop::Continue
            }
            ClientEvent::AsClient(index) => {
                self.client_target_map
                    .map(target_client, TargetClient::from_index(index));
                EditorLoop::Continue
            }
            ClientEvent::OpenFile(path) => {
                let target_client = self.client_target_map.get(target_client);

                let path = Path::new(path);
                match self.buffer_views.new_buffer_from_file(
                    &mut self.buffers,
                    &self.config.syntaxes,
                    target_client,
                    path,
                ) {
                    Ok(buffer_view_handle) => {
                        if let Some(client) = clients.get_mut(target_client) {
                            client.current_buffer_view_handle = Some(buffer_view_handle);
                        }
                    }
                    Err(error) => self.status_message(StatusMessageKind::Error, &error),
                }

                EditorLoop::Continue
            }
            ClientEvent::Key(key) => {
                let target_client = self.client_target_map.get(target_client);

                if target_client != self.focused_client {
                    self.focused_client = target_client;
                    self.buffered_keys.clear();
                }

                self.buffered_keys.push(key);

                match self
                    .keymaps
                    .matches(self.mode.discriminant(), &self.buffered_keys[..])
                {
                    MatchResult::None => (),
                    MatchResult::Prefix => return EditorLoop::Continue,
                    MatchResult::ReplaceWith(replaced_keys) => {
                        self.buffered_keys.clear();
                        self.buffered_keys.extend_from_slice(replaced_keys);
                    }
                }

                let mut keys = KeysIterator::new(&self.buffered_keys);
                let result = loop {
                    if keys.index >= self.buffered_keys.len() {
                        break EditorLoop::Continue;
                    }

                    let mut mode_context = ModeContext {
                        target_client,
                        config: &mut self.config,
                        keymaps: &mut self.keymaps,
                        scripts: &mut self.scripts,
                        buffers: &mut self.buffers,
                        buffer_views: &mut self.buffer_views,
                        clients,
                        input: &mut self.input,
                        status_message_kind: &mut self.status_message_kind,
                        status_message: &mut self.status_message,
                    };

                    match self.mode.on_event(&mut mode_context, &mut keys) {
                        ModeOperation::Pending => return EditorLoop::Continue,
                        ModeOperation::None => (),
                        ModeOperation::Quit => {
                            self.buffered_keys.clear();
                            return EditorLoop::Quit;
                        }
                        ModeOperation::QuitAll => {
                            self.buffered_keys.clear();
                            return EditorLoop::QuitAll;
                        }
                        ModeOperation::EnterMode(next_mode) => {
                            self.mode = next_mode.clone();
                            self.mode.on_enter(&mut mode_context);
                        }
                    }
                };

                self.buffered_keys.clear();
                result
            }
            ClientEvent::Resize(width, height) => {
                let target_client = self.client_target_map.get(target_client);
                if let Some(client) = clients.get_mut(target_client) {
                    client.width = width;
                    client.height = height;
                }
                EditorLoop::Continue
            }
        };

        for c in clients.clients() {
            let main_cursor = c
                .client
                .current_buffer_view_handle
                .and_then(|h| self.buffer_views.get(h))
                .map(|v| v.cursors.main_cursor().clone())
                .unwrap_or(Cursor::default());
            c.client
                .scroll(self.focused_client == c.target, main_cursor, &self.selects);
        }

        result
    }
}
