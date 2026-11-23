//! Destinations for log output (`--log-output`).
//!
//! Log output never goes to stdout, which carries the LSP communication with
//! the editor when the stdio transport is used, or to stderr. Without
//! `--log-output`, nothing is logged. stderr is never redirected.

use std::collections::VecDeque;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr};

/// Where log records are written to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogOutput {
    /// Append to a file.
    File(PathBuf),
    /// Send to a TCP socket, given as `host:port`.
    Socket(String),
}

impl FromStr for LogOutput {
    type Err = String;

    /// `host:port` (e.g. `localhost:1337`, `[::1]:1337`) selects a socket,
    /// everything else is a file path. Paths that look like `host:port` can
    /// be given with a directory, e.g. `./host:1337`.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err("empty log output".into());
        }
        if !value.contains('/') {
            if let Some((host, port)) = value.rsplit_once(':') {
                if !host.is_empty() && !port.is_empty() && port.parse::<u16>().is_ok() {
                    return Ok(LogOutput::Socket(value.into()));
                }
            }
        }
        Ok(LogOutput::File(value.into()))
    }
}

impl fmt::Display for LogOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogOutput::File(path) => write!(f, "{}", path.display()),
            LogOutput::Socket(address) => write!(f, "{address}"),
        }
    }
}

impl LogOutput {
    /// Opens the destination as a writer for the logger.
    pub fn open(&self) -> Result<Box<dyn Write + Send>> {
        match self {
            LogOutput::File(path) => {
                // Append mode: each record is a single write(2) with O_APPEND,
                // so several instances can share one log file.
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .wrap_err_with(|| format!("Cannot open log file {}", path.display()))?;
                Ok(Box::new(file))
            }
            LogOutput::Socket(address) => Ok(Box::new(SocketWriter::new(address.clone())?)),
        }
    }
}

/// Sends log records to a TCP socket (e.g. `nc -lk 1337`).
///
/// The connection is (re-)established on demand, so the listener can be
/// started after the proxy or restarted at any time. While no connection is
/// available, the most recent records are kept in a bounded backlog that is
/// sent once connected. Write errors are never reported to the logger, so
/// logging never fails or blocks the proxy for long.
pub struct SocketWriter {
    address: String,
    stream: Option<TcpStream>,
    last_attempt: Option<Instant>,
    backlog: VecDeque<Vec<u8>>,
    backlog_bytes: usize,
}

impl SocketWriter {
    /// Upper limit of buffered log output while disconnected.
    const BACKLOG_LIMIT: usize = 1 << 20;
    /// Minimum time between two connection attempts.
    const RETRY_INTERVAL: Duration = Duration::from_secs(1);
    const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
    const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

    /// Checks that `address` resolves and tries to connect once. A failed
    /// connection is not an error: records are buffered until a listener
    /// accepts the connection.
    pub fn new(address: String) -> Result<Self> {
        address
            .to_socket_addrs()
            .wrap_err_with(|| format!("Cannot resolve log output address {address}"))?;
        let mut writer = SocketWriter {
            address,
            stream: None,
            last_attempt: None,
            backlog: VecDeque::new(),
            backlog_bytes: 0,
        };
        writer.connect();
        Ok(writer)
    }

    fn connect(&mut self) {
        self.last_attempt = Some(Instant::now());
        let Ok(addresses) = self.address.to_socket_addrs() else { return };
        for address in addresses {
            if let Ok(stream) = TcpStream::connect_timeout(&address, Self::CONNECT_TIMEOUT) {
                let _ = stream.set_write_timeout(Some(Self::WRITE_TIMEOUT));
                let _ = stream.set_nodelay(true);
                self.stream = Some(stream);
                return;
            }
        }
    }

    fn buffer(&mut self, record: Vec<u8>) {
        self.backlog_bytes += record.len();
        self.backlog.push_back(record);
        while self.backlog_bytes > Self::BACKLOG_LIMIT {
            match self.backlog.pop_front() {
                Some(dropped) => self.backlog_bytes -= dropped.len(),
                None => break,
            }
        }
    }

    /// Sends the backlog, keeping everything not sent if the connection fails.
    fn send_backlog(&mut self) {
        while let Some(record) = self.backlog.front() {
            let Some(stream) = self.stream.as_mut() else { return };
            if stream.write_all(record).is_err() {
                self.stream = None;
                return;
            }
            self.backlog_bytes -= record.len();
            self.backlog.pop_front();
        }
    }
}

impl Write for SocketWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer(buf.to_vec());
        if self.stream.is_none()
            && self.last_attempt.is_none_or(|t| t.elapsed() >= Self::RETRY_INTERVAL)
        {
            self.connect();
        }
        self.send_backlog();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(stream) = self.stream.as_mut() {
            if stream.flush().is_err() {
                self.stream = None;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    #[test]
    fn parses_sockets_and_files() {
        let socket = |s: &str| LogOutput::Socket(s.into());
        let file = |s: &str| LogOutput::File(s.into());
        assert_eq!("localhost:1337".parse(), Ok(socket("localhost:1337")));
        assert_eq!("127.0.0.1:1337".parse(), Ok(socket("127.0.0.1:1337")));
        assert_eq!("[::1]:1337".parse(), Ok(socket("[::1]:1337")));
        assert_eq!("fiasco-lsp.log".parse(), Ok(file("fiasco-lsp.log")));
        assert_eq!("/tmp/fiasco-lsp.log".parse(), Ok(file("/tmp/fiasco-lsp.log")));
        assert_eq!("./localhost:1337".parse(), Ok(file("./localhost:1337")));
        assert_eq!("log:x".parse(), Ok(file("log:x")));
        assert_eq!("log:99999".parse(), Ok(file("log:99999")));
        assert_eq!(":1337".parse(), Ok(file(":1337")));
        assert!("".parse::<LogOutput>().is_err());
    }

    #[test]
    fn socket_writer_buffers_until_connected() {
        // Reserve a free port, then release it so the first attempt fails.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let address = format!("127.0.0.1:{port}");
        let mut writer = SocketWriter::new(address.clone()).unwrap();
        assert!(writer.stream.is_none());
        writer.write_all(b"first\n").unwrap();

        let listener = TcpListener::bind(&address).unwrap();
        writer.last_attempt = None;
        writer.write_all(b"second\n").unwrap();
        writer.flush().unwrap();
        let (connection, _) = listener.accept().unwrap();
        let mut lines = BufReader::new(connection).lines();
        assert_eq!(lines.next().unwrap().unwrap(), "first");
        assert_eq!(lines.next().unwrap().unwrap(), "second");
        assert_eq!(writer.backlog_bytes, 0);
    }

    #[test]
    fn socket_writer_limits_backlog() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut writer = SocketWriter::new(format!("127.0.0.1:{port}")).unwrap();
        let record = vec![b'x'; 1024];
        for _ in 0..2048 {
            writer.write_all(&record).unwrap();
        }
        assert!(writer.backlog_bytes <= SocketWriter::BACKLOG_LIMIT);
        assert_eq!(writer.backlog.len(), SocketWriter::BACKLOG_LIMIT / record.len());
    }
}
