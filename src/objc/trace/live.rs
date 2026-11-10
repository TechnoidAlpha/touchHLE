/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! The live stream for the side-by-side app (`--live-port=N`, 4Towers
//! branch): the rows the trace writes to `trace.log`, `input.log` and
//! `state.jsonl`, sent as they are written to one reader on
//! `127.0.0.1:N`, so the remake can follow the original frame by frame.
//!
//! Each line is a channel letter, a tab and the row's bytes unchanged:
//!
//! - `H\t{json}`: once per connection, first (made by the caller);
//! - `T\t<row>`: a `trace.log` row;
//! - `I\t<row>`: an `input.log` row;
//! - `S\t<line>`: a `state.jsonl` line, only when the reader asked for it;
//! - `F\t<frame>\t<ms>`: frame `<frame>` has been presented; every row of
//!   that frame or an earlier one is before it.
//!
//! The reader connects and sends one line first: `hello` and its words
//! (`hello state` asks for the `S` lines). The game thread never waits on
//! the socket: the listener runs on its own thread, a frame's rows go to a
//! writer thread through a bounded queue with `try_send` (as the frame
//! capture does), and a reader whose queue is full is dropped and its
//! socket closed, not fed a stream with a gap in it. With no reader the cost
//! of a row is one branch.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Frames a reader may fall behind before it is dropped: about 30 s of play
/// at 30 frames a second (docs/plans/SIDE_BY_SIDE.md, LIVE2; a guess until
/// the cloud end-to-end test measures it).
pub const QUEUE_FRAMES: usize = 900;

/// How long the listener waits for a new reader's hello line.
const HELLO_WAIT: Duration = Duration::from_secs(2);

/// A reader the listener has accepted and the game thread has not taken
/// up yet.
struct Pending {
    stream: TcpStream,
    wants_state: bool,
}

/// The reader being fed. Dropping it closes its queue: the writer thread
/// sends what is queued and then closes the socket. [Reader::cut] closes
/// the socket at once instead.
struct Reader {
    sender: SyncSender<Vec<u8>>,
    /// A handle on the socket, so [Reader::cut] can also end a writer thread
    /// that is stuck in a write to a reader that does not read.
    stream: TcpStream,
    wants_state: bool,
}

impl Reader {
    /// Drop the reader and close its socket now, queued rows and all.
    fn cut(self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

pub struct LiveStream {
    port: u16,
    queue_frames: usize,
    pending: Arc<Mutex<Option<Pending>>>,
    reader: Option<Reader>,
    /// The rows of the frame in progress; filled only while a reader is
    /// connected.
    buf: Vec<u8>,
    /// Readers dropped because they fell behind.
    pub dropped: u32,
}

impl LiveStream {
    /// Listen on `127.0.0.1:port` (0 picks a free port; see [Self::port]).
    pub fn bind(port: u16) -> std::io::Result<LiveStream> {
        Self::bind_with(port, QUEUE_FRAMES)
    }

    /// [Self::bind] with a queue of `queue_frames` frames (for the tests).
    pub fn bind_with(port: u16, queue_frames: usize) -> std::io::Result<LiveStream> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let port = listener.local_addr()?.port();
        let pending: Arc<Mutex<Option<Pending>>> = Arc::new(Mutex::new(None));
        let shared = pending.clone();
        std::thread::Builder::new()
            .name("live listener".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let wants_state = read_hello(&stream);
                    // A newer reader takes the place of one not yet taken up.
                    *shared.lock().unwrap() = Some(Pending {
                        stream,
                        wants_state,
                    });
                }
            })?;
        Ok(LiveStream {
            port,
            queue_frames,
            pending,
            reader: None,
            buf: Vec::new(),
            dropped: 0,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Whether a reader is being fed (the tests; LIVE9's touches).
    #[allow(dead_code)]
    pub fn is_on(&self) -> bool {
        self.reader.is_some()
    }

    /// A `trace.log` row, as written to the file (ending in a newline).
    pub fn trace_row(&mut self, row: &[u8]) {
        if self.reader.is_some() {
            self.push(b'T', row);
        }
    }

    /// An `input.log` row, as written to the file.
    pub fn input_row(&mut self, row: &[u8]) {
        if self.reader.is_some() {
            self.push(b'I', row);
        }
    }

    /// A `state.jsonl` line, as written to the file; sent only to a reader
    /// that asked for it.
    pub fn state_line(&mut self, line: &[u8]) {
        if self.reader.as_ref().is_some_and(|r| r.wants_state) {
            self.push(b'S', line);
        }
    }

    fn push(&mut self, channel: u8, row: &[u8]) {
        self.buf.push(channel);
        self.buf.push(b'\t');
        self.buf.extend_from_slice(row);
        if !row.ends_with(b"\n") {
            self.buf.push(b'\n');
        }
    }

    /// The end of frame `frame`, after its last row: the frame's rows and
    /// its `F` line go to the reader. A reader the listener accepted since
    /// the last call is taken up here: it gets `header()` as its `H` line
    /// and the rows from the next frame on. Never blocks.
    pub fn end_frame(&mut self, frame: u32, ms: f64, header: impl FnOnce() -> String) {
        if self.reader.is_some() {
            self.buf
                .extend_from_slice(format!("F\t{}\t{:.3}\n", frame, ms).as_bytes());
            let rows = std::mem::take(&mut self.buf);
            let reader = self.reader.as_ref().unwrap();
            match reader.sender.try_send(rows) {
                Ok(()) => (),
                Err(TrySendError::Full(_)) => {
                    self.dropped += 1;
                    log!(
                        "Live stream: the reader fell {} frames behind at frame {}; dropped it",
                        self.queue_frames,
                        frame
                    );
                    if let Some(reader) = self.reader.take() {
                        reader.cut();
                    }
                }
                Err(TrySendError::Disconnected(_)) => {
                    log!("Live stream: the reader went away at frame {}", frame);
                    self.reader = None;
                }
            }
        }
        // try_lock: the listener holds the lock only to store a new reader.
        let taken = match self.pending.try_lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => None,
        };
        if let Some(p) = taken {
            self.take_up(p, header());
        }
    }

    fn take_up(&mut self, p: Pending, header: String) {
        let Ok(write_half) = p.stream.try_clone() else {
            return;
        };
        let (sender, receiver) = sync_channel::<Vec<u8>>(self.queue_frames);
        let mut first = b"H\t".to_vec();
        first.extend_from_slice(header.trim_end_matches('\n').as_bytes());
        first.push(b'\n');
        // The queue is new and empty, so this cannot fail.
        let _ = sender.try_send(first);
        let spawned = std::thread::Builder::new()
            .name("live writer".into())
            .spawn(move || {
                let mut out = write_half;
                for chunk in receiver {
                    if out.write_all(&chunk).is_err() {
                        break;
                    }
                }
                let _ = out.shutdown(Shutdown::Both);
            });
        if spawned.is_err() {
            return;
        }
        log!("Live stream: a reader connected");
        self.buf.clear();
        // Taking up a new reader cuts the old one.
        if let Some(old) = self.reader.take() {
            old.cut();
        }
        self.reader = Some(Reader {
            sender,
            stream: p.stream,
            wants_state: p.wants_state,
        });
    }
}

/// `trace.log` and, when `--live-port=` is set, the live stream: every
/// write goes to the file as before, and each of its rows also to the
/// reader, if one is connected. The file's bytes do not depend on the
/// reader.
pub struct TraceOut {
    file: File,
    pub live: Option<LiveStream>,
}

impl TraceOut {
    pub fn new(file: File, live: Option<LiveStream>) -> TraceOut {
        TraceOut { file, live }
    }

    /// Write `bytes` (whole rows) to `trace.log`, and each row to the
    /// reader.
    pub fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let result = self.file.write_all(bytes);
        if let Some(live) = self.live.as_mut() {
            for row in bytes.split_inclusive(|&b| b == b'\n') {
                live.trace_row(row);
            }
        }
        result
    }
}

/// Read a new reader's first line (`hello` and its words) and say whether
/// it asked for the state lines. A reader that sends nothing within
/// [HELLO_WAIT] gets the default (no state lines).
fn read_hello(stream: &TcpStream) -> bool {
    let _ = stream.set_read_timeout(Some(HELLO_WAIT));
    let mut line = String::new();
    let wants = match stream.try_clone() {
        Ok(s) => {
            let mut r = BufReader::new(s.take(1024));
            r.read_line(&mut line).is_ok()
                && line.split_whitespace().next() == Some("hello")
                && line.split_whitespace().any(|w| w == "state")
        }
        Err(_) => false,
    };
    let _ = stream.set_read_timeout(None);
    wants
}

use std::io::Read as _;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::time::Instant;

    fn connect(live: &mut LiveStream, hello: &str) -> TcpStream {
        let mut s = TcpStream::connect(("127.0.0.1", live.port())).unwrap();
        s.write_all(hello.as_bytes()).unwrap();
        let t0 = Instant::now();
        let mut f = 0;
        while !live.is_on() {
            assert!(
                t0.elapsed() < Duration::from_secs(5),
                "reader never taken up"
            );
            live.end_frame(f, 0.0, || "{\"schema\":\"4towers-live-1\"}".to_string());
            f += 1;
            std::thread::sleep(Duration::from_millis(5));
        }
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s
    }

    fn read_until_f(r: &mut BufReader<TcpStream>, frame: u32) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let mut l = String::new();
            assert!(r.read_line(&mut l).unwrap() > 0, "stream ended early");
            let done = l.starts_with(&format!("F\t{}\t", frame));
            lines.push(l);
            if done {
                return lines;
            }
        }
    }

    #[test]
    fn no_reader_buffers_nothing() {
        let mut live = LiveStream::bind(0).unwrap();
        live.trace_row(b"1\t2.0\t0\tguest\tsend\tA\tb\n");
        live.input_row(b"1\t2.0\tdown\tScript\t10\t20\n");
        live.state_line(b"{}\n");
        assert!(live.buf.is_empty());
        assert!(!live.is_on());
        live.end_frame(1, 2.0, || unreachable!("no reader, no header"));
    }

    #[test]
    fn a_reader_gets_h_first_then_rows_behind_their_letter_and_f_closes_the_frame() {
        let mut live = LiveStream::bind(0).unwrap();
        let s = connect(&mut live, "hello\n");
        let mut r = BufReader::new(s);
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        assert_eq!(h, "H\t{\"schema\":\"4towers-live-1\"}\n");
        let trace = b"7\t233.500\t0\tguest\tsend\tCreeps\tdmg:\t0x00000001\t1e-45\n";
        let input = b"7\t233.600\tdown\tScript\t160\t300\n";
        live.trace_row(trace);
        live.input_row(input);
        live.state_line(b"{\"kind\":\"snapshot\"}\n");
        live.end_frame(7, 233.75, || unreachable!());
        let lines = read_until_f(&mut r, 7);
        assert_eq!(
            lines,
            vec![
                format!("T\t{}", std::str::from_utf8(trace).unwrap()),
                format!("I\t{}", std::str::from_utf8(input).unwrap()),
                "F\t7\t233.750\n".to_string(),
            ],
            "state lines go only to a reader that asked"
        );
    }

    #[test]
    fn hello_state_asks_for_the_state_lines() {
        let mut live = LiveStream::bind(0).unwrap();
        let s = connect(&mut live, "hello state\n");
        let mut r = BufReader::new(s);
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        live.state_line(b"{\"kind\":\"snapshot\",\"frame\":9}\n");
        live.end_frame(9, 300.0, || unreachable!());
        let lines = read_until_f(&mut r, 9);
        assert_eq!(lines[0], "S\t{\"kind\":\"snapshot\",\"frame\":9}\n");
    }

    #[test]
    fn a_full_queue_drops_the_reader_and_end_frame_returns_at_once() {
        // The game side runs on its own thread: a queue that blocked when
        // full would hang it, and the test fails on the timeout, not by
        // hanging.
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut live = LiveStream::bind_with(0, 2).unwrap();
            // A reader that never reads.
            let _s = connect(&mut live, "hello\n");
            let big = vec![b'x'; 1 << 20];
            let mut row = big.clone();
            row.push(b'\n');
            let t0 = Instant::now();
            let mut f = 100;
            while live.is_on() && f < 400 {
                live.trace_row(&row);
                live.end_frame(f, 0.0, || unreachable!());
                f += 1;
            }
            let _ = done_tx.send((live.is_on(), live.dropped, t0.elapsed()));
        });
        let (on, dropped, took) = done_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("end_frame blocked on a reader that does not read");
        assert!(!on, "the reader was not dropped");
        assert_eq!(dropped, 1);
        assert!(took < Duration::from_secs(10), "took {:?}", took);
    }

    #[test]
    fn a_new_reader_connects_after_a_drop() {
        let mut live = LiveStream::bind_with(0, 2).unwrap();
        let first = connect(&mut live, "hello\n");
        drop(first);
        // The writer sees the closed socket; the game side sees the closed
        // queue at a later frame.
        let t0 = Instant::now();
        let mut f = 1;
        while live.is_on() {
            assert!(
                t0.elapsed() < Duration::from_secs(5),
                "the closed reader stayed on"
            );
            live.trace_row(&[b'y'; 65536]);
            live.end_frame(f, 0.0, || unreachable!());
            f += 1;
            std::thread::sleep(Duration::from_millis(2));
        }
        let s = connect(&mut live, "hello\n");
        let mut r = BufReader::new(s);
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        assert!(h.starts_with("H\t"));
        live.end_frame(5000, 1.0, || unreachable!());
        let lines = read_until_f(&mut r, 5000);
        assert_eq!(lines, vec!["F\t5000\t1.000\n".to_string()]);
    }

    #[test]
    fn a_reader_that_sends_no_hello_is_taken_up_after_the_wait() {
        let mut live = LiveStream::bind(0).unwrap();
        let s = TcpStream::connect(("127.0.0.1", live.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let t0 = Instant::now();
        while !live.is_on() {
            assert!(t0.elapsed() < Duration::from_secs(5));
            live.end_frame(0, 0.0, || "{}".to_string());
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut b = [0u8; 4];
        let mut s = s;
        s.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"H\t{}");
    }
}

#[cfg(test)]
mod trace_out_tests {
    use super::*;
    use std::io::Read;

    const ROWS: &[&[u8]] = &[
        b"12\t400.125\t0\tguest\tsend\tCreeps\tdmg:\t0x436c4001\t236.25002\n",
        b"12\t400.200\t0\trng\tgen\t-\trandom\t0x4b5d5bec\t0x4b5d5bec\n13\t433.000\t0\tguest\tsend\tCCScheduler\ttick:\t0x3d08a58b\n",
        b"13\t433.010\t0\tlifecycle\t-\t-\tlifecycle: background\n",
    ];

    fn write_all_rows(out: &mut TraceOut) {
        for row in ROWS {
            out.write_all(row).unwrap();
        }
    }

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("touchhle-live-{}-{}", std::process::id(), name))
    }

    #[test]
    fn a_trace_written_with_a_reader_has_the_same_bytes_as_one_without() {
        let (p1, p2) = (temp("without"), temp("with"));
        let mut without = TraceOut::new(File::create(&p1).unwrap(), None);
        write_all_rows(&mut without);
        drop(without);

        let mut live = LiveStream::bind(0).unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", live.port())).unwrap();
        s.write_all(b"hello\n").unwrap();
        let t0 = std::time::Instant::now();
        while !live.is_on() {
            assert!(t0.elapsed() < Duration::from_secs(5));
            live.end_frame(0, 0.0, || "{}".to_string());
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut with = TraceOut::new(File::create(&p2).unwrap(), Some(live));
        write_all_rows(&mut with);
        with.live
            .as_mut()
            .unwrap()
            .end_frame(13, 433.5, || unreachable!());
        drop(with);

        let a = std::fs::read(&p1).unwrap();
        let b = std::fs::read(&p2).unwrap();
        assert_eq!(a, b, "the file must not change with a reader");
        assert_eq!(a, ROWS.concat());

        // Every row reached the reader behind its T, one line each.
        let mut got = String::new();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        while r.read_line(&mut line).unwrap() > 0 {
            got.push_str(&line);
            if line.starts_with("F\t13\t") {
                break;
            }
            line.clear();
        }
        let want: String = std::str::from_utf8(&ROWS.concat())
            .unwrap()
            .lines()
            .map(|l| format!("T\t{l}\n"))
            .collect::<String>()
            + "F\t13\t433.500\n";
        assert_eq!(got, format!("H\t{{}}\n{want}"));
        let _ = r.get_mut().read(&mut [0u8; 1]);
        let _ = std::fs::remove_file(p1);
        let _ = std::fs::remove_file(p2);
    }
}
