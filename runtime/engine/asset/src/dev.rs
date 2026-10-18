//! The dev source: units straight from the cook service (`marlin-cookd`), cooked on demand,
//! with change notifications for hot reload (0003 §6).
//!
//! # Protocol ([`wire`] frames)
//!
//! | message            | payload                                   |
//! |--------------------|-------------------------------------------|
//! | → `HELLO`          | platform                                  |
//! | ← `OBJECTS`        | n:u32, n × (uuid:u128 unit:u64) sorted by uuid; m:u32, m × (name:u64 uuid:u128) sorted by name |
//! | → `UNIT`           | unit:u64                                  |
//! | ← `UNIT_DATA`      | the cooked unit                           |
//! | ← `UNIT_FAILED`    | message                                   |
//! | ← `CHANGED` (id 0) | units: n:u32 n × u64, then `OBJECTS`' table when objects moved, else nothing |
//! | ← `ERROR`          | message                                   |
//!
//! The client learns every object's unit and the named objects at connect, so lookups never
//! wait on the network; units are requested as the asset server needs them and arrive whenever
//! cookd has cooked them (cache hits at once). `CHANGED` lists units whose cook keys changed
//! since the client got them, so it reloads exactly those.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::SocketAddr;
use core::time::Duration;

use marlin_containers::{HashMap, HashMapExt, HashSet, HashSetExt};
use marlin_net::{Completed, IoBuffer, NetQueue, SocketId, SocketOptions};
use marlin_sync::Mutex;

use crate::Uuid;
use crate::vfs::{Read, SourceError, UNIT_ALIGN, UnitBytes, UnitSource};
use crate::wire::{self, FrameReader, Reader, Writer};

pub const HELLO: u16 = 1;
pub const UNIT: u16 = 2;
pub const OBJECTS: u16 = 0x100;
pub const UNIT_DATA: u16 = 0x101;
pub const UNIT_FAILED: u16 = 0x102;
pub const ERROR: u16 = 0x1ff;
pub const CHANGED: u16 = 0x200;

/// Default cookd port.
pub const PORT: u16 = 47_200;

/// What a client finds objects by: every object's unit, and the named objects (0003 §4.4).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ObjectTable {
    /// (uuid, unit), sorted by UUID.
    pub objects: Vec<(Uuid, u64)>,
    /// ([`crate::name_hash`], uuid), sorted by name hash.
    pub names: Vec<(u64, Uuid)>,
}

/// An object table payload.
pub fn table_payload(table: &ObjectTable) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(table.objects.len() as u32);
    for &(uuid, unit) in &table.objects {
        w.uuid(uuid).u64(unit);
    }
    w.u32(table.names.len() as u32);
    for &(name, uuid) in &table.names {
        w.u64(name).uuid(uuid);
    }
    w.finish()
}

pub fn read_table(r: &mut Reader<'_>) -> Result<ObjectTable, wire::WireError> {
    let n = r.u32()? as usize;
    let mut objects = Vec::with_capacity(n.min(r.remaining() / 24));
    for _ in 0..n {
        objects.push((r.uuid()?, r.u64()?));
    }
    let n = r.u32()? as usize;
    let mut names = Vec::with_capacity(n.min(r.remaining() / 24));
    for _ in 0..n {
        names.push((r.u64()?, r.uuid()?));
    }
    if !objects.windows(2).all(|w| w[0].0 < w[1].0) || !names.windows(2).all(|w| w[0].0 < w[1].0) {
        return Err(wire::WireError::Malformed);
    }
    Ok(ObjectTable { objects, names })
}

/// A `CHANGED` payload.
pub fn changed_payload(units: &[u64], table: Option<&ObjectTable>) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(units.len() as u32);
    for &u in units {
        w.u64(u);
    }
    if let Some(table) = table {
        w.raw(&table_payload(table));
    }
    w.finish()
}

const RECV_SIZE: usize = 1 << 20;

struct State {
    queue: NetQueue,
    socket: SocketId,
    reader: FrameReader,
    next_id: u32,
    table: ObjectTable,
    units: HashSet<u64>,
    /// Request id -> unit.
    pending: HashMap<u32, u64>,
    arrived: HashMap<u64, Result<UnitBytes, SourceError>>,
    invalidated: Vec<u64>,
    outgoing: Vec<Vec<u8>>,
    sending: bool,
    /// The connection failed: every request fails with this.
    broken: Option<String>,
    /// The object table arrived.
    ready: bool,
}

/// A [`UnitSource`] backed by a cook service connection.
pub struct DevSource {
    state: Mutex<State>,
}

fn net(e: marlin_net::Error) -> SourceError {
    SourceError::Remote(e.to_string())
}

impl State {
    fn index(&mut self, table: ObjectTable) {
        self.units = table.objects.iter().map(|o| o.1).collect();
        self.table = table;
    }

    fn send(&mut self, kind: u16, id: u32, payload: &[u8]) {
        self.outgoing.push(wire::frame(kind, id, payload));
        self.flush();
    }

    fn flush(&mut self) {
        if self.sending || self.outgoing.is_empty() || self.broken.is_some() {
            return;
        }
        let bytes: Vec<u8> = self.outgoing.drain(..).flatten().collect();
        let mut buffer = IoBuffer::new(bytes.len());
        buffer.copy_from_slice(&bytes);
        match self.queue.send(self.socket, buffer, ..) {
            Ok(_) => self.sending = true,
            Err(e) => self.fail(e.error.to_string()),
        }
    }

    fn fail(&mut self, message: String) {
        if self.broken.is_none() {
            for (_, unit) in self.pending.drain() {
                self.arrived
                    .insert(unit, Err(SourceError::Remote(message.clone())));
            }
            self.broken = Some(message);
        }
    }

    /// Handles everything that has completed, waiting up to `timeout` for the first.
    fn pump(&mut self, timeout: Option<Duration>) {
        let mut first = timeout;
        loop {
            let done = match first.take() {
                Some(t) => self.queue.wait_timeout(t),
                None => self.queue.poll(),
            };
            match done {
                Ok(Some(c)) => self.complete(c),
                Ok(None) => break,
                Err(e) => {
                    self.fail(e.to_string());
                    break;
                }
            }
        }
        self.flush();
    }

    fn complete(&mut self, done: Completed) {
        match done {
            Completed::Sent { result, .. } => {
                self.sending = false;
                if let Err(e) = result {
                    self.fail(e.to_string());
                }
            }
            Completed::Received {
                buffer,
                start,
                result,
                ..
            } => match result {
                Ok(n) if n > 0 => {
                    self.reader.push(&buffer[start..start + n]);
                    loop {
                        match self.reader.next_frame() {
                            Ok(Some(frame)) => self.message(frame),
                            Ok(None) => break,
                            Err(e) => {
                                self.fail(e.to_string());
                                return;
                            }
                        }
                    }
                    if let Err(e) = self.queue.recv(self.socket, buffer, ..) {
                        self.fail(e.error.to_string());
                    }
                }
                Ok(_) => self.fail("the cook service closed the connection".into()),
                Err(e) => self.fail(e.to_string()),
            },
            _ => {}
        }
    }

    fn message(&mut self, frame: wire::Frame) {
        let mut r = Reader::new(&frame.payload);
        match frame.kind {
            UNIT_DATA => {
                if let Some(unit) = self.pending.remove(&frame.id) {
                    self.arrived
                        .insert(unit, Ok(UnitBytes::copy_from(&frame.payload, UNIT_ALIGN)));
                }
            }
            UNIT_FAILED | ERROR => {
                let message = r.str().unwrap_or_default();
                match self.pending.remove(&frame.id) {
                    Some(unit) => {
                        self.arrived.insert(unit, Err(SourceError::Remote(message)));
                    }
                    None => self.fail(message),
                }
            }
            CHANGED => {
                let parsed = (|| {
                    let n = r.u32()? as usize;
                    let mut units = Vec::with_capacity(n.min(r.remaining() / 8));
                    for _ in 0..n {
                        units.push(r.u64()?);
                    }
                    let table = if r.remaining() > 0 {
                        Some(read_table(&mut r)?)
                    } else {
                        None
                    };
                    r.end()?;
                    Ok::<_, wire::WireError>((units, table))
                })();
                match parsed {
                    Ok((units, table)) => {
                        if let Some(table) = table {
                            self.index(table);
                        }
                        self.invalidated.extend(units);
                    }
                    Err(e) => self.fail(e.to_string()),
                }
            }
            OBJECTS => match read_table(&mut r) {
                Ok(table) => {
                    self.index(table);
                    self.ready = true;
                }
                Err(e) => self.fail(e.to_string()),
            },
            _ => {}
        }
    }
}

impl DevSource {
    /// Connects to the cook service at `addr` as a client of `platform`, and receives the
    /// object table. Blocks up to `timeout` for that.
    pub fn connect(
        addr: SocketAddr,
        platform: &str,
        timeout: Duration,
    ) -> Result<Self, SourceError> {
        let mut queue = NetQueue::new(64).map_err(net)?;
        let (socket, _) = queue
            .connect(addr, &SocketOptions::default())
            .map_err(net)?;
        match queue.wait_timeout(timeout).map_err(net)? {
            Some(Completed::Connected { result, .. }) => result.map_err(net)?,
            _ => {
                return Err(SourceError::Remote(
                    "connecting to the cook service timed out".into(),
                ));
            }
        }
        let mut state = State {
            queue,
            socket,
            reader: FrameReader::new(),
            next_id: 1,
            table: ObjectTable::default(),
            units: HashSet::new(),
            pending: HashMap::new(),
            arrived: HashMap::new(),
            invalidated: Vec::new(),
            outgoing: Vec::new(),
            sending: false,
            broken: None,
            ready: false,
        };
        state
            .queue
            .recv(socket, IoBuffer::new(RECV_SIZE), ..)
            .map_err(|e| net(e.error))?;
        let mut hello = Writer::new();
        hello.str(platform);
        state.send(HELLO, 0, &hello.finish());
        // Wait for the object table.
        let mut waited = Duration::ZERO;
        let step = Duration::from_millis(10);
        while !state.ready {
            if let Some(e) = &state.broken {
                return Err(SourceError::Remote(e.clone()));
            }
            if waited >= timeout {
                return Err(SourceError::Remote(
                    "the cook service did not answer".into(),
                ));
            }
            state.pump(Some(step));
            waited += step;
        }
        Ok(Self {
            state: Mutex::new(state),
        })
    }

    /// Objects the cook service knows.
    pub fn object_count(&self) -> usize {
        self.state.lock().table.objects.len()
    }

    /// Whether the connection broke (requests then fail); the reason.
    pub fn error(&self) -> Option<String> {
        self.state.lock().broken.clone()
    }
}

impl UnitSource for DevSource {
    fn contains(&self, unit: u64) -> bool {
        self.state.lock().units.contains(&unit)
    }

    fn object_unit(&self, uuid: Uuid) -> Option<u64> {
        let s = self.state.lock();
        s.table
            .objects
            .binary_search_by_key(&uuid, |o| o.0)
            .ok()
            .map(|i| s.table.objects[i].1)
    }

    fn named(&self, name: u64) -> Option<Uuid> {
        let s = self.state.lock();
        s.table
            .names
            .binary_search_by_key(&name, |n| n.0)
            .ok()
            .map(|i| s.table.names[i].1)
    }

    fn begin(&self, unit: u64) -> Result<Read<'_>, SourceError> {
        let mut s = self.state.lock();
        if let Some(e) = &s.broken {
            return Err(SourceError::Remote(e.clone()));
        }
        let id = s.next_id;
        s.next_id = s.next_id.wrapping_add(1).max(1);
        s.pending.insert(id, unit);
        let mut w = Writer::new();
        w.u64(unit);
        s.send(UNIT, id, &w.finish());
        Ok(Read::Pending)
    }

    fn finish(&self, _unit: u64, _read: &[u8]) -> Result<UnitBytes, SourceError> {
        Err(SourceError::NotFound)
    }

    fn poll(&self, unit: u64) -> Option<Result<UnitBytes, SourceError>> {
        let mut s = self.state.lock();
        s.pump(None);
        s.arrived.remove(&unit)
    }

    fn wait(&self, timeout: Duration) {
        let mut s = self.state.lock();
        if s.arrived.is_empty() && s.broken.is_none() {
            s.pump(Some(timeout));
        }
    }

    fn take_invalidated(&self) -> Vec<u64> {
        let mut s = self.state.lock();
        s.pump(None);
        core::mem::take(&mut s.invalidated)
    }
}
