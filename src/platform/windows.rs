use std::{
    os::windows::io::AsRawHandle,
    process::{Child, Command, Stdio},
    time::Duration,
};

use winapi::{
    shared::{
        minwindef::{BOOL, DWORD, FALSE, TRUE},
        ntdef::NULL,
        winerror::{ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_PIPE_CONNECTED, WAIT_TIMEOUT},
    },
    um::{
        consoleapi::{GetConsoleMode, ReadConsoleInputW, SetConsoleCtrlHandler, SetConsoleMode},
        errhandlingapi::GetLastError,
        fileapi::{CreateFileW, FindFirstFileW, ReadFile, WriteFile, OPEN_EXISTING},
        handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
        ioapiset::GetOverlappedResult,
        minwinbase::OVERLAPPED,
        namedpipeapi::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, SetNamedPipeHandleState,
        },
        processenv::GetStdHandle,
        synchapi::{CreateEventW, SetEvent, WaitForMultipleObjects},
        winbase::{
            FILE_FLAG_OVERLAPPED, INFINITE, PIPE_ACCESS_DUPLEX, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
            PIPE_UNLIMITED_INSTANCES, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, WAIT_ABANDONED_0,
            WAIT_OBJECT_0,
        },
        wincon::{
            ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
        },
        wincontypes::{
            INPUT_RECORD, KEY_EVENT, LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, RIGHT_ALT_PRESSED,
            RIGHT_CTRL_PRESSED, SHIFT_PRESSED, WINDOW_BUFFER_SIZE_EVENT,
        },
        winnt::{GENERIC_READ, GENERIC_WRITE, HANDLE, MAXIMUM_WAIT_OBJECTS},
        winuser::{
            VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F24, VK_HOME, VK_LEFT,
            VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
        },
    },
};

use crate::platform::{Key, Platform};

pub fn run() {
    unsafe { run_unsafe() }
}

unsafe fn run_unsafe() {
    unsafe extern "system" fn ctrl_handler(_ctrl_type: DWORD) -> BOOL {
        FALSE
    }

    if SetConsoleCtrlHandler(Some(ctrl_handler), TRUE) == FALSE {
        panic!("could not set ctrl handler");
    }

    let session_name = "pepper_session_name";
    let mut pipe_path = Vec::new();
    pipe_path.extend("\\\\.\\pipe\\".encode_utf16());
    pipe_path.extend(session_name.encode_utf16());
    pipe_path.push(0);

    let mut find_data = Default::default();
    if FindFirstFileW(pipe_path.as_ptr(), &mut find_data) == INVALID_HANDLE_VALUE {
        println!("run server");
        run_server(&pipe_path);
    } else {
        println!("run client");
        run_client(&pipe_path);
    }
}

enum WaitResult {
    Signaled(usize),
    Abandoned(usize),
    Timeout,
}
unsafe fn wait_for_multiple_objects(handles: &[HANDLE], timeout: Option<Duration>) -> WaitResult {
    let timeout = match timeout {
        Some(duration) => duration.as_millis() as _,
        None => INFINITE,
    };
    let len = MAXIMUM_WAIT_OBJECTS.min(handles.len() as DWORD);
    let result = WaitForMultipleObjects(len, handles.as_ptr(), FALSE, timeout);
    if result == WAIT_TIMEOUT {
        WaitResult::Timeout
    } else if result >= WAIT_OBJECT_0 && result < (WAIT_OBJECT_0 + len) {
        WaitResult::Signaled((result - WAIT_OBJECT_0) as _)
    } else if result >= WAIT_ABANDONED_0 && result < (WAIT_ABANDONED_0 + len) {
        WaitResult::Abandoned((result - WAIT_ABANDONED_0) as _)
    } else {
        panic!("could not wait for event")
    }
}

const PIPE_BUFFER_LEN: usize = 512;

enum ReadResult {
    Waiting,
    Ok(usize),
    Err,
}
enum WriteResult {
    Ok,
    Err,
}

struct Pipe {
    pipe_handle: HANDLE,
    overlapped: OVERLAPPED,
    event_handle: HANDLE,
    pending_io: bool,
}
impl Pipe {
    pub unsafe fn create(path: &[u16]) -> Self {
        let event_handle = CreateEventW(std::ptr::null_mut(), TRUE, TRUE, std::ptr::null());
        if event_handle == NULL {
            panic!("could not create new connection");
        }

        let pipe_handle = CreateNamedPipeW(
            path.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_LEN as _,
            PIPE_BUFFER_LEN as _,
            0,
            std::ptr::null_mut(),
        );
        if pipe_handle == INVALID_HANDLE_VALUE {
            panic!("could not create new connection");
        }

        Self::from_handle(pipe_handle)
    }

    pub unsafe fn connect(path: &[u16]) -> Self {
        let pipe_handle = CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            NULL,
        );
        if pipe_handle == INVALID_HANDLE_VALUE {
            panic!("could not establish a connection");
        }

        let mut mode = PIPE_READMODE_BYTE;
        if SetNamedPipeHandleState(
            pipe_handle,
            &mut mode,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == FALSE
        {
            panic!("could not establish a connection");
        }

        Self::from_handle(pipe_handle)
    }

    pub fn from_handle(pipe_handle: HANDLE) -> Self {
        let event_handle =
            unsafe { CreateEventW(std::ptr::null_mut(), TRUE, FALSE, std::ptr::null()) };
        if event_handle == NULL {
            panic!("could not connect to server");
        }

        let mut overlapped = OVERLAPPED::default();
        overlapped.hEvent = event_handle;

        Self {
            pipe_handle,
            overlapped,
            event_handle,
            pending_io: false,
        }
    }

    pub unsafe fn accept(&mut self) -> ReadResult {
        if ConnectNamedPipe(self.pipe_handle, &mut self.overlapped) != FALSE {
            panic!("could not accept incomming connection");
        }

        match GetLastError() {
            ERROR_IO_PENDING => {
                self.pending_io = true;
                ReadResult::Waiting
            }
            ERROR_PIPE_CONNECTED => {
                self.pending_io = false;
                if SetEvent(self.event_handle) == FALSE {
                    panic!("could not accept incomming connection");
                }
                ReadResult::Ok(0)
            }
            _ => {
                self.pending_io = false;
                ReadResult::Err
            }
        }
    }

    pub unsafe fn read_async(&mut self, buf: &mut [u8]) -> ReadResult {
        let mut read_len = 0;
        if self.pending_io {
            if GetOverlappedResult(self.pipe_handle, &mut self.overlapped, &mut read_len, FALSE)
                == FALSE
            {
                match GetLastError() {
                    ERROR_MORE_DATA => {
                        self.pending_io = false;
                        ReadResult::Ok(read_len as _)
                    }
                    _ => {
                        self.pending_io = false;
                        ReadResult::Err
                    }
                }
            } else {
                self.pending_io = false;
                ReadResult::Ok(read_len as _)
            }
        } else {
            if ReadFile(
                self.pipe_handle,
                buf.as_mut_ptr() as _,
                buf.len() as _,
                &mut read_len,
                &mut self.overlapped,
            ) == FALSE
            {
                match GetLastError() {
                    ERROR_IO_PENDING => {
                        self.pending_io = true;
                        ReadResult::Waiting
                    }
                    _ => {
                        self.pending_io = false;
                        ReadResult::Err
                    }
                }
            } else {
                self.pending_io = false;
                ReadResult::Ok(read_len as _)
            }
        }
    }

    pub unsafe fn write(&mut self, buf: &[u8]) -> WriteResult {
        let mut write_len = 0;
        if WriteFile(
            self.pipe_handle,
            buf.as_ptr() as _,
            buf.len() as _,
            &mut write_len,
            std::ptr::null_mut(),
        ) == FALSE
        {
            WriteResult::Err
        } else {
            WriteResult::Ok
        }
    }
}
impl Drop for Pipe {
    fn drop(&mut self) {
        println!("dropping pipe");
        unsafe {
            if CloseHandle(self.pipe_handle) == FALSE {
                panic!("could not finish connection");
            }
            if CloseHandle(self.event_handle) == FALSE {
                panic!("could not finish connection");
            }
        }
    }
}

struct PipeListener {
    pub pipe: Pipe,
}
impl PipeListener {
    pub unsafe fn new(pipe_path: &[u16]) -> Self {
        let mut pipe = Pipe::create(pipe_path);
        match pipe.accept() {
            ReadResult::Waiting => {
                let Pipe {
                    pipe_handle,
                    event_handle,
                    pending_io,
                    ..
                } = pipe;
                let mut overlapped = OVERLAPPED::default();
                overlapped.hEvent = event_handle;
                std::mem::forget(pipe);
                let pipe = Pipe {
                    pipe_handle,
                    overlapped,
                    event_handle,
                    pending_io,
                };
                Self { pipe }
            }
            _ => panic!("could not listen for connections"),
        }
    }

    pub unsafe fn accept(&mut self, pipe_path: &[u16]) -> Option<Pipe> {
        let mut buf = [0; PIPE_BUFFER_LEN];
        match self.pipe.read_async(&mut buf) {
            ReadResult::Waiting => None,
            ReadResult::Ok(_) => {
                let mut pipe = Self::new(pipe_path).pipe;
                std::mem::swap(&mut self.pipe, &mut pipe);
                Some(pipe)
            }
            ReadResult::Err => panic!("could not accept connection {}", GetLastError()),
        }
    }
}

struct AsyncChild {
    child: Child,
    stdout_pipe: Pipe,
    stderr_pipe: Pipe,
}
impl AsyncChild {
    pub fn from_child(child: Child) -> Self {
        let stdout_handle = child.stdout.as_ref().unwrap().as_raw_handle();
        let stderr_handle = child.stderr.as_ref().unwrap().as_raw_handle();
        Self {
            child,
            stdout_pipe: Pipe::from_handle(stdout_handle),
            stderr_pipe: Pipe::from_handle(stderr_handle),
        }
    }
}

enum EventSource {
    ConnectionListener,
    Connection(usize),
    ChildStdout(usize),
    ChildStderr(usize),
}
#[derive(Default)]
struct Events {
    wait_handles: Vec<HANDLE>,
    sources: Vec<EventSource>,
}
impl Events {
    pub fn track(&mut self, handle: HANDLE, source: EventSource) {
        self.wait_handles.push(handle);
        self.sources.push(source);
    }

    pub fn wait_one(&mut self, timeout: Option<Duration>) -> Option<EventSource> {
        let result = match unsafe { wait_for_multiple_objects(&self.wait_handles, timeout) } {
            WaitResult::Signaled(i) => Some(self.sources.swap_remove(i)),
            WaitResult::Abandoned(_) => unreachable!(),
            WaitResult::Timeout => None,
        };

        self.wait_handles.clear();
        self.sources.clear();
        result
    }
}

unsafe fn run_server(pipe_path: &[u16]) {
    let mut read_buf = [0; PIPE_BUFFER_LEN];
    let mut events = Events::default();

    let mut listener = PipeListener::new(pipe_path);
    let mut pipes = Vec::<Option<Pipe>>::new();

    let mut running_child = None;

    unsafe fn disconnect(pipes: &mut Vec<Option<Pipe>>, index: usize) -> bool {
        if let Some(pipe) = &mut pipes[index] {
            println!("client [{}] disconnected", index);

            DisconnectNamedPipe(pipe.pipe_handle);
            pipes[index] = None;

            if let Some(i) = pipes.iter().rposition(Option::is_some) {
                pipes.truncate(i + 1);
                true
            } else {
                pipes.clear();
                false
            }
        } else {
            true
        }
    }

    loop {
        events.track(listener.pipe.event_handle, EventSource::ConnectionListener);
        for (i, pipe) in pipes.iter().enumerate() {
            if let Some(pipe) = pipe {
                events.track(pipe.event_handle, EventSource::Connection(i));
            }
        }

        match events.wait_one(None) {
            Some(EventSource::ConnectionListener) => {
                if let Some(pipe) = listener.accept(pipe_path) {
                    match pipes.iter_mut().find(|p| p.is_none()) {
                        Some(p) => *p = Some(pipe),
                        None => pipes.push(Some(pipe)),
                    }
                }
            }
            Some(EventSource::Connection(i)) => {
                if let Some(pipe) = &mut pipes[i] {
                    match pipe.read_async(&mut read_buf) {
                        ReadResult::Waiting => (),
                        ReadResult::Ok(0) | ReadResult::Err => {
                            if !disconnect(&mut pipes, i) {
                                break;
                            }
                        }
                        ReadResult::Ok(len) => {
                            let message = &read_buf[..len];
                            match Key::parse(&mut message.iter().map(|b| *b as _)) {
                                Ok(Key::Ctrl('r')) => {
                                    println!("execute program");
                                    let child = std::process::Command::new("fd")
                                        .stdin(std::process::Stdio::null())
                                        .stdout(std::process::Stdio::piped())
                                        .stderr(std::process::Stdio::null())
                                        .spawn()
                                        .unwrap();
                                    running_child = Some(AsyncChild::from_child(child));
                                }
                                _ => (),
                            }

                            let message = String::from_utf8_lossy(message);
                            println!(
                                "received {} bytes from client {}! message: '{}'",
                                len, i, message
                            );

                            let message = b"thank you for your message!";
                            match pipe.write(message) {
                                WriteResult::Ok => (),
                                WriteResult::Err => {
                                    if !disconnect(&mut pipes, i) {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Some(EventSource::ChildStdout(i)) => {
                //
            }
            Some(EventSource::ChildStderr(i)) => {
                //
            }
            None => println!("timeout waiting"),
        }
    }

    println!("finish server");
}

unsafe fn run_client(pipe_path: &[u16]) {
    let input_handle = GetStdHandle(STD_INPUT_HANDLE);
    let output_handle = GetStdHandle(STD_OUTPUT_HANDLE);

    let mut original_input_mode = DWORD::default();
    if GetConsoleMode(input_handle, &mut original_input_mode) == FALSE {
        panic!("could not retrieve original console input mode");
    }
    if SetConsoleMode(input_handle, ENABLE_WINDOW_INPUT) == FALSE {
        panic!("could not set console input mode");
    }

    let mut original_output_mode = DWORD::default();
    if GetConsoleMode(output_handle, &mut original_output_mode) == FALSE {
        panic!("could not retrieve original console output mode");
    }
    if SetConsoleMode(
        output_handle,
        ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    ) == FALSE
    {
        panic!("could not set console output mode");
    }

    let mut pipe = Pipe::connect(pipe_path);
    match pipe.write(b"hello there!") {
        WriteResult::Ok => (),
        WriteResult::Err => panic!("could not send message to server"),
    }
    if SetEvent(pipe.event_handle) == FALSE {
        panic!("could not receive next message");
    }

    let mut read_buf = [0u8; 1024 * 2];
    let event_buffer = &mut [INPUT_RECORD::default(); 32][..];
    let wait_handles = [input_handle, pipe.event_handle];

    'main_loop: loop {
        let wait_handle_index = match wait_for_multiple_objects(&wait_handles, None) {
            WaitResult::Signaled(i) => i,
            _ => continue,
        };
        match wait_handle_index {
            0 => {
                let mut event_count: DWORD = 0;
                if ReadConsoleInputW(
                    input_handle,
                    event_buffer.as_mut_ptr(),
                    event_buffer.len() as _,
                    &mut event_count,
                ) == FALSE
                {
                    panic!("could not read console events");
                }

                for i in 0..event_count {
                    let event = event_buffer[i as usize];
                    match event.EventType {
                        KEY_EVENT => {
                            let event = event.Event.KeyEvent();
                            if event.bKeyDown == FALSE {
                                continue;
                            }

                            let control_key_state = event.dwControlKeyState;
                            let keycode = event.wVirtualKeyCode as i32;
                            let repeat_count = event.wRepeatCount as usize;

                            const CHAR_A: i32 = b'A' as _;
                            const CHAR_Z: i32 = b'Z' as _;
                            let key = match keycode {
                                VK_BACK => Key::Backspace,
                                VK_RETURN => Key::Enter,
                                VK_LEFT => Key::Left,
                                VK_RIGHT => Key::Right,
                                VK_UP => Key::Up,
                                VK_DOWN => Key::Down,
                                VK_HOME => Key::Home,
                                VK_END => Key::End,
                                VK_PRIOR => Key::PageUp,
                                VK_NEXT => Key::PageDown,
                                VK_TAB => Key::Tab,
                                VK_DELETE => Key::Delete,
                                VK_F1..=VK_F24 => Key::F((keycode - VK_F1 + 1) as _),
                                VK_ESCAPE => Key::Esc,
                                CHAR_A..=CHAR_Z => {
                                    const ALT_PRESSED_MASK: DWORD =
                                        LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED;
                                    const CTRL_PRESSED_MASK: DWORD =
                                        LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED;

                                    let c = keycode as u8;
                                    if control_key_state & ALT_PRESSED_MASK != 0 {
                                        Key::Alt(c.to_ascii_lowercase() as _)
                                    } else if control_key_state & CTRL_PRESSED_MASK != 0 {
                                        Key::Ctrl(c.to_ascii_lowercase() as _)
                                    } else if control_key_state & SHIFT_PRESSED != 0 {
                                        Key::Char(c as _)
                                    } else {
                                        Key::Char(c.to_ascii_lowercase() as _)
                                    }
                                }
                                _ => {
                                    let c = *(event.uChar.AsciiChar()) as u8;
                                    if !c.is_ascii_graphic() {
                                        continue;
                                    }

                                    Key::Char(c as _)
                                }
                            };

                            let message = format!("{}", key);
                            println!("{} key x {}", message, repeat_count);
                            match pipe.write(message.as_bytes()) {
                                WriteResult::Ok => (),
                                WriteResult::Err => panic!("could not send message to server"),
                            }

                            if let Key::Esc = key {
                                break 'main_loop;
                            }
                        }
                        WINDOW_BUFFER_SIZE_EVENT => {
                            let size = event.Event.WindowBufferSizeEvent().dwSize;
                            let x = size.X as u16;
                            let y = size.Y as u16;
                            println!("window resized to {}, {}", x, y);
                        }
                        _ => (),
                    }
                }
            }
            1 => match pipe.read_async(&mut read_buf) {
                ReadResult::Waiting => (),
                ReadResult::Ok(0) | ReadResult::Err => {
                    break;
                }
                ReadResult::Ok(len) => {
                    if SetEvent(pipe.event_handle) == FALSE {
                        panic!("could not receive next message");
                    }

                    let message = &read_buf[..len];
                    let message = String::from_utf8_lossy(message);
                    println!("received {} bytes from server! message: '{}'", len, message);
                }
            },
            _ => unreachable!(),
        }
    }

    println!("finish client");

    SetConsoleMode(input_handle, original_input_mode);
    SetConsoleMode(output_handle, original_output_mode);
}