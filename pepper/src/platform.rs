use std::{
    io,
    mem::ManuallyDrop,
    process::{Command, Stdio},
};

use crate::{client::ClientHandle, editor_utils::parse_process_command, lsp, plugin::PluginHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    None,
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    Delete,
    F(u8),
    Char(char),
    Ctrl(char),
    Alt(char),
    Esc,
}

pub enum PlatformEvent {
    Idle,
    ConnectionOpen {
        handle: ClientHandle,
    },
    ConnectionClose {
        handle: ClientHandle,
    },
    ConnectionOutput {
        handle: ClientHandle,
        buf: PooledBuf,
    },
    ProcessSpawned {
        tag: ProcessTag,
        handle: ProcessHandle,
    },
    ProcessOutput {
        tag: ProcessTag,
        buf: PooledBuf,
    },
    ProcessExit {
        tag: ProcessTag,
    },
}

pub enum PlatformRequest {
    Quit,
    Redraw,
    WriteToClient {
        handle: ClientHandle,
        buf: PooledBuf,
    },
    CloseClient {
        handle: ClientHandle,
    },
    SpawnProcess {
        tag: ProcessTag,
        command: Command,
        buf_len: usize,
    },
    WriteToProcess {
        handle: ProcessHandle,
        buf: PooledBuf,
    },
    CloseProcessInput {
        handle: ProcessHandle,
    },
    KillProcess {
        handle: ProcessHandle,
    },
}

#[derive(Clone, Copy)]
pub struct ProcessIndex(pub u32);

#[derive(Clone, Copy)]
pub enum ProcessTag {
    Buffer(ProcessIndex),
    FindFiles,
    Plugin {
        plugin_handle: PluginHandle,
        index: ProcessIndex,
    },
    Lsp(lsp::ClientHandle),
}

#[derive(Clone, Copy)]
pub struct ProcessHandle(pub u8);

#[derive(Default)]
pub struct PlatformRequestCollection {
    pending_requests: Vec<PlatformRequest>,
}
impl PlatformRequestCollection {
    pub fn enqueue(&mut self, request: PlatformRequest) {
        self.pending_requests.push(request);
    }

    pub fn drain(&mut self) -> impl '_ + Iterator<Item = PlatformRequest> {
        self.pending_requests.drain(..)
    }
}

#[derive(Default)]
pub struct Platform {
    pub requests: PlatformRequestCollection,

    read_from_clipboard_fn: Option<fn(&mut String)>,
    write_to_clipboard_fn: Option<fn(&str)>,

    pub buf_pool: BufPool,

    internal_clipboard: String,
    pub copy_command: String,
    pub paste_command: String,
}
impl Platform {
    pub fn set_clipboard_api(
        &mut self,
        read_from_clipboard_fn: fn(&mut String),
        write_to_clipboard_fn: fn(&str),
    ) {
        self.read_from_clipboard_fn = Some(read_from_clipboard_fn);
        self.write_to_clipboard_fn = Some(write_to_clipboard_fn);
    }

    pub fn read_from_clipboard(&self, text: &mut String) {
        if let Some(mut command) = parse_process_command(&self.paste_command) {
            command.stdin(Stdio::null());
            command.stdout(Stdio::piped());
            command.stderr(Stdio::null());
            if let Ok(output) = command.output() {
                if let Ok(output) = String::from_utf8(output.stdout) {
                    text.clear();
                    text.push_str(&output);
                }
            }
        } else if let Some(read_from_clipboard) = self.read_from_clipboard_fn {
            read_from_clipboard(text);
        } else {
            text.push_str(&self.internal_clipboard);
        }
    }

    pub fn write_to_clipboard(&mut self, text: &str) {
        if let Some(mut command) = parse_process_command(&self.copy_command) {
            command.stdin(Stdio::piped());
            command.stdout(Stdio::null());
            command.stderr(Stdio::null());
            if let Ok(mut child) = command.spawn() {
                if let Some(mut stdin) = child.stdin.take() {
                    use io::Write;
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
            }
        } else if let Some(write_to_clipboard) = self.write_to_clipboard_fn {
            write_to_clipboard(text);
        } else {
            self.internal_clipboard.clear();
            self.internal_clipboard.push_str(text);
        }
    }
}

pub struct PooledBuf(Vec<u8>);
impl PooledBuf {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn write(&mut self) -> &mut Vec<u8> {
        let buf = &mut self.0;
        buf.clear();
        buf
    }

    pub fn write_with_len(&mut self, len: usize) -> &mut Vec<u8> {
        let buf = &mut self.0;
        buf.resize(len, 0);
        buf
    }
}
impl Drop for PooledBuf {
    fn drop(&mut self) {
        panic!("buf was dropped outside of a pool");
    }
}

#[derive(Default)]
pub struct BufPool {
    pool: Vec<ManuallyDrop<PooledBuf>>,
}
impl BufPool {
    pub fn acquire(&mut self) -> PooledBuf {
        match self.pool.pop() {
            Some(buf) => ManuallyDrop::into_inner(buf),
            None => PooledBuf(Vec::new()),
        }
    }

    pub fn release(&mut self, buf: PooledBuf) {
        self.pool.push(ManuallyDrop::new(buf));
    }
}
