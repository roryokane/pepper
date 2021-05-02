use std::{
    fs, io,
    os::unix::{
        io::{AsRawFd, RawFd},
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        atomic::{AtomicIsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use pepper::{
    application::{AnyError, ApplicationEvent, ClientApplication, ServerApplication},
    client::ClientHandle,
    platform::{BufPool, Key, Platform, PlatformRequest, ProcessHandle},
    Args,
};

mod unix_utils;
use unix_utils::{run, RawMode, Process};

const MAX_CLIENT_COUNT: usize = 20;
const MAX_PROCESS_COUNT: usize = 42;
const CLIENT_EVENT_BUFFER_LEN: usize = 32;

pub fn main() {
    run(run_server, run_client);
}

struct EventFd(RawFd);
impl EventFd {
    pub fn new() -> Self {
        let fd = unsafe { libc::eventfd(0, 0) };
        if fd == -1 {
            panic!("could not create event fd");
        }
        Self(fd)
    }

    pub fn write(fd: RawFd) {
        let mut buf = 1u64.to_ne_bytes();
        let result = unsafe { libc::write(fd, buf.as_mut_ptr() as _, buf.len() as _) };
        if result != buf.len() as _ {
            panic!("could not write to event fd");
        }
    }

    pub fn read(&self) {
        let mut buf = [0; 8];
        let result = unsafe { libc::read(self.0, buf.as_mut_ptr() as _, buf.len() as _) };
        if result != buf.len() as _ {
            panic!("could not read from event fd");
        }
    }
}
impl AsRawFd for EventFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}
impl Drop for EventFd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

struct SignalFd(RawFd);
impl SignalFd {
    pub fn new(signal: libc::c_int) -> Self {
        unsafe {
            let mut signals = std::mem::zeroed();
            let result = libc::sigemptyset(&mut signals);
            if result == -1 {
                panic!("could not create signal fd");
            }
            let result = libc::sigaddset(&mut signals, signal);
            if result == -1 {
                panic!("could not create signal fd");
            }
            let result = libc::sigprocmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut());
            if result == -1 {
                panic!("could not create signal fd");
            }
            let fd = libc::signalfd(-1, &signals, 0);
            if fd == -1 {
                panic!("could not create signal fd");
            }
            Self(fd)
        }
    }

    pub fn read(&self) {
        let mut buf = [0u8; std::mem::size_of::<libc::signalfd_siginfo>()];
        let result = unsafe { libc::read(self.0, buf.as_mut_ptr() as _, buf.len() as _) };
        if result != buf.len() as _ {
            panic!("could not read from signal fd");
        }
    }
}
impl AsRawFd for SignalFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}
impl Drop for SignalFd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

struct EpollEvents([libc::epoll_event; CLIENT_EVENT_BUFFER_LEN]);
impl EpollEvents {
    pub fn new() -> Self {
        const DEFAULT_EPOLL_EVENT: libc::epoll_event = libc::epoll_event { events: 0, u64: 0 };
        Self([DEFAULT_EPOLL_EVENT; CLIENT_EVENT_BUFFER_LEN])
    }
}
struct Epoll(RawFd);
impl Epoll {
    pub fn new() -> Self {
        let fd = unsafe { libc::epoll_create1(0) };
        if fd == -1 {
            panic!("could not create epoll");
        }
        Self(fd)
    }

    pub fn add(&self, fd: RawFd, index: usize) {
        let mut event = libc::epoll_event {
            events: (libc::EPOLLIN | libc::EPOLLERR | libc::EPOLLRDHUP | libc::EPOLLHUP) as _,
            u64: index as _,
        };
        let result = unsafe { libc::epoll_ctl(self.0, libc::EPOLL_CTL_ADD, fd, &mut event) };
        if result == -1 {
            panic!("could not add event");
        }
    }

    pub fn remove(&self, fd: RawFd) {
        let mut event = libc::epoll_event { events: 0, u64: 0 };
        unsafe { libc::epoll_ctl(self.0, libc::EPOLL_CTL_DEL, fd, &mut event) };
    }

    pub fn wait<'a>(
        &self,
        events: &'a mut EpollEvents,
        timeout: Option<Duration>,
    ) -> impl 'a + ExactSizeIterator<Item = usize> {
        let timeout = match timeout {
            Some(duration) => duration.as_millis() as _,
            None => -1,
        };
        let len = unsafe {
            libc::epoll_wait(self.0, events.0.as_mut_ptr(), events.0.len() as _, timeout)
        };
        if len == -1 {
            panic!("could not wait for events");
        }

        events.0[..len as usize].iter().map(|e| e.u64 as _)
    }
}
impl Drop for Epoll {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

fn run_server(stream_path: &Path) -> Result<(), AnyError> {
    use io::{Read, Write};

    const NONE_PROCESS: Option<Process> = None;
    static NEW_REQUEST_EVENT_FD: AtomicIsize = AtomicIsize::new(-1);

    if let Some(dir) = stream_path.parent() {
        if !dir.exists() {
            let _ = fs::create_dir(dir);
        }
    }

    let _ = fs::remove_file(stream_path);
    let listener =
        UnixListener::bind(stream_path).expect("could not start unix domain socket server");

    let mut client_connections: [Option<UnixStream>; MAX_CLIENT_COUNT] = Default::default();
    let mut processes = [NONE_PROCESS; MAX_PROCESS_COUNT];
    let mut buf_pool = BufPool::default();

    let new_request_event = EventFd::new();
    NEW_REQUEST_EVENT_FD.store(new_request_event.as_raw_fd() as _, Ordering::Relaxed);

    let (request_sender, request_receiver) = mpsc::channel();
    let platform = Platform::new(
        || EventFd::write(NEW_REQUEST_EVENT_FD.load(Ordering::Relaxed) as _),
        request_sender,
    );

    let event_sender = match ServerApplication::run(platform) {
        Some(sender) => sender,
        None => return Ok(()),
    };

    let mut timeout = Some(ServerApplication::idle_duration());

    const CLIENTS_START_INDEX: usize = 1 + 1;
    const CLIENTS_LAST_INDEX: usize = CLIENTS_START_INDEX + MAX_CLIENT_COUNT - 1;
    const PROCESSES_START_INDEX: usize = CLIENTS_LAST_INDEX + 1;
    const PROCESSES_LAST_INDEX: usize = PROCESSES_START_INDEX + MAX_PROCESS_COUNT - 1;

    let epoll = Epoll::new();
    epoll.add(new_request_event.as_raw_fd(), 0);
    epoll.add(listener.as_raw_fd(), 1);
    let mut epoll_events = EpollEvents::new();

    loop {
        let events = epoll.wait(&mut epoll_events, timeout);
        if events.len() == 0 {
            timeout = None;
            event_sender.send(ApplicationEvent::Idle)?;
            continue;
        }

        for event_index in events {
            match event_index {
                0 => {
                    new_request_event.read();
                    for request in request_receiver.try_iter() {
                        match request {
                            PlatformRequest::Exit => return Ok(()),
                            PlatformRequest::WriteToClient { handle, buf } => {
                                let index = handle.into_index();
                                if let Some(ref mut connection) = client_connections[index] {
                                    if connection.write_all(buf.as_bytes()).is_err() {
                                        epoll.remove(connection.as_raw_fd());
                                        client_connections[index] = None;
                                        event_sender
                                            .send(ApplicationEvent::ConnectionClose { handle })?;
                                    }
                                }
                            }
                            PlatformRequest::CloseClient { handle } => {
                                let index = handle.into_index();
                                if let Some(connection) = client_connections[index].take() {
                                    epoll.remove(connection.as_raw_fd());
                                }
                                event_sender.send(ApplicationEvent::ConnectionClose { handle })?;
                            }
                            PlatformRequest::SpawnProcess {
                                tag,
                                mut command,
                                buf_len,
                            } => {
                                for (i, p) in processes.iter_mut().enumerate() {
                                    if p.is_some() {
                                        continue;
                                    }

                                    let handle = ProcessHandle(i);
                                    match command.spawn() {
                                        Ok(child) => {
                                            let process = Process::new(child, tag, buf_len);
                                            if let Some(fd) = process.try_as_raw_fd() {
                                                epoll.add(fd, PROCESSES_START_INDEX + i);
                                            }
                                            *p = Some(process);
                                            event_sender.send(
                                                ApplicationEvent::ProcessSpawned { tag, handle },
                                            )?;
                                        }
                                        Err(_) => {
                                            event_sender.send(ApplicationEvent::ProcessExit {
                                                tag,
                                                success: false,
                                            })?
                                        }
                                    }
                                    break;
                                }
                            }
                            PlatformRequest::WriteToProcess { handle, buf } => {
                                let index = handle.0;
                                if let Some(ref mut process) = processes[index] {
                                    if !process.write(buf.as_bytes()) {
                                        if let Some(fd) = process.try_as_raw_fd() {
                                            epoll.remove(fd);
                                        }
                                        let tag = process.tag();
                                        process.kill();
                                        processes[index] = None;
                                        event_sender.send(ApplicationEvent::ProcessExit {
                                            tag,
                                            success: false,
                                        })?;
                                    }
                                }
                            }
                            PlatformRequest::CloseProcessInput { handle } => {
                                if let Some(ref mut process) = processes[handle.0] {
                                    process.close_input();
                                }
                            }
                            PlatformRequest::KillProcess { handle } => {
                                let index = handle.0;
                                if let Some(ref mut process) = processes[index] {
                                    if let Some(fd) = process.try_as_raw_fd() {
                                        epoll.remove(fd);
                                    }
                                    let tag = process.tag();
                                    process.kill();
                                    processes[index] = None;
                                    event_sender.send(ApplicationEvent::ProcessExit {
                                        tag,
                                        success: false,
                                    })?;
                                }
                            }
                        }
                    }
                }
                1 => match listener.accept() {
                    Ok((connection, _)) => {
                        for (i, c) in client_connections.iter_mut().enumerate() {
                            if c.is_none() {
                                epoll.add(connection.as_raw_fd(), CLIENTS_START_INDEX + i);
                                *c = Some(connection);
                                let handle = ClientHandle::from_index(i).unwrap();
                                event_sender.send(ApplicationEvent::ConnectionOpen { handle })?;
                                break;
                            }
                        }
                    }
                    Err(error) => panic!("could not accept connection {}", error),
                },
                CLIENTS_START_INDEX..=CLIENTS_LAST_INDEX => {
                    let index = event_index - CLIENTS_START_INDEX;
                    if let Some(ref mut connection) = client_connections[index] {
                        let handle = ClientHandle::from_index(index).unwrap();
                        let mut buf = buf_pool.acquire();
                        let write = buf.write_with_len(ServerApplication::connection_buffer_len());
                        match connection.read(write) {
                            Ok(0) | Err(_) => {
                                epoll.remove(connection.as_raw_fd());
                                client_connections[index] = None;
                                event_sender.send(ApplicationEvent::ConnectionClose { handle })?;
                            }
                            Ok(len) => {
                                write.truncate(len);
                                let buf = buf.share();
                                event_sender
                                    .send(ApplicationEvent::ConnectionOutput { handle, buf })?;
                            }
                        }
                    }

                    timeout = Some(ServerApplication::idle_duration());
                }
                PROCESSES_START_INDEX..=PROCESSES_LAST_INDEX => {
                    let index = event_index - PROCESSES_START_INDEX;
                    if let Some(ref mut process) = processes[index] {
                        let tag = process.tag();
                        match process.read(&mut buf_pool) {
                            Ok(None) => (),
                            Ok(Some(buf)) => {
                                if buf.as_bytes().is_empty() {
                                    event_sender.send(ApplicationEvent::ProcessExit {
                                        tag,
                                        success: true,
                                    })?;
                                } else {
                                    event_sender
                                        .send(ApplicationEvent::ProcessOutput { tag, buf })?;
                                }
                            }
                            Err(()) => {
                                if let Some(fd) = process.try_as_raw_fd() {
                                    epoll.remove(fd);
                                }
                                process.kill();
                                processes[index] = None;
                                event_sender.send(ApplicationEvent::ProcessExit {
                                    tag,
                                    success: false,
                                })?;
                            }
                        }
                    }
                }
                _ => unreachable!(),
            }
        }
    }
}

fn run_client(args: Args, mut connection: UnixStream) {
    use io::{Read, Write};

    let stdin = io::stdin();
    let mut stdin = stdin.lock();

    let mut client_index = 0;
    match connection.read(std::slice::from_mut(&mut client_index)) {
        Ok(1) => (),
        _ => return,
    }

    let client_handle = ClientHandle::from_index(client_index as _).unwrap();
    let is_pipped = unsafe { libc::isatty(stdin.as_raw_fd()) == 0 };

    let stdout = io::stdout();
    let mut application = ClientApplication::new(client_handle, stdout.lock(), is_pipped);
    let bytes = application.init(args);
    if connection.write(bytes).is_err() {
        return;
    }

    let raw_mode;
    let resize_signal;

    let epoll = Epoll::new();
    epoll.add(connection.as_raw_fd(), 0);
    epoll.add(stdin.as_raw_fd(), 1);
    let mut epoll_events = EpollEvents::new();

    if is_pipped {
        raw_mode = None;
        resize_signal = None;
    } else {
        raw_mode = Some(RawMode::enter());
        let signal = SignalFd::new(libc::SIGWINCH);
        epoll.add(signal.as_raw_fd(), 2);
        resize_signal = Some(signal);

        let size = get_console_size();
        let bytes = application.update(Some(size), &[], &[], &[]);
        if connection.write(bytes).is_err() {
            return;
        }
    }

    let mut keys = Vec::new();
    let mut stream_buf = [0; ClientApplication::connection_buffer_len()];
    let mut stdin_buf = [0; ClientApplication::stdin_buffer_len()];

    'main_loop: loop {
        for event_index in epoll.wait(&mut epoll_events, None) {
            let mut resize = None;
            let mut stdin_bytes = &[][..];
            let mut server_bytes = &[][..];

            keys.clear();

            match event_index {
                0 => match connection.read(&mut stream_buf) {
                    Ok(0) | Err(_) => break 'main_loop,
                    Ok(len) => server_bytes = &stream_buf[..len],
                },
                1 => match stdin.read(&mut stdin_buf) {
                    Ok(0) | Err(_) => {
                        epoll.remove(stdin.as_raw_fd());
                        continue;
                    }
                    Ok(len) => {
                        let bytes = &stdin_buf[..len];
                        if is_pipped {
                            stdin_bytes = bytes;
                        } else {
                            parse_terminal_keys(&bytes, &mut keys);
                        }
                    }
                },
                2 => {
                    if let Some(ref signal) = resize_signal {
                        signal.read();
                        resize = Some(get_console_size());
                    }
                }
                _ => unreachable!(),
            }

            let bytes = application.update(resize, &keys, stdin_bytes, server_bytes);
            if connection.write(bytes).is_err() {
                break;
            }
        }
    }

    drop(raw_mode);
}

fn get_console_size() -> (usize, usize) {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::ioctl(
            libc::STDOUT_FILENO,
            libc::TIOCGWINSZ,
            &mut size as *mut libc::winsize,
        )
    };
    if result == -1 || size.ws_col == 0 {
        panic!("could not get console size");
    }

    (size.ws_col as _, size.ws_row as _)
}

fn parse_terminal_keys(mut buf: &[u8], keys: &mut Vec<Key>) {
    loop {
        let (key, rest) = match buf {
            &[] => break,
            &[0x1b, b'[', b'5', b'~', ref rest @ ..] => (Key::PageUp, rest),
            &[0x1b, b'[', b'6', b'~', ref rest @ ..] => (Key::PageDown, rest),
            &[0x1b, b'[', b'A', ref rest @ ..] => (Key::Up, rest),
            &[0x1b, b'[', b'B', ref rest @ ..] => (Key::Down, rest),
            &[0x1b, b'[', b'C', ref rest @ ..] => (Key::Right, rest),
            &[0x1b, b'[', b'D', ref rest @ ..] => (Key::Left, rest),
            &[0x1b, b'[', b'1', b'~', ref rest @ ..]
            | &[0x1b, b'[', b'7', b'~', ref rest @ ..]
            | &[0x1b, b'[', b'H', ref rest @ ..]
            | &[0x1b, b'O', b'H', ref rest @ ..] => (Key::Home, rest),
            &[0x1b, b'[', b'4', b'~', ref rest @ ..]
            | &[0x1b, b'[', b'8', b'~', ref rest @ ..]
            | &[0x1b, b'[', b'F', ref rest @ ..]
            | &[0x1b, b'O', b'F', ref rest @ ..] => (Key::End, rest),
            &[0x1b, b'[', b'3', b'~', ref rest @ ..] => (Key::Delete, rest),
            &[0x1b, ref rest @ ..] => (Key::Esc, rest),
            &[0x8, ref rest @ ..] => (Key::Backspace, rest),
            &[b'\n', ref rest @ ..] => (Key::Enter, rest),
            &[b'\t', ref rest @ ..] => (Key::Tab, rest),
            &[0x7f, ref rest @ ..] => (Key::Delete, rest),
            &[b @ 0b0..=0b11111, ref rest @ ..] => {
                let byte = b | 0b01100000;
                (Key::Ctrl(byte as _), rest)
            }
            _ => match buf.iter().position(|b| b.is_ascii()).unwrap_or(buf.len()) {
                0 => (Key::Char(buf[0] as _), &buf[1..]),
                len => {
                    let (c, rest) = buf.split_at(len);
                    match std::str::from_utf8(c) {
                        Ok(s) => match s.chars().next() {
                            Some(c) => (Key::Char(c), rest),
                            None => (Key::None, rest),
                        },
                        Err(_) => (Key::None, rest),
                    }
                }
            },
        };
        buf = rest;
        keys.push(key);
    }
}

