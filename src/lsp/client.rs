use std::{
    fmt,
    fs::File,
    io,
    ops::Range,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    str::FromStr,
};

use crate::{
    buffer::{Buffer, BufferCapabilities, BufferContent, BufferHandle},
    buffer_position::{BufferPosition, BufferRange},
    client,
    command::parse_process_command,
    cursor::Cursor,
    editor::Editor,
    editor_utils::{MessageKind, StatusBar},
    events::{EditorEvent, EditorEventIter},
    glob::{Glob, InvalidGlobError},
    json::{
        FromJson, Json, JsonArray, JsonConvertError, JsonInteger, JsonObject, JsonString, JsonValue,
    },
    lsp::{
        capabilities,
        protocol::{
            self, DocumentEdit, DocumentLocation, DocumentPosition, DocumentRange, PendingRequest,
            PendingRequestColection, Protocol, ResponseError, ServerEvent, ServerNotification,
            ServerRequest, ServerResponse, Uri,
        },
    },
    platform::{Platform, PlatformRequest, ProcessHandle, ProcessTag},
};

#[derive(Default)]
struct GenericCapability(bool);
impl<'json> FromJson<'json> for GenericCapability {
    fn from_json(value: JsonValue, _: &'json Json) -> Result<Self, JsonConvertError> {
        match value {
            JsonValue::Null => Ok(Self(false)),
            JsonValue::Boolean(b) => Ok(Self(b)),
            JsonValue::Object(_) => Ok(Self(true)),
            _ => Err(JsonConvertError),
        }
    }
}
#[derive(Default)]
struct TriggerCharactersCapability {
    on: bool,
    trigger_characters: String,
}
impl<'json> FromJson<'json> for TriggerCharactersCapability {
    fn from_json(value: JsonValue, json: &'json Json) -> Result<Self, JsonConvertError> {
        match value {
            JsonValue::Null => Ok(Self {
                on: false,
                trigger_characters: String::new(),
            }),
            JsonValue::Object(options) => {
                let mut trigger_characters = String::new();
                for c in options.get("triggerCharacters".into(), json).elements(json) {
                    if let JsonValue::String(c) = c {
                        let c = c.as_str(json);
                        trigger_characters.push_str(c);
                    }
                }
                Ok(Self {
                    on: true,
                    trigger_characters,
                })
            }
            _ => Err(JsonConvertError),
        }
    }
}
#[derive(Default)]
struct RenameCapability {
    on: bool,
    prepare_provider: bool,
}
impl<'json> FromJson<'json> for RenameCapability {
    fn from_json(value: JsonValue, json: &'json Json) -> Result<Self, JsonConvertError> {
        match value {
            JsonValue::Null => Ok(Self {
                on: false,
                prepare_provider: false,
            }),
            JsonValue::Boolean(b) => Ok(Self {
                on: b,
                prepare_provider: false,
            }),
            JsonValue::Object(options) => Ok(Self {
                on: true,
                prepare_provider: matches!(
                    options.get("prepareProvider", &json),
                    JsonValue::Boolean(true)
                ),
            }),
            _ => Err(JsonConvertError),
        }
    }
}
enum TextDocumentSyncKind {
    None,
    Full,
    Incremental,
}
struct TextDocumentSyncCapability {
    open_close: bool,
    change: TextDocumentSyncKind,
    save: TextDocumentSyncKind,
}
impl Default for TextDocumentSyncCapability {
    fn default() -> Self {
        Self {
            open_close: false,
            change: TextDocumentSyncKind::None,
            save: TextDocumentSyncKind::None,
        }
    }
}
impl<'json> FromJson<'json> for TextDocumentSyncCapability {
    fn from_json(value: JsonValue, json: &'json Json) -> Result<Self, JsonConvertError> {
        match value {
            JsonValue::Integer(0) => Ok(Self {
                open_close: false,
                change: TextDocumentSyncKind::None,
                save: TextDocumentSyncKind::None,
            }),
            JsonValue::Integer(1) => Ok(Self {
                open_close: true,
                change: TextDocumentSyncKind::Full,
                save: TextDocumentSyncKind::Full,
            }),
            JsonValue::Integer(2) => Ok(Self {
                open_close: true,
                change: TextDocumentSyncKind::Incremental,
                save: TextDocumentSyncKind::Incremental,
            }),
            JsonValue::Object(options) => {
                let mut open_close = false;
                let mut change = TextDocumentSyncKind::None;
                let mut save = TextDocumentSyncKind::None;
                for (key, value) in options.members(json) {
                    match key {
                        "change" => {
                            change = match value {
                                JsonValue::Integer(0) => TextDocumentSyncKind::None,
                                JsonValue::Integer(1) => TextDocumentSyncKind::Full,
                                JsonValue::Integer(2) => TextDocumentSyncKind::Incremental,
                                _ => return Err(JsonConvertError),
                            }
                        }
                        "openClose" => {
                            open_close = match value {
                                JsonValue::Boolean(b) => b,
                                _ => return Err(JsonConvertError),
                            }
                        }
                        "save" => {
                            save = match value {
                                JsonValue::Boolean(false) => TextDocumentSyncKind::None,
                                JsonValue::Boolean(true) => TextDocumentSyncKind::Incremental,
                                JsonValue::Object(options) => {
                                    match options.get("includeText", json) {
                                        JsonValue::Boolean(true) => TextDocumentSyncKind::Full,
                                        _ => TextDocumentSyncKind::Incremental,
                                    }
                                }
                                _ => return Err(JsonConvertError),
                            }
                        }
                        _ => (),
                    }
                }
                Ok(Self {
                    open_close,
                    change,
                    save,
                })
            }
            _ => Err(JsonConvertError),
        }
    }
}

declare_json_object! {
    #[derive(Default)]
    struct ServerCapabilities {
        textDocumentSync: TextDocumentSyncCapability,
        completionProvider: TriggerCharactersCapability,
        hoverProvider: GenericCapability,
        signatureHelpProvider: TriggerCharactersCapability,
        declarationProvider: GenericCapability,
        definitionProvider: GenericCapability,
        implementationProvider: GenericCapability,
        referencesProvider: GenericCapability,
        documentSymbolProvider: GenericCapability,
        documentFormattingProvider: GenericCapability,
        renameProvider: RenameCapability,
        workspaceSymbolProvider: GenericCapability,
    }
}

// TODO: move to buffer.rs
pub struct Diagnostic {
    pub message: String,
    pub utf16_range: BufferRange,
}

struct BufferDiagnosticCollection {
    path: PathBuf,
    buffer_handle: Option<BufferHandle>,
    diagnostics: Vec<Diagnostic>,
    len: usize,
}
impl BufferDiagnosticCollection {
    pub fn add(&mut self, message: &str, range: BufferRange) {
        if self.len < self.diagnostics.len() {
            let diagnostic = &mut self.diagnostics[self.len];
            diagnostic.message.clear();
            diagnostic.message.push_str(message);
            diagnostic.utf16_range = range;
        } else {
            self.diagnostics.push(Diagnostic {
                message: message.into(),
                utf16_range: range,
            });
        }
        self.len += 1;
    }

    pub fn sort(&mut self) {
        self.diagnostics.sort_by_key(|d| d.utf16_range.from);
    }
}

fn are_same_path_with_root(root_a: &Path, a: &Path, b: &Path) -> bool {
    if a.is_absolute() {
        a.components().eq(b.components())
    } else {
        root_a.components().chain(a.components()).eq(b.components())
    }
}

struct VersionedBufferEdit {
    buffer_range: BufferRange,
    text_range: Range<usize>,
}
#[derive(Default)]
struct VersionedBuffer {
    version: usize,
    texts: String,
    pending_edits: Vec<VersionedBufferEdit>,
}
impl VersionedBuffer {
    pub fn flush(&mut self) {
        self.texts.clear();
        self.pending_edits.clear();
        self.version += 1;
    }

    pub fn dispose(&mut self) {
        self.flush();
        self.version = 1;
    }
}
#[derive(Default)]
struct VersionedBufferCollection {
    buffers: Vec<VersionedBuffer>,
}
impl VersionedBufferCollection {
    pub fn add_edit(&mut self, buffer_handle: BufferHandle, range: BufferRange, text: &str) {
        let index = buffer_handle.0 as usize;
        if index >= self.buffers.len() {
            self.buffers
                .resize_with(index + 1, VersionedBuffer::default);
        }
        let buffer = &mut self.buffers[index];
        let text_range_start = buffer.texts.len();
        buffer.texts.push_str(text);
        buffer.pending_edits.push(VersionedBufferEdit {
            buffer_range: range,
            text_range: text_range_start..buffer.texts.len(),
        });
    }

    pub fn dispose(&mut self, buffer_handle: BufferHandle) {
        if let Some(buffer) = self.buffers.get_mut(buffer_handle.0 as usize) {
            buffer.dispose();
        }
    }

    pub fn iter_pending_mut<'a>(
        &'a mut self,
    ) -> impl 'a + Iterator<Item = (BufferHandle, &'a mut VersionedBuffer)> {
        self.buffers
            .iter_mut()
            .enumerate()
            .filter(|(_, e)| !e.pending_edits.is_empty())
            .map(|(i, e)| (BufferHandle(i as _), e))
    }
}

#[derive(Default)]
pub struct DiagnosticCollection {
    buffer_diagnostics: Vec<BufferDiagnosticCollection>,
}
impl DiagnosticCollection {
    pub fn buffer_diagnostics(&self, buffer_handle: BufferHandle) -> &[Diagnostic] {
        for diagnostics in &self.buffer_diagnostics {
            if diagnostics.buffer_handle == Some(buffer_handle) {
                return &diagnostics.diagnostics[..diagnostics.len];
            }
        }
        &[]
    }

    fn path_diagnostics_mut(
        &mut self,
        editor: &Editor,
        path: &Path,
    ) -> &mut BufferDiagnosticCollection {
        let buffer_diagnostics = &mut self.buffer_diagnostics;
        for i in 0..buffer_diagnostics.len() {
            if buffer_diagnostics[i].path == path {
                let diagnostics = &mut buffer_diagnostics[i];
                diagnostics.len = 0;
                return diagnostics;
            }
        }

        let mut buffer_handle = None;
        for buffer in editor.buffers.iter() {
            if let Some(buffer_path) = buffer.path() {
                if are_same_path_with_root(&editor.current_directory, buffer_path, path) {
                    buffer_handle = Some(buffer.handle());
                    break;
                }
            }
        }

        let end_index = buffer_diagnostics.len();
        buffer_diagnostics.push(BufferDiagnosticCollection {
            path: path.into(),
            buffer_handle,
            diagnostics: Vec::new(),
            len: 0,
        });
        &mut buffer_diagnostics[end_index]
    }

    pub fn clear_empty(&mut self) {
        let buffer_diagnostics = &mut self.buffer_diagnostics;
        for i in (0..buffer_diagnostics.len()).rev() {
            if buffer_diagnostics[i].len == 0 {
                buffer_diagnostics.swap_remove(i);
            }
        }
    }

    pub fn iter<'a>(
        &'a self,
    ) -> impl DoubleEndedIterator<Item = (&'a Path, Option<BufferHandle>, &'a [Diagnostic])> {
        self.buffer_diagnostics
            .iter()
            .map(|d| (d.path.as_path(), d.buffer_handle, &d.diagnostics[..d.len]))
    }

    pub fn on_load_buffer(&mut self, editor: &Editor, buffer_handle: BufferHandle) {
        let buffer_path = match editor.buffers.get(buffer_handle).and_then(|b| b.path()) {
            Some(path) => path,
            None => return,
        };

        for diagnostics in &mut self.buffer_diagnostics {
            if let None = diagnostics.buffer_handle {
                if are_same_path_with_root(
                    &editor.current_directory,
                    buffer_path,
                    &diagnostics.path,
                ) {
                    diagnostics.buffer_handle = Some(buffer_handle);
                    return;
                }
            }
        }
    }

    pub fn on_save_buffer(&mut self, editor: &Editor, buffer_handle: BufferHandle) {
        let buffer_path = match editor.buffers.get(buffer_handle).and_then(|b| b.path()) {
            Some(path) => path,
            None => return,
        };

        for diagnostics in &mut self.buffer_diagnostics {
            if diagnostics.buffer_handle == Some(buffer_handle) {
                diagnostics.buffer_handle = None;
                if are_same_path_with_root(
                    &editor.current_directory,
                    buffer_path,
                    &diagnostics.path,
                ) {
                    diagnostics.buffer_handle = Some(buffer_handle);
                    return;
                }
            }
        }
    }

    pub fn on_close_buffer(&mut self, buffer_handle: BufferHandle) {
        for diagnostics in &mut self.buffer_diagnostics {
            if diagnostics.buffer_handle == Some(buffer_handle) {
                diagnostics.buffer_handle = None;
                return;
            }
        }
    }
}

#[derive(Default)]
pub struct ReferencesOptions {
    pub auto_close_buffer: bool,
    pub context_len: usize,
}

pub struct Client {
    protocol: Protocol,
    root: PathBuf,
    pending_requests: PendingRequestColection,

    initialized: bool,
    server_capabilities: ServerCapabilities,
    log_write_buf: Vec<u8>,
    log_buffer_handle: Option<BufferHandle>,
    document_selectors: Vec<Glob>,
    versioned_buffers: VersionedBufferCollection,
    diagnostics: DiagnosticCollection,

    references_options: ReferencesOptions,
    formatting_edits: Vec<(BufferRange, BufferRange)>,
}

impl Client {
    fn new(root: PathBuf, log_buffer_handle: Option<BufferHandle>) -> Self {
        Self {
            protocol: Protocol::new(),
            root,
            pending_requests: PendingRequestColection::default(),

            initialized: false,
            server_capabilities: ServerCapabilities::default(),

            log_write_buf: Vec::new(),
            log_buffer_handle,

            document_selectors: Vec::new(),
            versioned_buffers: VersionedBufferCollection::default(),
            diagnostics: DiagnosticCollection::default(),

            references_options: ReferencesOptions::default(),
            formatting_edits: Vec::new(),
        }
    }

    pub fn handles_path(&self, path: &[u8]) -> bool {
        if self.document_selectors.is_empty() {
            true
        } else {
            self.document_selectors.iter().any(|g| g.matches(path))
        }
    }

    pub fn diagnostics(&self) -> &DiagnosticCollection {
        &self.diagnostics
    }

    pub fn hover(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        json: &mut Json,
        buffer_handle: BufferHandle,
        position: BufferPosition,
    ) {
        if !self.server_capabilities.hoverProvider.0 {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).and_then(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, platform, editor, json);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, json);
        let position = DocumentPosition::from(position);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), json);
        params.set("position".into(), position.to_json_value(json), json);

        self.request(platform, json, "textDocument/hover", params, None, None);
    }

    pub fn signature_help(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        json: &mut Json,
        buffer_handle: BufferHandle,
        position: BufferPosition,
    ) {
        if !self.server_capabilities.signatureHelpProvider.on {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).and_then(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, platform, editor, json);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, json);
        let position = DocumentPosition::from(position);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), json);
        params.set("position".into(), position.to_json_value(json), json);

        self.request(
            platform,
            json,
            "textDocument/signatureHelp",
            params,
            None,
            None,
        );
    }

    pub fn definition(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        json: &mut Json,
        buffer_handle: BufferHandle,
        position: BufferPosition,
        client_handle: Option<client::ClientHandle>,
    ) {
        if !self.server_capabilities.definitionProvider.0 {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).and_then(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, platform, editor, json);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, json);
        let position = DocumentPosition::from(position);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), json);
        params.set("position".into(), position.to_json_value(json), json);

        self.request(
            platform,
            json,
            "textDocument/definition",
            params,
            client_handle,
            None,
        );
    }

    pub fn references(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        json: &mut Json,
        buffer_handle: BufferHandle,
        position: BufferPosition,
        options: ReferencesOptions,
        client_handle: Option<client::ClientHandle>,
    ) {
        if !self.server_capabilities.referencesProvider.0 {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).and_then(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, platform, editor, json);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, json);
        let position = DocumentPosition::from(position);

        let mut context = JsonObject::default();
        context.set("includeDeclaration".into(), true.into(), json);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), json);
        params.set("position".into(), position.to_json_value(json), json);
        params.set("context".into(), context.into(), json);

        self.references_options = options;

        self.request(
            platform,
            json,
            "textDocument/references",
            params,
            client_handle,
            None,
        );
    }

    // TODO: these requests
    pub fn rename() {
        // https://microsoft.github.io/language-server-protocol/specifications/specification-current/#textDocument_rename
    }
    pub fn code_action() {
        // https://microsoft.github.io/language-server-protocol/specifications/specification-current/#textDocument_codeAction
    }

    pub fn formatting(
        &mut self,
        editor: &Editor,
        platform: &mut Platform,
        json: &mut Json,
        buffer_handle: BufferHandle,
    ) {
        // https://microsoft.github.io/language-server-protocol/specifications/specification-current/#textDocument_formatting

        if !self.server_capabilities.documentFormattingProvider.0 {
            return;
        }

        let buffer_path = match editor.buffers.get(buffer_handle).and_then(Buffer::path) {
            Some(path) => path,
            None => return,
        };

        helper::send_pending_did_change(self, platform, editor, json);

        let text_document = helper::text_document_with_id(&self.root, buffer_path, json);
        let mut options = JsonObject::default();
        options.set(
            "tabSize".into(),
            JsonValue::Integer(editor.config.tab_size.get() as _),
            json,
        );
        options.set(
            "insertSpaces".into(),
            (!editor.config.indent_with_tabs).into(),
            json,
        );
        options.set("trimTrailingWhitespace".into(), true.into(), json);
        options.set("trimFinalNewlines".into(), true.into(), json);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), json);
        params.set("options".into(), options.into(), json);

        self.request(
            platform,
            json,
            "textDocument/formatting",
            params,
            None,
            Some(buffer_handle),
        );
    }

    fn write_to_log_buffer<F>(&mut self, writer: F)
    where
        F: FnOnce(&mut Vec<u8>),
    {
        if let Some(_) = self.log_buffer_handle {
            writer(&mut self.log_write_buf);
            self.log_write_buf.extend_from_slice(b"\n----\n\n");
        }
    }

    fn flush_log_buffer(&mut self, editor: &mut Editor) {
        let buffers = &mut editor.buffers;
        if let Some(buffer) = self.log_buffer_handle.and_then(|h| buffers.get_mut(h)) {
            let position = buffer.content().end();
            let text = String::from_utf8_lossy(&self.log_write_buf);
            buffer.insert_text(
                &mut editor.word_database,
                position,
                &text,
                &mut editor.events,
            );
            self.log_write_buf.clear();
        }
    }

    fn on_request(
        &mut self,
        editor: &mut Editor,
        platform: &mut Platform,
        clients: &mut client::ClientManager,
        json: &mut Json,
        request: ServerRequest,
    ) {
        macro_rules! deserialize {
            ($value:expr) => {
                match FromJson::from_json($value, &json) {
                    Ok(value) => value,
                    Err(_) => {
                        self.respond(
                            platform,
                            json,
                            JsonValue::Null,
                            Err(ResponseError::parse_error()),
                        );
                        return;
                    }
                }
            };
        }

        self.write_to_log_buffer(|buf| {
            use io::Write;
            let _ = write!(buf, "receive request\nid: ");
            json.write(buf, &request.id);
            let _ = write!(
                buf,
                "\nmethod: '{}'\nparams:\n",
                request.method.as_str(json)
            );
            json.write(buf, &request.params);
        });

        match request.method.as_str(&json) {
            "client/registerCapability" => {
                for registration in request.params.get("registrations", &json).elements(&json) {
                    declare_json_object! {
                        struct Registration {
                            method: JsonString,
                            registerOptions: JsonObject,
                        }
                    }

                    let registration: Registration = deserialize!(registration);
                    match registration.method.as_str(&json) {
                        "textDocument/didSave" => {
                            self.document_selectors.clear();
                            for filter in registration
                                .registerOptions
                                .get("documentSelector", &json)
                                .elements(&json)
                            {
                                declare_json_object! {
                                    struct Filter {
                                        pattern: Option<JsonString>,
                                    }
                                }
                                let filter: Filter = deserialize!(filter);
                                let pattern = match filter.pattern {
                                    Some(pattern) => pattern.as_str(&json),
                                    None => continue,
                                };
                                let mut glob = Glob::default();
                                if let Err(_) = glob.compile(pattern.as_bytes()) {
                                    self.document_selectors.clear();
                                    self.respond(
                                        platform,
                                        json,
                                        request.id,
                                        Err(ResponseError::parse_error()),
                                    );
                                    return;
                                }
                                self.document_selectors.push(glob);
                            }
                        }
                        _ => (),
                    }
                }
                self.respond(platform, json, request.id, Ok(JsonValue::Null));
            }
            "window/showMessage" => {
                fn parse_params(
                    params: JsonValue,
                    json: &Json,
                ) -> Result<(MessageKind, &str), JsonConvertError> {
                    let params = match params {
                        JsonValue::Object(object) => object,
                        _ => return Err(JsonConvertError),
                    };
                    let mut kind = MessageKind::Info;
                    let mut message = "";
                    for (key, value) in params.members(json) {
                        match key {
                            "type" => {
                                kind = match value {
                                    JsonValue::Integer(1) => MessageKind::Error,
                                    JsonValue::Integer(2..=4) => MessageKind::Info,
                                    _ => return Err(JsonConvertError),
                                }
                            }
                            "message" => {
                                message = match value {
                                    JsonValue::String(string) => string.as_str(json),
                                    _ => return Err(JsonConvertError),
                                }
                            }
                            _ => (),
                        }
                    }

                    Ok((kind, message))
                }

                let (kind, message) = match parse_params(request.params, json) {
                    Ok(params) => params,
                    Err(_) => {
                        self.respond(
                            platform,
                            json,
                            request.id,
                            Err(ResponseError::parse_error()),
                        );
                        return;
                    }
                };

                editor.status_bar.write(kind).str(message);
                self.respond(platform, json, request.id, Ok(JsonValue::Null));
            }
            "window/showDocument" => {
                declare_json_object! {
                    struct ShowDocumentParams {
                        uri: JsonString,
                        external: Option<bool>,
                        takeFocus: Option<bool>,
                        selection: Option<DocumentRange>,
                    }
                }

                let params: ShowDocumentParams = deserialize!(request.params);
                let path = match Uri::parse(&self.root, params.uri.as_str(json)) {
                    Some(Uri::AbsolutePath(path)) => path,
                    Some(Uri::RelativePath(_, path)) => path,
                    None => return,
                };

                let success = if let Some(true) = params.external {
                    false
                } else {
                    let mut closure = || {
                        let client_handle = clients.focused_client()?;
                        let client = clients.get_mut(client_handle)?;
                        let buffer_view_handle = editor
                            .buffer_views
                            .buffer_view_handle_from_path(
                                client_handle,
                                &mut editor.buffers,
                                &mut editor.word_database,
                                &self.root,
                                path,
                                &mut editor.events,
                            )
                            .ok()?;
                        if let Some(range) = params.selection {
                            let buffer_view = editor.buffer_views.get_mut(buffer_view_handle)?;
                            let mut cursors = buffer_view.cursors.mut_guard();
                            cursors.clear();
                            cursors.add(Cursor {
                                anchor: range.start.into(),
                                position: range.end.into(),
                            });
                        }
                        if let Some(true) = params.takeFocus {
                            client.set_buffer_view_handle(
                                Some(buffer_view_handle),
                                &mut editor.events,
                            );
                        }
                        Some(())
                    };
                    closure().is_some()
                };

                let mut result = JsonObject::default();
                result.set("success".into(), success.into(), json);
                self.respond(platform, json, request.id, Ok(result.into()));
            }
            _ => self.respond(
                platform,
                json,
                request.id,
                Err(ResponseError::method_not_found()),
            ),
        }
    }

    fn on_notification(
        &mut self,
        editor: &mut Editor,
        platform: &mut Platform,
        json: &mut Json,
        notification: ServerNotification,
    ) {
        macro_rules! deserialize {
            ($value:expr) => {
                match FromJson::from_json($value, &json) {
                    Ok(value) => value,
                    Err(_) => {
                        self.respond(
                            platform,
                            json,
                            JsonValue::Null,
                            Err(ResponseError::parse_error()),
                        );
                        return;
                    }
                }
            };
        }

        self.write_to_log_buffer(|buf| {
            use io::Write;
            let _ = write!(
                buf,
                "receive notification\nmethod: '{}'\nparams:\n",
                notification.method.as_str(json)
            );
            json.write(buf, &notification.params);
        });

        match notification.method.as_str(json) {
            "window/showMessage" => {
                let mut message_type: JsonInteger = 0;
                let mut message = JsonString::default();
                for (key, value) in notification.params.members(json) {
                    match key {
                        "type" => message_type = deserialize!(value),
                        "value" => message = deserialize!(value),
                        _ => (),
                    }
                }
                let message = message.as_str(json);
                match message_type {
                    1 => editor.status_bar.write(MessageKind::Error).str(message),
                    2 => editor
                        .status_bar
                        .write(MessageKind::Info)
                        .fmt(format_args!("warning: {}", message)),
                    3 => editor
                        .status_bar
                        .write(MessageKind::Info)
                        .fmt(format_args!("info: {}", message)),
                    4 => editor.status_bar.write(MessageKind::Info).str(message),
                    _ => (),
                }
            }
            "textDocument/publishDiagnostics" => {
                declare_json_object! {
                    struct Params {
                        uri: JsonString,
                        diagnostics: JsonArray,
                    }
                }

                let params: Params = deserialize!(notification.params);
                let uri = params.uri.as_str(json);
                let path = match Uri::parse(&self.root, uri) {
                    Some(Uri::AbsolutePath(path)) => path,
                    _ => return,
                };

                let diagnostics = self.diagnostics.path_diagnostics_mut(editor, path);
                for diagnostic in params.diagnostics.elements(json) {
                    declare_json_object! {
                        #[derive(Default)]
                        struct Position {
                            line: usize,
                            character: usize,
                        }
                    }
                    declare_json_object! {
                        #[derive(Default)]
                        struct Range {
                            start: Position,
                            end: Position,
                        }
                    }
                    declare_json_object! {
                        struct Diagnostic {
                            message: JsonString,
                            range: Range,
                        }
                    }

                    let diagnostic: Diagnostic = deserialize!(diagnostic);
                    let range = diagnostic.range;
                    let range = BufferRange::between(
                        BufferPosition::line_col(range.start.line, range.start.character),
                        BufferPosition::line_col(range.end.line, range.end.character),
                    );
                    diagnostics.add(diagnostic.message.as_str(json), range);
                }
                diagnostics.sort();
                self.diagnostics.clear_empty();
            }
            _ => (),
        }
    }

    fn on_response(
        &mut self,
        editor: &mut Editor,
        platform: &mut Platform,
        clients: &mut client::ClientManager,
        json: &mut Json,
        response: ServerResponse,
    ) {
        let request = match self.pending_requests.take(response.id) {
            Some(request) => request,
            None => return,
        };
        let method = request.method;

        macro_rules! deserialize {
            ($value:expr) => {
                match FromJson::from_json($value, json) {
                    Ok(value) => value,
                    Err(_) => {
                        self.respond(
                            platform,
                            json,
                            request.id.into(),
                            Err(ResponseError::parse_error()),
                        );
                        return;
                    }
                }
            };
        }

        self.write_to_log_buffer(|buf| {
            use io::Write;
            let _ = write!(
                buf,
                "receive response\nid: {}\nmethod: '{}'\n",
                response.id.0, method
            );
            match &response.result {
                Ok(result) => {
                    let _ = write!(buf, "result:\n");
                    json.write(buf, result);
                }
                Err(error) => {
                    let _ = write!(
                        buf,
                        "error_code: {}\nerror_message: '{}'\nerror_data:\n",
                        error.code,
                        error.message.as_str(json)
                    );
                    json.write(buf, &error.data);
                }
            }
        });

        let result = match response.result {
            Ok(result) => result,
            Err(error) => {
                helper::write_response_error(&mut editor.status_bar, method, error, json);
                return;
            }
        };

        match method {
            "initialize" => {
                self.server_capabilities = deserialize!(result.get("capabilities", json));
                self.initialized = true;
                self.notify(platform, json, "initialized", JsonObject::default());

                for buffer in editor.buffers.iter() {
                    helper::send_did_open(self, platform, editor, json, buffer.handle());
                }
            }
            "textDocument/hover" => {
                let contents = result.get("contents".into(), json);
                let info = helper::extract_markup_content(contents, json);
                editor.status_bar.write(MessageKind::Info).str(info);
            }
            "textDocument/signatureHelp" => {
                declare_json_object! {
                    struct SignatureHelp {
                        activeSignature: usize,
                        signatures: JsonArray,
                    }
                }
                declare_json_object! {
                    struct SignatureInformation {
                        label: JsonString,
                        documentation: JsonValue,
                    }
                }

                let signature_help: Option<SignatureHelp> = deserialize!(result);
                let signature = match signature_help
                    .and_then(|sh| sh.signatures.elements(json).nth(sh.activeSignature))
                {
                    Some(signature) => signature,
                    None => return,
                };
                let signature: SignatureInformation = deserialize!(signature);
                let label = signature.label.as_str(json);
                let documentation = helper::extract_markup_content(signature.documentation, json);

                if documentation.is_empty() {
                    editor.status_bar.write(MessageKind::Info).str(label);
                } else {
                    editor
                        .status_bar
                        .write(MessageKind::Info)
                        .fmt(format_args!("{}\n{}", documentation, label));
                }
            }
            "textDocument/definition" => {
                let location = match result {
                    JsonValue::Null => return,
                    JsonValue::Object(_) => result,
                    // TODO: use picker in this case?
                    JsonValue::Array(locations) => match locations.elements(json).next() {
                        Some(location) => location,
                        None => return,
                    },
                    _ => {
                        self.respond(
                            platform,
                            json,
                            request.id.into(),
                            Err(ResponseError::parse_error()),
                        );
                        return;
                    }
                };
                let location = match DocumentLocation::from_json(location, json) {
                    Ok(location) => location,
                    Err(_) => {
                        self.respond(
                            platform,
                            json,
                            request.id.into(),
                            Err(ResponseError::parse_error()),
                        );
                        return;
                    }
                };

                let client = match request.client_handle.and_then(|h| clients.get_mut(h)) {
                    Some(client) => client,
                    None => return,
                };
                let path = match Uri::parse(&self.root, location.uri.as_str(json)) {
                    Some(Uri::AbsolutePath(path)) => path,
                    Some(Uri::RelativePath(_, path)) => path,
                    None => return,
                };
                if let Ok(buffer_view_handle) = editor.buffer_views.buffer_view_handle_from_path(
                    client.handle(),
                    &mut editor.buffers,
                    &mut editor.word_database,
                    &self.root,
                    path,
                    &mut editor.events,
                ) {
                    if let Some(buffer_view) = editor.buffer_views.get_mut(buffer_view_handle) {
                        let position = location.range.start.into();
                        let mut cursors = buffer_view.cursors.mut_guard();
                        cursors.clear();
                        cursors.add(Cursor {
                            anchor: position,
                            position,
                        });
                    }
                    client.set_buffer_view_handle(Some(buffer_view_handle), &mut editor.events);
                }
            }
            "textDocument/references" => {
                let locations = match result {
                    JsonValue::Null => return,
                    JsonValue::Array(locations) => locations,
                    _ => {
                        self.respond(
                            platform,
                            json,
                            request.id.into(),
                            Err(ResponseError::parse_error()),
                        );
                        return;
                    }
                };

                let client = match request.client_handle.and_then(|h| clients.get_mut(h)) {
                    Some(client) => client,
                    None => return,
                };

                let mut buffer_name = editor.string_pool.acquire();
                for location in locations.clone().elements(json) {
                    let location = match DocumentLocation::from_json(location, json) {
                        Ok(location) => location,
                        Err(_) => continue,
                    };
                    let path = match Uri::parse(&self.root, location.uri.as_str(json)) {
                        Some(Uri::AbsolutePath(path)) => path,
                        Some(Uri::RelativePath(_, path)) => path,
                        _ => continue,
                    };
                    if let Some(buffer) = editor.buffers.find_with_path(&self.root, path) {
                        buffer
                            .content()
                            .append_range_text_to_string(location.range.into(), &mut buffer_name);
                        break;
                    }
                }
                if buffer_name.is_empty() {
                    buffer_name.push_str("lsp");
                }
                buffer_name.push_str(".refs");

                let buffer_view_handle = editor.buffer_views.buffer_view_handle_from_path(
                    client.handle(),
                    &mut editor.buffers,
                    &mut editor.word_database,
                    &self.root,
                    Path::new(&buffer_name),
                    &mut editor.events,
                );
                editor.string_pool.release(buffer_name);
                let buffer_view_handle = match buffer_view_handle {
                    Ok(handle) => handle,
                    Err(_) => return,
                };

                let mut context_buffer = BufferContent::new();

                let buffers = &mut editor.buffers;
                if let Some(buffer) = editor
                    .buffer_views
                    .get(buffer_view_handle)
                    .and_then(|v| buffers.get_mut(v.buffer_handle))
                {
                    buffer.capabilities = BufferCapabilities::log();
                    buffer.capabilities.auto_close = self.references_options.auto_close_buffer;

                    let mut position = BufferPosition::zero();
                    let range = BufferRange::between(position, buffer.content().end());
                    buffer.delete_range(&mut editor.word_database, range, &mut editor.events);

                    let mut text = editor.string_pool.acquire();
                    let mut last_path = "";
                    for location in locations.elements(json) {
                        let location = match DocumentLocation::from_json(location, json) {
                            Ok(location) => location,
                            Err(_) => {
                                self.respond(
                                    platform,
                                    json,
                                    request.id.into(),
                                    Err(ResponseError::parse_error()),
                                );
                                editor.string_pool.release(text);
                                return;
                            }
                        };

                        let path = match Uri::parse(&self.root, location.uri.as_str(json)) {
                            Some(Uri::AbsolutePath(path)) => path,
                            Some(Uri::RelativePath(_, path)) => path,
                            _ => continue,
                        };
                        let path = match path.to_str() {
                            Some(path) => path,
                            None => continue,
                        };

                        use fmt::Write;
                        let _ = write!(
                            text,
                            "{}:{},{}\n",
                            path,
                            location.range.start.line + 1,
                            location.range.start.character + 1
                        );

                        if self.references_options.context_len > 0 {
                            if last_path != path {
                                context_buffer.clear();
                                if let Ok(file) = File::open(path) {
                                    let mut reader = io::BufReader::new(file);
                                    let _ = context_buffer.read(&mut reader);
                                }
                            }

                            let surrounding_len = self.references_options.context_len - 1;
                            let start = (location.range.start.line as usize)
                                .saturating_sub(surrounding_len);
                            let end = location.range.end.line as usize + surrounding_len;
                            let len = end - start + 1;

                            for line in context_buffer
                                .lines()
                                .skip(start)
                                .take(len)
                                .skip_while(|l| l.as_str().is_empty())
                            {
                                text.push_str(line.as_str());
                                text.push('\n');
                            }
                            text.push('\n');
                        }

                        let range = buffer.insert_text(
                            &mut editor.word_database,
                            position,
                            &text,
                            &mut editor.events,
                        );
                        position = position.insert(range);
                        text.clear();

                        last_path = path;
                    }
                    editor.string_pool.release(text);
                }

                client.set_buffer_view_handle(Some(buffer_view_handle), &mut editor.events);
                editor.trigger_event_handlers(platform, clients, None);

                if let Some(buffer_view) = editor.buffer_views.get_mut(buffer_view_handle) {
                    let mut cursors = buffer_view.cursors.mut_guard();
                    cursors.clear();
                    cursors.add(Cursor {
                        anchor: BufferPosition::zero(),
                        position: BufferPosition::zero(),
                    });
                }
            }
            "textDocument/formatting" => {
                let edits = match result {
                    JsonValue::Null => return,
                    JsonValue::Array(edits) => edits,
                    _ => {
                        self.respond(
                            platform,
                            json,
                            request.id.into(),
                            Err(ResponseError::parse_error()),
                        );
                        return;
                    }
                };

                let buffers = &mut editor.buffers;
                let buffer = match request.buffer_handle.and_then(|h| buffers.get_mut(h)) {
                    Some(buffer) => buffer,
                    None => return,
                };

                buffer.commit_edits();

                self.formatting_edits.clear();
                for edit in edits.clone().elements(json) {
                    let edit = match DocumentEdit::from_json(edit, json) {
                        Ok(edit) => edit,
                        Err(_) => {
                            self.respond(
                                platform,
                                json,
                                request.id.into(),
                                Err(ResponseError::parse_error()),
                            );
                            return;
                        }
                    };

                    let mut delete_range: BufferRange = edit.range.into();
                    let text = edit.new_text.as_str(json);

                    for (d, i) in &self.formatting_edits {
                        delete_range.from = delete_range.from.delete(*d);
                        delete_range.to = delete_range.to.delete(*d);

                        delete_range.from = delete_range.from.insert(*i);
                        delete_range.to = delete_range.to.insert(*i);
                    }

                    buffer.delete_range(
                        &mut editor.word_database,
                        delete_range,
                        &mut editor.events,
                    );
                    let insert_range = buffer.insert_text(
                        &mut editor.word_database,
                        delete_range.from,
                        text,
                        &mut editor.events,
                    );

                    self.formatting_edits.push((delete_range, insert_range));
                }

                buffer.commit_edits();
            }
            _ => (),
        }
    }

    fn on_parse_error(&mut self, platform: &mut Platform, json: &mut Json, request_id: JsonValue) {
        self.write_to_log_buffer(|buf| {
            use io::Write;
            let _ = write!(buf, "send parse error\nrequest_id: ");
            json.write(buf, &request_id);
        });
        self.respond(
            platform,
            json,
            request_id,
            Err(ResponseError::parse_error()),
        )
    }

    fn on_editor_events(&mut self, editor: &Editor, platform: &mut Platform, json: &mut Json) {
        if !self.initialized {
            return;
        }

        let mut events = EditorEventIter::new();
        while let Some(event) = events.next(&editor.events) {
            match event {
                &EditorEvent::Idle => {
                    helper::send_pending_did_change(self, platform, editor, json);
                }
                &EditorEvent::BufferLoad { handle } => {
                    let handle = handle;
                    self.versioned_buffers.dispose(handle);
                    self.diagnostics.on_load_buffer(editor, handle);
                    helper::send_did_open(self, platform, editor, json, handle);
                }
                &EditorEvent::BufferInsertText {
                    handle,
                    range,
                    text,
                } => {
                    let text = text.as_str(&editor.events);
                    let range = BufferRange::between(range.from, range.from);
                    self.versioned_buffers.add_edit(handle, range, text);
                }
                &EditorEvent::BufferDeleteText { handle, range } => {
                    self.versioned_buffers.add_edit(handle, range, "");
                }
                &EditorEvent::BufferSave { handle, .. } => {
                    self.diagnostics.on_save_buffer(editor, handle);
                    helper::send_pending_did_change(self, platform, editor, json);
                    helper::send_did_save(self, platform, editor, json, handle);
                }
                &EditorEvent::BufferClose { handle } => {
                    if self.log_buffer_handle == Some(handle) {
                        self.log_buffer_handle = None;
                    }
                    self.versioned_buffers.dispose(handle);
                    self.diagnostics.on_close_buffer(handle);
                    helper::send_did_close(self, platform, editor, json, handle);
                }
                EditorEvent::ClientChangeBufferView { .. } => (),
            }
        }
    }

    fn request(
        &mut self,
        platform: &mut Platform,
        json: &mut Json,
        method: &'static str,
        params: JsonObject,
        client_handle: Option<client::ClientHandle>,
        buffer_handle: Option<BufferHandle>,
    ) {
        if !self.initialized {
            return;
        }

        let params = params.into();
        self.write_to_log_buffer(|buf| {
            use io::Write;
            let _ = write!(buf, "send request\nmethod: '{}'\nparams:\n", method);
            json.write(buf, &params);
        });
        let id = self.protocol.request(platform, json, method, params);
        self.pending_requests.add(PendingRequest {
            id,
            method,
            client_handle,
            buffer_handle,
        });
    }

    fn respond(
        &mut self,
        platform: &mut Platform,
        json: &mut Json,
        request_id: JsonValue,
        result: Result<JsonValue, ResponseError>,
    ) {
        self.write_to_log_buffer(|buf| {
            use io::Write;
            let _ = write!(buf, "send response\nid: ");
            json.write(buf, &request_id);
            match &result {
                Ok(result) => {
                    let _ = write!(buf, "\nresult:\n");
                    json.write(buf, result);
                }
                Err(error) => {
                    let _ = write!(
                        buf,
                        "\nerror.code: {}\nerror.message: {}\nerror.data:\n",
                        error.code,
                        error.message.as_str(json)
                    );
                    json.write(buf, &error.data);
                }
            }
        });
        self.protocol.respond(platform, json, request_id, result);
    }

    fn notify(
        &mut self,
        platform: &mut Platform,
        json: &mut Json,
        method: &'static str,
        params: JsonObject,
    ) {
        let params = params.into();
        self.write_to_log_buffer(|buf| {
            use io::Write;
            let _ = write!(buf, "send notification\nmethod: '{}'\nparams:\n", method);
            json.write(buf, &params);
        });
        self.protocol.notify(platform, json, method, params);
    }

    fn initialize(&mut self, platform: &mut Platform, json: &mut Json) {
        let mut params = JsonObject::default();
        params.set(
            "processId".into(),
            JsonValue::Integer(process::id() as _),
            json,
        );

        let mut client_info = JsonObject::default();
        client_info.set("name".into(), env!("CARGO_PKG_NAME").into(), json);
        client_info.set("name".into(), env!("CARGO_PKG_VERSION").into(), json);
        params.set("clientInfo".into(), client_info.into(), json);

        let root = json.fmt_string(format_args!("{}", Uri::AbsolutePath(&self.root)));
        params.set("rootUri".into(), root.into(), json);

        params.set(
            "capabilities".into(),
            capabilities::client_capabilities(json),
            json,
        );

        self.initialized = true;
        self.request(platform, json, "initialize", params, None, None);
        self.initialized = false;
    }
}

mod helper {
    use super::*;

    pub fn write_response_error(
        status_bar: &mut StatusBar,
        method: &str,
        error: ResponseError,
        json: &Json,
    ) {
        let error_message = error.message.as_str(json);
        status_bar.write(MessageKind::Error).fmt(format_args!(
            "[lsp error code {}] {}: '{}'",
            error.code, method, error_message
        ));
    }

    pub fn get_path_uri<'a>(current_directory: &'a Path, path: &'a Path) -> Uri<'a> {
        if path.is_absolute() {
            Uri::AbsolutePath(path)
        } else {
            Uri::RelativePath(current_directory, path)
        }
    }

    pub fn text_document_with_id(
        current_directory: &Path,
        path: &Path,
        json: &mut Json,
    ) -> JsonObject {
        let mut id = JsonObject::default();
        let uri = json.fmt_string(format_args!("{}", get_path_uri(current_directory, path)));
        id.set("uri".into(), uri.into(), json);
        id
    }

    pub fn extract_markup_content<'json>(content: JsonValue, json: &'json Json) -> &'json str {
        match content {
            JsonValue::String(s) => s.as_str(json),
            JsonValue::Object(o) => match o.get("value".into(), json) {
                JsonValue::String(s) => s.as_str(json),
                _ => "",
            },
            _ => "",
        }
    }

    pub fn send_did_open(
        client: &mut Client,
        platform: &mut Platform,
        editor: &Editor,
        json: &mut Json,
        buffer_handle: BufferHandle,
    ) {
        if !client.server_capabilities.textDocumentSync.open_close {
            return;
        }

        let buffer = match editor.buffers.get(buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };
        if !buffer.capabilities.can_save {
            return;
        }
        let buffer_path = match buffer.path() {
            Some(path) => path,
            None => return,
        };

        let mut text_document = text_document_with_id(&client.root, buffer_path, json);
        let language_id = json.create_string(protocol::path_to_language_id(buffer_path));
        text_document.set("languageId".into(), language_id.into(), json);
        text_document.set("version".into(), JsonValue::Integer(0), json);
        let text = json.fmt_string(format_args!("{}", buffer.content()));
        text_document.set("text".into(), text.into(), json);

        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), json);

        client.notify(platform, json, "textDocument/didOpen", params.into());
    }

    pub fn send_pending_did_change(
        client: &mut Client,
        platform: &mut Platform,
        editor: &Editor,
        json: &mut Json,
    ) {
        if let TextDocumentSyncKind::None = client.server_capabilities.textDocumentSync.change {
            return;
        }

        let mut versioned_buffers = std::mem::take(&mut client.versioned_buffers);
        for (buffer_handle, versioned_buffer) in versioned_buffers.iter_pending_mut() {
            let buffer = match editor.buffers.get(buffer_handle) {
                Some(buffer) => buffer,
                None => continue,
            };
            if !buffer.capabilities.can_save {
                continue;
            }
            let buffer_path = match buffer.path() {
                Some(path) => path,
                None => continue,
            };

            let mut text_document = text_document_with_id(&client.root, buffer_path, json);
            text_document.set(
                "version".into(),
                JsonValue::Integer(versioned_buffer.version as _),
                json,
            );

            let mut params = JsonObject::default();
            params.set("textDocument".into(), text_document.into(), json);

            let mut content_changes = JsonArray::default();
            match client.server_capabilities.textDocumentSync.save {
                TextDocumentSyncKind::None => (),
                TextDocumentSyncKind::Full => {
                    let text = json.fmt_string(format_args!("{}", buffer.content()));
                    let mut change_event = JsonObject::default();
                    change_event.set("text".into(), text.into(), json);
                    content_changes.push(change_event.into(), json);
                }
                TextDocumentSyncKind::Incremental => {
                    for edit in &versioned_buffer.pending_edits {
                        let mut change_event = JsonObject::default();

                        let edit_range = DocumentRange::from(edit.buffer_range).to_json_value(json);
                        change_event.set("range".into(), edit_range, json);

                        let text = &versioned_buffer.texts[edit.text_range.clone()];
                        let text = json.create_string(text);
                        change_event.set("text".into(), text.into(), json);

                        content_changes.push(change_event.into(), json);
                    }
                }
            }

            params.set("contentChanges".into(), content_changes.into(), json);

            versioned_buffer.flush();
            client.notify(platform, json, "textDocument/didChange", params.into());
        }
        std::mem::swap(&mut client.versioned_buffers, &mut versioned_buffers);
    }

    pub fn send_did_save(
        client: &mut Client,
        platform: &mut Platform,
        editor: &Editor,
        json: &mut Json,
        buffer_handle: BufferHandle,
    ) {
        if let TextDocumentSyncKind::None = client.server_capabilities.textDocumentSync.save {
            return;
        }

        let buffer = match editor.buffers.get(buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };
        if !buffer.capabilities.can_save {
            return;
        }
        let buffer_path = match buffer.path() {
            Some(path) => path,
            None => return,
        };

        let text_document = text_document_with_id(&client.root, buffer_path, json);
        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), json);

        if let TextDocumentSyncKind::Full = client.server_capabilities.textDocumentSync.save {
            let text = json.fmt_string(format_args!("{}", buffer.content()));
            params.set("text".into(), text.into(), json);
        }

        client.notify(platform, json, "textDocument/didSave", params.into())
    }

    pub fn send_did_close(
        client: &mut Client,
        platform: &mut Platform,
        editor: &Editor,
        json: &mut Json,
        buffer_handle: BufferHandle,
    ) {
        if !client.server_capabilities.textDocumentSync.open_close {
            return;
        }

        let buffer = match editor.buffers.get(buffer_handle) {
            Some(buffer) => buffer,
            None => return,
        };
        if !buffer.capabilities.can_save {
            return;
        }
        let buffer_path = match buffer.path() {
            Some(path) => path,
            None => return,
        };

        let text_document = text_document_with_id(&client.root, buffer_path, json);
        let mut params = JsonObject::default();
        params.set("textDocument".into(), text_document.into(), json);

        client.notify(platform, json, "textDocument/didClose", params.into());
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ClientHandle(u8);
impl fmt::Display for ClientHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for ClientHandle {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse() {
            Ok(i) => Ok(Self(i)),
            Err(_) => Err(()),
        }
    }
}

struct ClientManagerEntry {
    client: Client,
    json: Json,
}

struct ClientRecipe {
    glob: Glob,
    command: String,
    environment: String,
    root: PathBuf,
    log_buffer_name: String,
    running_client: Option<ClientHandle>,
}

pub struct ClientManager {
    entries: Vec<Option<ClientManagerEntry>>,
    recipes: Vec<ClientRecipe>,
}

impl ClientManager {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            recipes: Vec::new(),
        }
    }

    pub fn add_recipe(
        &mut self,
        glob: &[u8],
        command: &str,
        environment: &str,
        root: Option<&Path>,
        log_buffer_name: Option<&str>,
    ) -> Result<(), InvalidGlobError> {
        for recipe in &mut self.recipes {
            if recipe.command == command {
                recipe.glob.compile(glob)?;
                recipe.environment.clear();
                recipe.environment.push_str(environment);
                recipe.root.clear();
                if let Some(path) = root {
                    recipe.root.push(path);
                }
                recipe.log_buffer_name.clear();
                if let Some(name) = log_buffer_name {
                    recipe.log_buffer_name.push_str(name);
                }
                recipe.running_client = None;
                return Ok(());
            }
        }

        let mut recipe_glob = Glob::default();
        recipe_glob.compile(glob)?;
        self.recipes.push(ClientRecipe {
            glob: recipe_glob,
            command: command.into(),
            environment: environment.into(),
            root: root.unwrap_or(Path::new("")).into(),
            log_buffer_name: log_buffer_name.unwrap_or("").into(),
            running_client: None,
        });
        Ok(())
    }

    pub fn start(
        &mut self,
        platform: &mut Platform,
        mut command: Command,
        root: PathBuf,
        log_buffer_handle: Option<BufferHandle>,
    ) -> ClientHandle {
        let handle = self.find_free_slot();
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        platform.enqueue_request(PlatformRequest::SpawnProcess {
            tag: ProcessTag::Lsp(handle),
            command,
            buf_len: protocol::BUFFER_LEN,
        });
        self.entries[handle.0 as usize] = Some(ClientManagerEntry {
            client: Client::new(root, log_buffer_handle),
            json: Json::new(),
        });
        handle
    }

    pub fn stop(&mut self, platform: &mut Platform, handle: ClientHandle) {
        if let Some(entry) = &mut self.entries[handle.0 as usize] {
            let _ = entry
                .client
                .notify(platform, &mut entry.json, "exit", JsonObject::default());
            self.entries[handle.0 as usize] = None;
        }
    }

    pub fn stop_all(&mut self, platform: &mut Platform) {
        for i in 0..self.entries.len() {
            self.stop(platform, ClientHandle(i as _));
        }
    }

    pub fn access<A, R>(editor: &mut Editor, handle: ClientHandle, accessor: A) -> Option<R>
    where
        A: FnOnce(&mut Editor, &mut Client, &mut Json) -> R,
    {
        let mut entry = editor.lsp.entries[handle.0 as usize].take()?;
        let result = accessor(editor, &mut entry.client, &mut entry.json);
        editor.lsp.entries[handle.0 as usize] = Some(entry);
        Some(result)
    }

    pub fn clients(&self) -> impl DoubleEndedIterator<Item = &Client> {
        self.entries.iter().flat_map(|e| match e {
            Some(e) => Some(&e.client),
            None => None,
        })
    }

    pub fn client_with_handles(&self) -> impl Iterator<Item = (ClientHandle, &Client)> {
        self.entries.iter().enumerate().flat_map(|(i, e)| match e {
            Some(e) => Some((ClientHandle(i as _), &e.client)),
            None => None,
        })
    }

    pub fn on_process_spawned(
        editor: &mut Editor,
        platform: &mut Platform,
        handle: ClientHandle,
        process_handle: ProcessHandle,
    ) {
        if let Some(ref mut entry) = editor.lsp.entries[handle.0 as usize] {
            entry.client.protocol.set_process_handle(process_handle);
            entry.client.initialize(platform, &mut entry.json);
        }
    }

    pub fn on_process_output(
        editor: &mut Editor,
        platform: &mut Platform,
        clients: &mut client::ClientManager,
        handle: ClientHandle,
        bytes: &[u8],
    ) {
        let (mut client, mut json) = match editor.lsp.entries[handle.0 as usize].take() {
            Some(entry) => (entry.client, entry.json),
            None => return,
        };

        let mut events = client.protocol.parse_events(bytes);
        while let Some(event) = events.next(&mut client.protocol, &mut json) {
            match event {
                ServerEvent::Closed => editor.lsp.stop(platform, handle),
                ServerEvent::ParseError => {
                    client.on_parse_error(platform, &mut json, JsonValue::Null)
                }
                ServerEvent::Request(request) => {
                    client.on_request(editor, platform, clients, &mut json, request)
                }
                ServerEvent::Notification(notification) => {
                    client.on_notification(editor, platform, &mut json, notification)
                }
                ServerEvent::Response(response) => {
                    client.on_response(editor, platform, clients, &mut json, response)
                }
            }
            client.flush_log_buffer(editor);
        }
        events.finish(&mut client.protocol);

        editor.lsp.entries[handle.0 as usize] = Some(ClientManagerEntry { client, json });
    }

    pub fn on_process_exit(editor: &mut Editor, handle: ClientHandle) {
        editor.lsp.entries[handle.0 as usize] = None;

        for recipe in &mut editor.lsp.recipes {
            if recipe.running_client == Some(handle) {
                recipe.running_client = None;
            }
        }
    }

    pub fn on_editor_events(editor: &mut Editor, platform: &mut Platform) {
        let mut events = EditorEventIter::new();
        while let Some(event) = events.next(&editor.events) {
            if let &EditorEvent::BufferLoad { handle } = event {
                let buffer_path = match editor
                    .buffers
                    .get(handle)
                    .and_then(Buffer::path)
                    .and_then(Path::to_str)
                {
                    Some(path) => path,
                    None => continue,
                };
                let (index, recipe) = match editor
                    .lsp
                    .recipes
                    .iter_mut()
                    .enumerate()
                    .find(|(_, r)| r.glob.matches(buffer_path.as_bytes()))
                {
                    Some(recipe) => recipe,
                    None => continue,
                };
                if recipe.running_client.is_some() {
                    continue;
                }
                let command = match parse_process_command(&recipe.command, &recipe.environment) {
                    Ok(command) => command,
                    Err(error) => {
                        let error =
                            error.display(&recipe.command, None, &editor.commands, &editor.buffers);
                        editor
                            .status_bar
                            .write(MessageKind::Error)
                            .fmt(format_args!("{}", error));
                        continue;
                    }
                };
                let root = if recipe.root.as_os_str().is_empty() {
                    editor.current_directory.clone()
                } else {
                    recipe.root.clone()
                };

                let log_buffer_handle = if !recipe.log_buffer_name.is_empty() {
                    let mut buffer = editor.buffers.new();
                    buffer.capabilities = BufferCapabilities::log();
                    buffer.set_path(Some(Path::new(&recipe.log_buffer_name)));
                    Some(buffer.handle())
                } else {
                    None
                };

                let client_handle = editor.lsp.start(platform, command, root, log_buffer_handle);
                editor.lsp.recipes[index].running_client = Some(client_handle);
            }
        }

        for i in 0..editor.lsp.entries.len() {
            if let Some(mut entry) = editor.lsp.entries[i].take() {
                entry
                    .client
                    .on_editor_events(editor, platform, &mut entry.json);
                editor.lsp.entries[i] = Some(entry);
            }
        }
    }

    fn find_free_slot(&mut self) -> ClientHandle {
        for (i, slot) in self.entries.iter_mut().enumerate() {
            if let None = slot {
                return ClientHandle(i as _);
            }
        }
        let handle = ClientHandle(self.entries.len() as _);
        self.entries.push(None);
        handle
    }
}
