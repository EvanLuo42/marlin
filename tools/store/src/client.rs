//! Clients: a framed connection, the store client, and [`RemoteDdc`].

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::Duration;

use marlin_asset::wire::{self, Frame, FrameReader, Reader, WireError};
use marlin_cook_core::Ddc;
use marlin_net::{SocketOptions, TcpStream};

use crate::protocol::*;

#[derive(Debug, marlin_error::Error)]
pub enum ClientError {
    #[error("network: {0}")]
    Net(marlin_net::Error),
    #[error("protocol: {0}")]
    Wire(WireError),
    #[error("the server says: {0}")]
    Server(String),
    #[error("unexpected response {0}")]
    Unexpected(u16),
}

impl From<marlin_net::Error> for ClientError {
    fn from(e: marlin_net::Error) -> Self {
        Self::Net(e)
    }
}

impl From<WireError> for ClientError {
    fn from(e: WireError) -> Self {
        Self::Wire(e)
    }
}

impl From<ClientError> for io::Error {
    fn from(e: ClientError) -> Self {
        io::Error::other(e.to_string())
    }
}

/// A blocking framed connection with pipelining: send several requests, then receive their
/// responses (in whatever order they come).
pub struct Connection {
    stream: TcpStream,
    reader: FrameReader,
    next_id: u32,
    scratch: Vec<u8>,
}

impl Connection {
    pub fn connect(addr: SocketAddr, timeout: Option<Duration>) -> Result<Self, ClientError> {
        let stream = TcpStream::connect_with(addr, &SocketOptions::default(), timeout)?;
        Ok(Self {
            stream,
            reader: FrameReader::new(),
            next_id: 1,
            scratch: vec![0; 256 << 10],
        })
    }

    /// Sends a request; returns its id.
    pub fn send(&mut self, kind: u16, payload: &[u8]) -> Result<u32, ClientError> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.stream.send_all(&wire::frame(kind, id, payload))?;
        Ok(id)
    }

    /// Sends several requests in one write; returns their ids.
    pub fn send_many(&mut self, requests: &[(u16, Vec<u8>)]) -> Result<Vec<u32>, ClientError> {
        let mut bytes = Vec::new();
        let mut ids = Vec::with_capacity(requests.len());
        for (kind, payload) in requests {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            wire::encode(&mut bytes, *kind, id, payload);
            ids.push(id);
        }
        self.stream.send_all(&bytes)?;
        Ok(ids)
    }

    /// The next frame from the server.
    pub fn recv(&mut self) -> Result<Frame, ClientError> {
        loop {
            if let Some(frame) = self.reader.next_frame()? {
                return Ok(frame);
            }
            let n = self.stream.recv(&mut self.scratch)?;
            if n == 0 {
                return Err(ClientError::Net(marlin_net::Error::from_kind(
                    marlin_net::ErrorKind::UnexpectedEof,
                )));
            }
            self.reader.push(&self.scratch[..n]);
        }
    }

    /// One request, its response.
    pub fn call(&mut self, kind: u16, payload: &[u8]) -> Result<Frame, ClientError> {
        let id = self.send(kind, payload)?;
        loop {
            let frame = self.recv()?;
            if frame.id == id {
                return Ok(frame);
            }
        }
    }

    /// Pipelined requests; responses in request order.
    pub fn call_many(&mut self, requests: &[(u16, Vec<u8>)]) -> Result<Vec<Frame>, ClientError> {
        let ids = self.send_many(requests)?;
        let mut by_id: HashMap<u32, Frame> = HashMap::with_capacity(ids.len());
        while by_id.len() < ids.len() {
            let frame = self.recv()?;
            by_id.insert(frame.id, frame);
        }
        Ok(ids
            .iter()
            .map(|id| by_id.remove(id).expect("received"))
            .collect())
    }
}

fn server_error(frame: &Frame) -> ClientError {
    match Reader::new(&frame.payload).str() {
        Ok(message) if frame.kind == ERROR => ClientError::Server(message),
        _ => ClientError::Unexpected(frame.kind),
    }
}

/// A connection to a `marlin-store`.
pub struct StoreClient {
    conn: Connection,
}

impl StoreClient {
    pub fn connect(addr: SocketAddr) -> Result<Self, ClientError> {
        Ok(Self {
            conn: Connection::connect(addr, Some(Duration::from_secs(30)))?,
        })
    }

    pub fn get(&mut self, k: u128) -> Result<Option<Vec<u8>>, ClientError> {
        let f = self.conn.call(GET, &key(k))?;
        match f.kind {
            OK => Ok(Some(f.payload)),
            MISSING => Ok(None),
            _ => Err(server_error(&f)),
        }
    }

    /// Several gets, pipelined.
    pub fn get_many(&mut self, keys: &[u128]) -> Result<Vec<Option<Vec<u8>>>, ClientError> {
        let requests: Vec<(u16, Vec<u8>)> = keys.iter().map(|&k| (GET, key(k))).collect();
        self.conn
            .call_many(&requests)?
            .into_iter()
            .map(|f| match f.kind {
                OK => Ok(Some(f.payload)),
                MISSING => Ok(None),
                _ => Err(server_error(&f)),
            })
            .collect()
    }

    pub fn has(&mut self, k: u128) -> Result<bool, ClientError> {
        let f = self.conn.call(HAS, &key(k))?;
        match f.kind {
            OK => Ok(true),
            MISSING => Ok(false),
            _ => Err(server_error(&f)),
        }
    }

    /// Which of `keys` the store has, in one round trip.
    pub fn has_many(&mut self, keys: &[u128]) -> Result<Vec<bool>, ClientError> {
        let items: Vec<(u16, Vec<u8>)> = keys.iter().map(|&k| (HAS, key(k))).collect();
        let f = self.conn.call(BATCH, &batch(&items))?;
        if f.kind != OK {
            return Err(server_error(&f));
        }
        let answers = read_batch(&f.payload)?;
        if answers.len() != keys.len() {
            return Err(ClientError::Wire(WireError::Malformed));
        }
        Ok(answers.iter().map(|(kind, _)| *kind == OK).collect())
    }

    pub fn put(&mut self, k: u128, data: &[u8]) -> Result<(), ClientError> {
        let f = self.conn.call(PUT, &put(k, data))?;
        if f.kind == OK {
            Ok(())
        } else {
            Err(server_error(&f))
        }
    }

    pub fn ref_get(&mut self, ref_name: &str) -> Result<Option<u128>, ClientError> {
        let f = self.conn.call(REF_GET, &name(ref_name))?;
        match f.kind {
            OK => {
                let mut r = Reader::new(&f.payload);
                let k = r.u128()?;
                r.end()?;
                Ok(Some(k))
            }
            MISSING => Ok(None),
            _ => Err(server_error(&f)),
        }
    }

    /// Sets a ref; with `expected`, only if it still points there (`Err(current)` if not).
    pub fn ref_set(
        &mut self,
        ref_name: &str,
        k: u128,
        expected: Option<u128>,
    ) -> Result<Result<(), Option<u128>>, ClientError> {
        let f = self.conn.call(REF_SET, &ref_set(ref_name, k, expected))?;
        match f.kind {
            OK => Ok(Ok(())),
            CONFLICT => {
                let mut r = Reader::new(&f.payload);
                Ok(Err(if r.remaining() > 0 {
                    Some(r.u128()?)
                } else {
                    None
                }))
            }
            _ => Err(server_error(&f)),
        }
    }

    /// Entries, bytes and refs.
    pub fn stats(&mut self) -> Result<(u64, u64, u64), ClientError> {
        let f = self.conn.call(STATS, &[])?;
        if f.kind != OK {
            return Err(server_error(&f));
        }
        let mut r = Reader::new(&f.payload);
        Ok((r.u64()?, r.u64()?, r.u64()?))
    }

    pub fn connection(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

/// The shared store as a [`Ddc`]. Reconnects once when a request fails; a store that cannot be
/// reached reads as empty and refuses writes, so cooking goes on locally.
pub struct RemoteDdc {
    addr: SocketAddr,
    client: Mutex<Option<StoreClient>>,
}

impl RemoteDdc {
    pub fn connect(addr: SocketAddr) -> Result<Self, ClientError> {
        let client = StoreClient::connect(addr)?;
        Ok(Self {
            addr,
            client: Mutex::new(Some(client)),
        })
    }

    fn with<T>(
        &self,
        f: impl Fn(&mut StoreClient) -> Result<T, ClientError>,
    ) -> Result<T, ClientError> {
        let mut guard = self.client.lock().unwrap_or_else(|e| e.into_inner());
        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some(StoreClient::connect(self.addr)?);
            }
            match f(guard.as_mut().expect("connected")) {
                Ok(v) => return Ok(v),
                Err(ClientError::Net(e)) if attempt == 0 => {
                    let _ = e;
                    *guard = None;
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!()
    }
}

impl Ddc for RemoteDdc {
    fn get(&self, key: u128) -> io::Result<Option<Vec<u8>>> {
        Ok(self.with(|c| c.get(key))?)
    }

    fn contains(&self, key: u128) -> bool {
        self.with(|c| c.has(key)).unwrap_or(false)
    }

    fn put(&self, key: u128, data: &[u8]) -> io::Result<()> {
        Ok(self.with(|c| c.put(key, data))?)
    }
}
