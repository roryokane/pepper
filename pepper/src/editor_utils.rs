use std::{fmt, process::Command};

use crate::{
    client::ClientManager,
    command::{CommandManager, CommandTokenizer},
    editor::{BufferedKeys, Editor, EditorControlFlow, KeysIterator},
    platform::{Key, Platform},
    word_database::{WordIter, WordKind},
};

#[derive(Clone, Copy)]
pub enum ReadLinePoll {
    Pending,
    Submitted,
    Canceled,
}

#[derive(Default)]
pub struct ReadLine {
    prompt: String,
    input: String,
}
impl ReadLine {
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn set_prompt(&mut self, prompt: &str) {
        self.prompt.clear();
        self.prompt.push_str(prompt);
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn input_mut(&mut self) -> &mut String {
        &mut self.input
    }

    pub fn poll(
        &mut self,
        platform: &mut Platform,
        string_pool: &mut StringPool,
        buffered_keys: &BufferedKeys,
        keys_iter: &mut KeysIterator,
    ) -> ReadLinePoll {
        match keys_iter.next(buffered_keys) {
            Key::Esc | Key::Ctrl('c') => ReadLinePoll::Canceled,
            Key::Enter | Key::Ctrl('m') => ReadLinePoll::Submitted,
            Key::Home | Key::Ctrl('u') => {
                self.input.clear();
                ReadLinePoll::Pending
            }
            Key::Ctrl('w') => {
                let mut words = WordIter(&self.input);
                (&mut words)
                    .filter(|w| w.kind == WordKind::Identifier)
                    .next_back();
                let len = words.0.len();
                self.input.truncate(len);
                ReadLinePoll::Pending
            }
            Key::Backspace | Key::Ctrl('h') => {
                if let Some((last_char_index, _)) = self.input.char_indices().next_back() {
                    self.input.truncate(last_char_index);
                }
                ReadLinePoll::Pending
            }
            Key::Ctrl('y') => {
                let mut text = string_pool.acquire();
                platform.read_from_clipboard(&mut text);
                self.input.push_str(&text);
                string_pool.release(text);
                ReadLinePoll::Pending
            }
            Key::Char(c) => {
                self.input.push(c);
                ReadLinePoll::Pending
            }
            _ => ReadLinePoll::Pending,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MessageKind {
    Info,
    Error,
}

pub struct StatusBar {
    kind: MessageKind,
    message: String,
}
impl StatusBar {
    pub fn new() -> Self {
        Self {
            kind: MessageKind::Info,
            message: String::new(),
        }
    }

    pub fn message(&self) -> (MessageKind, &str) {
        (self.kind, &self.message)
    }

    pub fn clear(&mut self) {
        self.message.clear();
    }

    pub fn write(&mut self, kind: MessageKind) -> EditorOutputWrite {
        self.kind = kind;
        self.message.clear();
        EditorOutputWrite(&mut self.message)
    }
}
pub struct EditorOutputWrite<'a>(&'a mut String);
impl<'a> EditorOutputWrite<'a> {
    pub fn str(&mut self, message: &str) {
        self.0.push_str(message);
    }

    pub fn fmt(&mut self, args: fmt::Arguments) {
        let _ = fmt::write(&mut self.0, args);
    }
}

#[derive(Default)]
pub struct StringPool {
    pool: Vec<String>,
}
impl StringPool {
    pub fn acquire(&mut self) -> String {
        match self.pool.pop() {
            Some(s) => s,
            None => String::new(),
        }
    }

    pub fn acquire_with(&mut self, value: &str) -> String {
        match self.pool.pop() {
            Some(mut s) => {
                s.push_str(value);
                s
            }
            None => String::from(value),
        }
    }

    pub fn release(&mut self, mut s: String) {
        s.clear();
        self.pool.push(s);
    }
}

// FNV-1a : https://en.wikipedia.org/wiki/Fowler–Noll–Vo_hash_function
// TODO: will this still be a good hash if we hash 8 bytes at a time and then combine them at the end?
// or should we just jump directly to a more complex hash that is simd-friendly?
pub const fn hash_bytes(mut bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    while let [b, rest @ ..] = bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        bytes = rest;
    }
    hash
}

// extracted from str::is_char_boundary(&self, index: usize) -> bool
// https://doc.rust-lang.org/src/core/str/mod.rs.html#193
pub const fn is_char_boundary(b: u8) -> bool {
    (b as i8) >= -0x40
}

#[derive(Default)]
pub struct ResidualStrBytes {
    bytes: [u8; std::mem::size_of::<char>()],
    len: u8,
}
impl ResidualStrBytes {
    pub fn receive_bytes<'a>(
        &mut self,
        buf: &'a mut [u8; std::mem::size_of::<char>()],
        mut bytes: &'a [u8],
    ) -> [&'a str; 2] {
        loop {
            if bytes.is_empty() {
                break;
            }

            let b = bytes[0];
            if is_char_boundary(b) {
                break;
            }

            if self.len == self.bytes.len() as _ {
                self.len = 0;
                break;
            }

            self.bytes[self.len as usize] = bytes[0];
            self.len += 1;
            bytes = &bytes[1..];
        }

        *buf = self.bytes;
        let before = &buf[..self.len as usize];
        self.len = 0;

        let mut len = bytes.len();
        loop {
            if len == 0 {
                break;
            }
            len -= 1;
            if is_char_boundary(bytes[len]) {
                break;
            }
        }

        let (after, rest) = bytes.split_at(len);
        if self.bytes.len() < rest.len() {
            return ["", ""];
        }

        self.len = rest.len() as _;
        self.bytes[..self.len as usize].copy_from_slice(rest);

        let before = std::str::from_utf8(before).unwrap_or("");
        let after = std::str::from_utf8(after).unwrap_or("");

        [before, after]
    }
}

pub fn parse_process_command(command: &str) -> Option<Command> {
    let mut tokenizer = CommandTokenizer(command);
    let name = tokenizer.next()?;
    let mut command = Command::new(name);
    for arg in tokenizer {
        command.arg(arg);
    }
    Some(command)
}

pub fn load_config(
    editor: &mut Editor,
    platform: &mut Platform,
    clients: &mut ClientManager,
    config_name: &str,
    config_content: &str,
) -> EditorControlFlow {
    for (line_index, line) in config_content.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut command = editor.string_pool.acquire_with(line);
        let result = CommandManager::try_eval(editor, platform, clients, None, &mut command);
        editor.string_pool.release(command);

        match result {
            Ok(flow) => match flow {
                EditorControlFlow::Continue => (),
                _ => return flow,
            },
            Err(error) => {
                editor
                    .status_bar
                    .write(MessageKind::Error)
                    .fmt(format_args!(
                        "{}:{}\n{}\n{}",
                        config_name,
                        line_index + 1,
                        line,
                        error
                    ));
                break;
            }
        }
    }

    EditorControlFlow::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_char_boundary_test() {
        let bytes = "áé".as_bytes();
        assert_eq!(4, bytes.len());
        assert!(is_char_boundary(bytes[0]));
        assert!(!is_char_boundary(bytes[1]));
        assert!(is_char_boundary(bytes[2]));
        assert!(!is_char_boundary(bytes[3]));
    }

    #[test]
    fn residual_str_bytes() {
        let message = "abcdef".as_bytes();
        let mut residue = ResidualStrBytes::default();
        assert_eq!(
            ["", "ab"],
            residue.receive_bytes(&mut Default::default(), &message[..3])
        );
        assert_eq!(
            ["c", "de"],
            residue.receive_bytes(&mut Default::default(), &message[3..])
        );
        assert_eq!(
            ["f", ""],
            residue.receive_bytes(&mut Default::default(), &message[6..])
        );
        assert_eq!(
            ["", ""],
            residue.receive_bytes(&mut Default::default(), &[])
        );

        let message1 = "abcdef".as_bytes();
        let message2 = "123456".as_bytes();
        let mut residue = ResidualStrBytes::default();
        assert_eq!(
            ["", "abcde"],
            residue.receive_bytes(&mut Default::default(), &message1)
        );
        assert_eq!(
            ["f", "12345"],
            residue.receive_bytes(&mut Default::default(), &message2)
        );
        assert_eq!(
            ["6", ""],
            residue.receive_bytes(&mut Default::default(), &[])
        );
        assert_eq!(
            ["", ""],
            residue.receive_bytes(&mut Default::default(), &[])
        );

        let message = "áéíóú".as_bytes();
        assert_eq!(10, message.len());
        let mut residue = ResidualStrBytes::default();
        assert_eq!(
            ["", "á"],
            residue.receive_bytes(&mut Default::default(), &message[..3])
        );
        assert_eq!(
            ["é", ""],
            residue.receive_bytes(&mut Default::default(), &message[3..5])
        );
        assert_eq!(
            ["í", ""],
            residue.receive_bytes(&mut Default::default(), &message[5..8])
        );
        assert_eq!(
            ["ó", ""],
            residue.receive_bytes(&mut Default::default(), &message[8..])
        );
        assert_eq!(
            ["ú", ""],
            residue.receive_bytes(&mut Default::default(), &message[10..])
        );
        assert_eq!(
            ["", ""],
            residue.receive_bytes(&mut Default::default(), &[])
        );
    }
}