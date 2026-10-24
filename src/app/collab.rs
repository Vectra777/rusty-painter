//! Painting together on one canvas. One painter hosts: their app is the
//! server, as with Minecraft's "Open to LAN". The others join with an
//! invite code, either directly or through a relay (`src/bin/relay.rs`)
//! when the host can't be reached.
//!
//! What goes over the wire is pixels, not strokes. Strokes don't repeat
//! exactly (random seeds, wall-clock dynamics), and fills and filters are
//! closures, so after each step the pixels it changed are sent and the
//! others write them over theirs. Two painters in the same tile both keep
//! their paint; where both changed the same pixel, the host's order
//! decides. Every few seconds the host sends its tiles' checksums, and a
//! guest whose tile differs gets it whole.
//!
//! Undo takes back only this painter's own pixels. Where someone painted
//! over them since, their paint stays.
//!
//! Layer changes (added, removed, moved, their settings, text and vector
//! sources) go as the layer list without pixels, which each painter takes
//! keeping their own pixels and undo history. Only resizing, cropping,
//! turning and merging send the whole document, and those can't be undone
//! while painting together (it would take back everyone's work since).
//!
//! Others' strokes are painted here as they paint them, with their brush
//! (see [`replay`]); the pixels they send when the stroke ends are what's
//! kept.
//!
//! The session belongs to one document. On another tab, what arrives
//! waits for coming back to it.
//!
//! The connection is TLS end to end, so a relay sees nothing. The host's
//! certificate is made when hosting starts, and the invite carries its
//! SHA-256, which the guest checks the certificate against.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Vec2};
use serde::{Deserialize, Serialize};

mod replay;

use crate::PainterApp;
use crate::app::view::render::ScreenMap;
use crate::canvas::Canvas;
use crate::canvas::history::{History, LayerHistoryOp, TileSnapshot, UndoAction};
use crate::canvas::storage::{Depth, LayerId};

/// The largest message taken (a whole document, as a project file).
const MAX_FRAME: usize = 512 << 20;
/// How often the host sends checksums of the tiles changed since.
const HASH_EVERY: Duration = Duration::from_secs(3);
/// How often a moving cursor is sent.
const CURSOR_EVERY: Duration = Duration::from_millis(33);
/// How long a connection thread waits for data before checking its outbox.
const POLL: Duration = Duration::from_millis(10);
/// How long a connection may take to say who it is.
const HANDSHAKE: Duration = Duration::from_secs(15);
/// The name the certificate is made out to (the invite's fingerprint is
/// what's checked, not this).
const SERVER_NAME: &str = "rusty-painter";
/// Tiles one checksum request may ask for.
const MAX_WANT: usize = 4096;
/// The port a host listens on unless changed.
pub(crate) const DEFAULT_PORT: u16 = 7878;
/// Each painter's new layers get ids from their own range (`id << 40`), so
/// two adding layers at once don't make the same one.
const ID_RANGE: u32 = 40;
/// Guests one session takes: their ranges stay below the project format's
/// largest layer id (2^53).
const MAX_GUESTS: u32 = 8000;
/// Messages a guest away on another tab keeps; past that it asks for the
/// whole document on coming back.
const MAX_HELD_AWAY: usize = 20_000;

/// A message's header; any pixels or document ride after it as its blob.
#[derive(Serialize, Deserialize, Debug, Clone)]
enum Head {
    Hello {
        name: String,
        password: String,
    },
    Welcome {
        you: u32,
    },
    Refused(String),
    /// Who's painting: ids and names, the host first.
    Users(Vec<(u32, String)>),
    /// The whole document (the blob: a project file, no history).
    Snapshot,
    /// The layer list without pixels (the blob: a project file), the
    /// layers the sender removed (others' it doesn't list stay: added
    /// meanwhile), and where vector layers' lines changed, to draw again
    /// from them.
    Layers {
        removed: Vec<u64>,
        redraw: Vec<(u64, [i32; 4])>,
    },
    /// A guest back from another tab, having missed too much: the whole
    /// document, please.
    Resync,
    /// Pixels a step changed (the blob: see [`encode_patches`]). `seq`
    /// counts the sender's changes, for the checksums.
    Change {
        from: u32,
        seq: u64,
        tiles: Vec<TileHead>,
    },
    /// Whole tiles a guest asked for, to apply only if the host had all its
    /// changes when it sent them (`applied` is the guest's last it had).
    Fix {
        applied: u64,
        tiles: Vec<TileHead>,
    },
    /// Strokes being painted, as they're painted (the blob: a brush, as a
    /// preset file, when one begins with a new one).
    Stroke {
        from: u32,
        events: Vec<replay::StrokeEvent>,
    },
    Cursor {
        from: u32,
        pos: [f32; 2],
        down: bool,
        color: [u8; 4],
        size: f32,
    },
    /// Checksums of the tiles changed lately; `applied` is the recipient's
    /// last change the host had then.
    Hashes {
        applied: u64,
        tiles: Vec<(u64, i32, i32, u32)>,
    },
    Want(Vec<(u64, i32, i32)>),
}

/// Where a patch goes: a rectangle of one tile of one layer.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
struct TileHead {
    layer: u64,
    tx: i32,
    ty: i32,
    x0: u16,
    y0: u16,
    w: u16,
    h: u16,
}

impl TileHead {
    fn of(snapshot: &TileSnapshot) -> Self {
        Self {
            layer: snapshot.layer_id.0,
            tx: snapshot.tx,
            ty: snapshot.ty,
            x0: snapshot.x0 as u16,
            y0: snapshot.y0 as u16,
            w: snapshot.width as u16,
            h: snapshot.height as u16,
        }
    }

    fn whole(layer: u64, tx: i32, ty: i32, tile_size: usize) -> Self {
        let ts = tile_size as u16;
        Self {
            layer,
            tx,
            ty,
            x0: 0,
            y0: 0,
            w: ts,
            h: ts,
        }
    }

    fn len(&self) -> usize {
        self.w as usize * self.h as usize
    }
}

/// A rectangle's pixels and which of them to write.
#[derive(Clone, Debug, PartialEq)]
struct Patch {
    at: TileHead,
    mask: Vec<bool>,
    px: Vec<Color32>,
}

impl Patch {
    fn changes(&self) -> bool {
        self.mask.iter().any(|&m| m)
    }
}

/// What the connection threads tell the app.
enum Net {
    /// A guest got in (the host's side), or the host let us in (a guest's,
    /// id 0): where to send to it.
    Joined {
        id: u32,
        name: String,
        tx: Sender<Arc<Vec<u8>>>,
    },
    Msg {
        id: u32,
        head: Head,
        blob: Vec<u8>,
    },
    Left {
        id: u32,
        why: String,
    },
    /// Something to show (the relay refused the room, …).
    Status(String),
}

struct Peer {
    id: u32,
    tx: Sender<Arc<Vec<u8>>>,
}

/// Another painter's cursor and the stroke they're painting.
struct Remote {
    pos: Vec2,
    down: bool,
    color: Color32,
    size: f32,
}

/// A painting session, hosted or joined.
pub(crate) struct Collab {
    host: bool,
    /// Our id: 0 for the host.
    me: u32,
    name: String,
    /// What guests join with (the host's).
    pub(crate) invite: Option<String>,
    /// The same through Tailscale, when this machine is on a tailnet (for
    /// guests the local network doesn't reach).
    pub(crate) tailnet_invite: Option<String>,
    pub(crate) status: String,
    rx: Receiver<Net>,
    stop: Arc<AtomicBool>,
    /// The host's guests, or a guest's host (id 0).
    peers: Vec<Peer>,
    pub(crate) users: Vec<(u32, String)>,
    /// A guest: the host's document has arrived (nothing is sent before).
    synced: bool,
    /// The session's document, by its tab's autosave slot (`None`: a guest
    /// whose document hasn't come yet).
    slot: Option<usize>,
    /// Ids of the layers the others have (those that weren't get their
    /// pixels sent whole).
    known: HashSet<u64>,
    /// The next id for a layer of ours.
    next_id: u64,
    /// The host: guests waiting for the document.
    waiting: Vec<u32>,
    /// A guest: missed too much while away (see [`MAX_HELD_AWAY`]).
    resync: bool,
    /// Messages held while a stroke is painted.
    held: VecDeque<(u32, Head, Vec<u8>)>,
    /// The local history's push count, as last seen.
    seen_push: u64,
    /// Per local undo (and redo) step, the pixels it left, in step with the
    /// history's stacks; `None` where not known (steps from before).
    left_undo: Vec<Option<Vec<Patch>>>,
    left_redo: Vec<Option<Vec<Patch>>>,
    /// Changes sent.
    sent: u64,
    /// The host: each guest's last change applied.
    applied: HashMap<u32, u64>,
    /// The host: tiles changed since the last checksums.
    touched: HashSet<(u64, i32, i32)>,
    last_hashes: Instant,
    last_cursor: Instant,
    cursor_sent: Option<(Vec2, bool)>,
    remotes: HashMap<u32, Remote>,
    /// This painter's strokes, as the stroke worker reports them.
    outgoing: Option<replay::Outgoing>,
    /// Others' strokes arrived, to paint.
    strokes_in: Vec<(u32, Vec<replay::StrokeEvent>, Vec<u8>)>,
    replays: replay::Replays,
}

impl Drop for Collab {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The Paint Together window's fields.
pub(crate) struct Form {
    pub open: bool,
    pub name: String,
    pub port: u16,
    pub relay: String,
    pub password: String,
    pub invite: String,
    pub error: Option<String>,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            open: false,
            name: "Painter".into(),
            port: DEFAULT_PORT,
            relay: String::new(),
            password: String::new(),
            invite: String::new(),
            error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Messages on the wire: a u32 length, a u32 header length, the header (JSON),
// the blob.

fn frame(head: &Head, blob: &[u8]) -> Arc<Vec<u8>> {
    let head = serde_json::to_vec(head).expect("headers serialize");
    let total = 4 + head.len() + blob.len();
    let mut out = Vec::with_capacity(4 + total);
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(head.len() as u32).to_le_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(blob);
    Arc::new(out)
}

/// The next whole message in `buf`, taken out of it.
fn take_frame(buf: &mut Vec<u8>) -> Result<Option<(Head, Vec<u8>)>, String> {
    let Some(len) = buf.first_chunk::<4>() else {
        return Ok(None);
    };
    let total = u32::from_le_bytes(*len) as usize;
    if !(4..=MAX_FRAME).contains(&total) {
        return Err("Bad message size".into());
    }
    if buf.len() < 4 + total {
        return Ok(None);
    }
    let body: Vec<u8> = buf.drain(..4 + total).skip(4).collect();
    let head_len = u32::from_le_bytes(body[..4].try_into().unwrap()) as usize;
    let Some(head) = body.get(4..4 + head_len) else {
        return Err("Bad message header".into());
    };
    let head = serde_json::from_slice(head).map_err(|e| format!("Bad message: {e}"))?;
    Ok(Some((head, body[4 + head_len..].to_vec())))
}

/// Patches as a blob: per patch, a byte per pixel (written or not), then
/// its pixels; zstd over all of it.
fn encode_patches(patches: &[Patch]) -> (Vec<TileHead>, Vec<u8>) {
    let mut raw = Vec::new();
    for p in patches {
        raw.extend(p.mask.iter().map(|&m| m as u8));
        for (&px, &m) in p.px.iter().zip(&p.mask) {
            raw.extend(if m { px.to_array() } else { [0; 4] });
        }
    }
    let blob = zstd::bulk::compress(&raw, 1).expect("zstd compresses in memory");
    (patches.iter().map(|p| p.at).collect(), blob)
}

fn decode_patches(heads: &[TileHead], blob: &[u8]) -> Result<Vec<Patch>, String> {
    let size: usize = heads.iter().map(|h| h.len() * 5).sum();
    if size > MAX_FRAME {
        return Err("Change too large".into());
    }
    let raw = zstd::bulk::decompress(blob, size).map_err(|e| format!("Bad change: {e}"))?;
    if raw.len() != size {
        return Err("Bad change size".into());
    }
    let mut at = 0;
    Ok(heads
        .iter()
        .map(|&head| {
            let n = head.len();
            let mask = raw[at..at + n].iter().map(|&b| b != 0).collect();
            let px = raw[at + n..at + 5 * n]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3]))
                .collect();
            at += 5 * n;
            Patch { at: head, mask, px }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Pixels.

/// The layer a patch goes to, if it fits the canvas (tiles may sit a little
/// off it: pixels moved past its edge are kept).
fn check(canvas: &Canvas, at: &TileHead) -> Option<usize> {
    let ts = canvas.tile_size();
    let fits = |o: u16, l: u16| l > 0 && o as usize + l as usize <= ts;
    let reach = |n: usize| (n.div_ceil(ts) + 64) as i32;
    let near = at.tx.abs() <= reach(canvas.width()) && at.ty.abs() <= reach(canvas.height());
    (fits(at.x0, at.w) && fits(at.y0, at.h) && near)
        .then(|| canvas.layer_index_of(LayerId(at.layer)))
        .flatten()
}

fn read_rect(canvas: &Canvas, idx: usize, at: &TileHead) -> Vec<Color32> {
    let ts = canvas.tile_size();
    let Some(tile) = canvas.get_layer_tile_data(idx, at.tx, at.ty) else {
        return vec![Color32::TRANSPARENT; at.len()];
    };
    let (x0, w) = (at.x0 as usize, at.w as usize);
    (0..at.h as usize)
        .flat_map(|row| {
            let start = (at.y0 as usize + row) * ts + x0;
            tile[start..start + w].iter().copied()
        })
        .collect()
}

/// Write `px` where `mask` says (the rest of the tile kept).
fn write_masked(canvas: &Canvas, idx: usize, at: &TileHead, mask: &[bool], px: &[Color32]) {
    let ts = canvas.tile_size();
    let mut tile = canvas
        .get_layer_tile_data(idx, at.tx, at.ty)
        .unwrap_or_else(|| vec![Color32::TRANSPARENT; ts * ts]);
    for (i, (&m, &p)) in mask.iter().zip(px).enumerate() {
        if m {
            let (x, y) = (i % at.w as usize, i / at.w as usize);
            tile[(at.y0 as usize + y) * ts + at.x0 as usize + x] = p;
        }
    }
    canvas.set_layer_tile_data(idx, at.tx, at.ty, tile);
}

/// The step's pixels as they are now, marked where they differ from before
/// it (its snapshot).
#[cfg(test)]
fn patches_of(canvas: &Canvas, step: &UndoAction) -> Vec<Patch> {
    patches_of_steps(canvas, &[step]).pop().unwrap_or_default()
}

/// Each of `steps`' pixels as it left them (oldest first), marked where
/// they differ from before it (its snapshot). The canvas now shows the
/// last one's; each before it left what the next ones' snapshots say was
/// there before them: worked out from the newest back, so where a later
/// step painted over an earlier one, the earlier keeps its own pixels.
fn patches_of_steps(canvas: &Canvas, steps: &[&UndoAction]) -> Vec<Vec<Patch>> {
    let ts = canvas.tile_size();
    // Tiles as they were after the step being worked out.
    let mut view: HashMap<(u64, i32, i32), Vec<Color32>> = HashMap::new();
    let mut out: Vec<Vec<Patch>> = steps
        .iter()
        .rev()
        .map(|step| {
            step.tiles
                .iter()
                .filter_map(|snapshot| {
                    let at = TileHead::of(snapshot);
                    let idx = check(canvas, &at)?;
                    let before = snapshot.data.to_vec();
                    if before.len() != at.len() {
                        return None;
                    }
                    let tile = view.entry((at.layer, at.tx, at.ty)).or_insert_with(|| {
                        (canvas.get_layer_tile_data(idx, at.tx, at.ty))
                            .unwrap_or_else(|| vec![Color32::TRANSPARENT; ts * ts])
                    });
                    let (x0, y0, w) = (at.x0 as usize, at.y0 as usize, at.w as usize);
                    let mut px = Vec::with_capacity(at.len());
                    for (i, &b) in before.iter().enumerate() {
                        let at_tile = (y0 + i / w) * ts + x0 + i % w;
                        px.push(tile[at_tile]);
                        // Before this step, for the ones before it.
                        tile[at_tile] = b;
                    }
                    Some(Patch {
                        at,
                        mask: px.iter().zip(&before).map(|(a, b)| a != b).collect(),
                        px,
                    })
                })
                .collect()
        })
        .collect();
    out.reverse();
    out
}

/// After an undo (or redo) put back `now`: only where this painter's step
/// changed the pixel (`left`'s mask) and it's still what it left there;
/// elsewhere the pixels just before it (`current`) stay: someone painted
/// since. `None` for `left`: not known, all put back.
fn keep_theirs(current: &[Color32], left: Option<&Patch>, now: &[Color32]) -> Vec<Color32> {
    match left {
        Some(left) if left.px.len() == now.len() && left.mask.len() == now.len() => now
            .iter()
            .zip(current)
            .zip(left.px.iter().zip(&left.mask))
            .map(|((&now, &c), (&l, &mine))| if mine && c == l { now } else { c })
            .collect(),
        _ => now.to_vec(),
    }
}

/// Someone else's pixels (`patches`) went over this painter's steps:
/// those pixels aren't theirs to undo any more.
fn disown(steps: &mut [Option<Vec<Patch>>], patches: &[Patch]) {
    for mine in steps.iter_mut().flatten().flatten() {
        for p in patches {
            let (a, b) = (&mine.at, &p.at);
            if (a.layer, a.tx, a.ty) != (b.layer, b.tx, b.ty) {
                continue;
            }
            for (i, &theirs) in p.mask.iter().enumerate() {
                let (x, y) = (
                    b.x0 as usize + i % b.w as usize,
                    b.y0 as usize + i / b.w as usize,
                );
                let (x0, y0) = (a.x0 as usize, a.y0 as usize);
                if theirs
                    && (x0..x0 + a.w as usize).contains(&x)
                    && (y0..y0 + a.h as usize).contains(&y)
                {
                    mine.mask[(y - y0) * a.w as usize + (x - x0)] = false;
                }
            }
        }
    }
}

/// 0 for a tile that's missing or clear.
fn tile_hash(canvas: &Canvas, idx: usize, tx: i32, ty: i32) -> u32 {
    match canvas.get_layer_tile_data(idx, tx, ty) {
        Some(tile) if tile.iter().any(|&p| p != Color32::TRANSPARENT) => {
            let bytes: Vec<u8> = tile.iter().flat_map(|p| p.to_array()).collect();
            crc32fast::hash(&bytes).max(1)
        }
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Invites and TLS.

/// Where to join and the certificate to expect: `rp://host:port#sha256`, or
/// `rp://relay:port/room#sha256` through a relay.
#[derive(Debug, PartialEq)]
struct Invite {
    addr: String,
    room: Option<String>,
    fingerprint: [u8; 32],
}

impl Invite {
    fn parse(s: &str) -> Result<Self, String> {
        let bad = || "That isn't an invite code".to_string();
        let rest = s.trim().strip_prefix("rp://").ok_or_else(bad)?;
        let (place, hex) = rest.split_once('#').ok_or_else(bad)?;
        let mut fingerprint = [0; 32];
        if hex.len() != 64 {
            return Err(bad());
        }
        for (i, b) in fingerprint.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
        }
        let (addr, room) = match place.split_once('/') {
            Some((addr, room)) if valid_room(room) => (addr, Some(room.to_string())),
            Some(_) => return Err(bad()),
            None => (place, None),
        };
        Ok(Self {
            addr: addr.to_string(),
            room,
            fingerprint,
        })
    }

    fn code(&self) -> String {
        let hex: String = self
            .fingerprint
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        match &self.room {
            Some(room) => format!("rp://{}/{room}#{hex}", self.addr),
            None => format!("rp://{}#{hex}", self.addr),
        }
    }
}

/// The relay takes room names of letters and digits.
fn valid_room(room: &str) -> bool {
    (1..=32).contains(&room.len()) && room.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().try_into().expect("SHA-256 is 32 bytes")
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A fresh certificate for hosting, and its fingerprint.
fn server_config() -> Result<(Arc<rustls::ServerConfig>, [u8; 32]), String> {
    let made = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_string()])
        .map_err(|e| format!("Certificate: {e}"))?;
    let cert = made.cert.der().clone();
    let fingerprint = sha256(&cert);
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(made.key_pair.serialize_der());
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key.into())
        .map_err(|e| e.to_string())?;
    Ok((Arc::new(config), fingerprint))
}

/// Trusts the one certificate the invite names, whoever it's made out to.
#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if sha256(end_entity) == self.fingerprint {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "The host isn't the one the invite is for".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 isn't used".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn client_config(fingerprint: [u8; 32]) -> Result<Arc<rustls::ClientConfig>, String> {
    let provider = provider();
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            fingerprint,
            provider,
        }))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

// ---------------------------------------------------------------------------
// Connections: a thread each, sending what the app queues and handing it
// what arrives.

/// A connection and what's been read of its next message.
struct Link<T> {
    io: T,
    buf: Vec<u8>,
}

impl<T: Read + Write> Link<T> {
    fn send(&mut self, frame: &[u8]) -> std::io::Result<()> {
        self.io.write_all(frame)?;
        self.io.flush()
    }

    /// The next whole message, or `None` if none came within the socket's
    /// read timeout.
    fn poll(&mut self) -> Result<Option<(Head, Vec<u8>)>, String> {
        if let Some(msg) = take_frame(&mut self.buf)? {
            return Ok(Some(msg));
        }
        let mut chunk = vec![0; 1 << 16];
        match self.io.read(&mut chunk) {
            Ok(0) => Err("Disconnected".into()),
            Ok(n) => {
                self.buf.extend_from_slice(&chunk[..n]);
                take_frame(&mut self.buf)
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn wait(&mut self, within: Duration) -> Result<(Head, Vec<u8>), String> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if let Some(msg) = self.poll()? {
                return Ok(msg);
            }
        }
        Err("No answer".into())
    }
}

/// Finish the TLS handshake (reads waiting as long as it takes), then
/// leave reads polling.
fn handshake<C, S>(tls: &mut rustls::StreamOwned<C, TcpStream>) -> Result<(), String>
where
    C: std::ops::DerefMut + std::ops::Deref<Target = rustls::ConnectionCommon<S>>,
    S: rustls::SideData,
{
    tls.sock
        .set_read_timeout(Some(HANDSHAKE))
        .map_err(|e| e.to_string())?;
    while tls.conn.is_handshaking() {
        tls.conn
            .complete_io(&mut tls.sock)
            .map_err(|e| e.to_string())?;
    }
    tls.sock
        .set_read_timeout(Some(POLL))
        .map_err(|e| e.to_string())
}

/// What hosting needs to let a guest in.
#[derive(Clone)]
struct Door {
    config: Arc<rustls::ServerConfig>,
    password: String,
    next_id: Arc<AtomicU32>,
    to_ui: Sender<Net>,
    ctx: egui::Context,
    stop: Arc<AtomicBool>,
}

/// Send what the app queues and pass on what arrives, until either side
/// goes.
fn run_link<T: Read + Write>(
    mut link: Link<T>,
    id: u32,
    out: &Receiver<Arc<Vec<u8>>>,
    to_ui: &Sender<Net>,
    ctx: &egui::Context,
    stop: &AtomicBool,
) {
    let why = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let sent = loop {
            match out.try_recv() {
                Ok(frame) => {
                    if let Err(e) = link.io.write_all(&frame) {
                        break Err(e.to_string());
                    }
                }
                Err(TryRecvError::Empty) => break link.io.flush().map_err(|e| e.to_string()),
                Err(TryRecvError::Disconnected) => return,
            }
        };
        if let Err(why) = sent {
            break why;
        }
        match link.poll() {
            Ok(Some((head, blob))) => {
                let _ = to_ui.send(Net::Msg { id, head, blob });
                ctx.request_repaint();
            }
            Ok(None) => {}
            Err(why) => break why,
        }
    };
    let _ = to_ui.send(Net::Left { id, why });
    ctx.request_repaint();
}

/// A guest knocking: TLS, then the password, then in.
fn let_in(stream: TcpStream, door: Door) {
    let result = (|| -> Result<(), String> {
        let conn = rustls::ServerConnection::new(door.config.clone()).map_err(|e| e.to_string())?;
        let mut tls = rustls::StreamOwned::new(conn, stream);
        handshake(&mut tls)?;
        let mut link = Link {
            io: tls,
            buf: Vec::new(),
        };
        let (Head::Hello { name, password }, _) = link.wait(HANDSHAKE)? else {
            return Err("No hello".into());
        };
        if password != door.password {
            let _ = link.send(&frame(&Head::Refused("Wrong password".into()), &[]));
            return Err(format!("{name}: wrong password"));
        }
        let id = door.next_id.fetch_add(1, Ordering::Relaxed);
        if id >= MAX_GUESTS {
            let _ = link.send(&frame(&Head::Refused("The session is full".into()), &[]));
            return Err("full".into());
        }
        link.send(&frame(&Head::Welcome { you: id }, &[]))
            .map_err(|e| e.to_string())?;
        let (tx, rx) = channel();
        let name: String = name.chars().take(32).collect();
        let _ = door.to_ui.send(Net::Joined { id, name, tx });
        door.ctx.request_repaint();
        run_link(link, id, &rx, &door.to_ui, &door.ctx, &door.stop);
        Ok(())
    })();
    if let Err(why) = result {
        log::info!("Paint together: a guest didn't get in: {why}");
    }
}

fn connect(addr: &str) -> Result<TcpStream, String> {
    let addrs = addr.to_socket_addrs().map_err(|e| format!("{addr}: {e}"))?;
    let mut last = format!("{addr}: no address");
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_secs(10)) {
            Ok(s) => return Ok(s),
            Err(e) => last = format!("{addr}: {e}"),
        }
    }
    Err(last)
}

/// A line of the relay's (read a byte at a time, so nothing after it is
/// taken from the stream).
fn read_line(s: &mut TcpStream) -> Result<String, String> {
    let mut line = Vec::new();
    let mut b = [0];
    while line.len() < 128 {
        match s.read(&mut b) {
            Ok(0) => return Err("The relay closed the connection".into()),
            Ok(_) if b[0] == b'\n' => return Ok(String::from_utf8_lossy(&line).into_owned()),
            Ok(_) => line.push(b[0]),
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("The relay said too much".into())
}

fn expect_ok(s: &mut TcpStream) -> Result<(), String> {
    s.set_read_timeout(Some(HANDSHAKE))
        .map_err(|e| e.to_string())?;
    match read_line(s)? {
        line if line == "OK" => Ok(()),
        line => Err(format!(
            "Relay: {}",
            line.strip_prefix("ERR ").unwrap_or(&line)
        )),
    }
}

/// Hosting through a relay: hold a room there, and open a connection back
/// for each guest it announces.
fn host_on_relay(relay: String, room: String, door: Door) {
    let result = (|| -> Result<(), String> {
        let mut control = connect(&relay)?;
        control
            .write_all(format!("HOST {room}\n").as_bytes())
            .map_err(|e| e.to_string())?;
        expect_ok(&mut control)?;
        control
            .set_read_timeout(Some(Duration::from_secs(1)))
            .map_err(|e| e.to_string())?;
        let (mut line, mut pinged) = (Vec::new(), Instant::now());
        while !door.stop.load(Ordering::Relaxed) {
            // (Keeps the room's connection from looking idle to routers.)
            if pinged.elapsed() > Duration::from_secs(30) {
                control.write_all(b"PING\n").map_err(|e| e.to_string())?;
                pinged = Instant::now();
            }
            let mut b = [0];
            match control.read(&mut b) {
                Ok(0) => return Err("The relay closed the room".into()),
                Ok(_) if b[0] == b'\n' => {
                    let text = String::from_utf8_lossy(&line).into_owned();
                    line.clear();
                    let Some(n) = text.strip_prefix("CONNECT ") else {
                        continue;
                    };
                    let accept = format!("ACCEPT {room} {n}\n");
                    let (relay, door) = (relay.clone(), door.clone());
                    std::thread::spawn(move || {
                        if let Ok(mut s) = connect(&relay)
                            && s.write_all(accept.as_bytes()).is_ok()
                        {
                            let_in(s, door);
                        }
                    });
                }
                Ok(_) if line.len() < 128 => line.push(b[0]),
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    })();
    if let Err(why) = result {
        let _ = door.to_ui.send(Net::Status(why));
        door.ctx.request_repaint();
    }
}

/// The address others on this network reach us at (no packet is sent: a
/// UDP "connection" just picks the route).
/// This machine's Tailscale address, when it's on a tailnet: the address
/// it would reach Tailscale's own resolver from (in 100.64.0.0/10).
fn tailnet_ip() -> Option<std::net::Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("100.100.100.100:53").ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64 => {
            Some(ip)
        }
        _ => None,
    }
}

fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map_or_else(|_| "127.0.0.1".into(), |a| a.ip().to_string())
}

impl Collab {
    fn new(host: bool, name: &str, rx: Receiver<Net>, stop: Arc<AtomicBool>) -> Self {
        let long_ago = Instant::now() - HASH_EVERY;
        Self {
            host,
            me: 0,
            name: name.chars().take(32).collect(),
            invite: None,
            tailnet_invite: None,
            status: String::new(),
            rx,
            stop,
            peers: Vec::new(),
            users: Vec::new(),
            synced: host,
            slot: None,
            known: HashSet::new(),
            next_id: 0,
            waiting: Vec::new(),
            resync: false,
            held: VecDeque::new(),
            seen_push: 0,
            left_undo: Vec::new(),
            left_redo: Vec::new(),
            sent: 0,
            applied: HashMap::new(),
            touched: HashSet::new(),
            last_hashes: long_ago,
            last_cursor: long_ago,
            cursor_sent: None,
            remotes: HashMap::new(),
            outgoing: None,
            strokes_in: Vec::new(),
            replays: Default::default(),
        }
    }

    /// Host: on `port` directly, or in a room on `relay` (`host:port`).
    fn host(
        ctx: &egui::Context,
        name: &str,
        port: u16,
        relay: Option<&str>,
        password: &str,
    ) -> Result<Self, String> {
        let (config, fingerprint) = server_config()?;
        let (to_ui, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let door = Door {
            config,
            password: password.to_string(),
            next_id: Arc::new(AtomicU32::new(1)),
            to_ui,
            ctx: ctx.clone(),
            stop: stop.clone(),
        };
        let invite = match relay {
            Some(relay) => {
                let room = format!("{:08x}", rand::random::<u32>());
                let invite = Invite {
                    addr: relay.to_string(),
                    room: Some(room.clone()),
                    fingerprint,
                };
                let relay = relay.to_string();
                std::thread::spawn(move || host_on_relay(relay, room, door));
                invite
            }
            None => {
                let listener = TcpListener::bind(("0.0.0.0", port))
                    .map_err(|e| format!("Port {port}: {e}"))?;
                let port = listener.local_addr().map_err(|e| e.to_string())?.port();
                listener.set_nonblocking(true).map_err(|e| e.to_string())?;
                std::thread::spawn(move || {
                    while !door.stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((s, _)) => {
                                let door = door.clone();
                                if s.set_nonblocking(false).is_ok() {
                                    std::thread::spawn(move || let_in(s, door));
                                }
                            }
                            Err(_) => std::thread::sleep(Duration::from_millis(50)),
                        }
                    }
                });
                Invite {
                    addr: format!("{}:{port}", local_ip()),
                    room: None,
                    fingerprint,
                }
            }
        };
        let mut c = Self::new(true, name, rx, stop);
        if invite.room.is_none()
            && let Some(ip) = tailnet_ip()
        {
            let port = invite.addr.rsplit_once(':').map_or("", |(_, p)| p);
            c.tailnet_invite = Some(
                Invite {
                    addr: format!("{ip}:{port}"),
                    room: None,
                    fingerprint,
                }
                .code(),
            );
        }
        c.invite = Some(invite.code());
        c.users = vec![(0, c.name.clone())];
        c.status = "Hosting: share the invite code".into();
        Ok(c)
    }

    /// Join with an invite code (connecting goes on in the background).
    fn join(ctx: &egui::Context, name: &str, invite: &str, password: &str) -> Result<Self, String> {
        let invite = Invite::parse(invite)?;
        let config = client_config(invite.fingerprint)?;
        let (to_ui, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let c = Self::new(false, name, rx, stop.clone());
        let hello = frame(
            &Head::Hello {
                name: c.name.clone(),
                password: password.to_string(),
            },
            &[],
        );
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let mut sock = connect(&invite.addr)?;
                if let Some(room) = &invite.room {
                    sock.write_all(format!("JOIN {room}\n").as_bytes())
                        .map_err(|e| e.to_string())?;
                    expect_ok(&mut sock)?;
                }
                let name =
                    rustls::pki_types::ServerName::try_from(SERVER_NAME).expect("a valid name");
                let conn =
                    rustls::ClientConnection::new(config, name).map_err(|e| e.to_string())?;
                let mut tls = rustls::StreamOwned::new(conn, sock);
                handshake(&mut tls)?;
                let mut link = Link {
                    io: tls,
                    buf: Vec::new(),
                };
                link.send(&hello).map_err(|e| e.to_string())?;
                let you = match link.wait(HANDSHAKE)?.0 {
                    Head::Welcome { you } => you,
                    Head::Refused(why) => return Err(why),
                    _ => return Err("The host answered oddly".into()),
                };
                let (tx, out) = channel();
                let _ = to_ui.send(Net::Joined {
                    id: 0,
                    name: String::new(),
                    tx,
                });
                let _ = to_ui.send(Net::Msg {
                    id: 0,
                    head: Head::Welcome { you },
                    blob: Vec::new(),
                });
                ctx.request_repaint();
                run_link(link, 0, &out, &to_ui, &ctx, &stop);
                Ok(())
            })();
            if let Err(why) = result {
                let _ = to_ui.send(Net::Left { id: 0, why });
                ctx.request_repaint();
            }
        });
        let mut c = c;
        c.status = "Connecting…".into();
        Ok(c)
    }

    pub(crate) fn is_host(&self) -> bool {
        self.host
    }

    /// Send to everyone (a guest: to the host), but `except`.
    fn broadcast(&self, frame: &Arc<Vec<u8>>, except: Option<u32>) {
        for p in self.peers.iter().filter(|p| Some(p.id) != except) {
            let _ = p.tx.send(frame.clone());
        }
    }

    fn send_to(&self, id: u32, frame: Arc<Vec<u8>>) {
        if let Some(p) = self.peers.iter().find(|p| p.id == id) {
            let _ = p.tx.send(frame);
        }
    }

    /// This painter's pen samples so far: before anything else they made
    /// is sent (a stroke's end before its pixels, or the undo of it,
    /// which would otherwise come first and leave the end painted).
    fn send_strokes(&mut self) {
        let batches = self.outgoing.as_mut().map(|o| o.take()).unwrap_or_default();
        if !self.synced {
            return;
        }
        for (events, blob) in batches {
            let head = Head::Stroke {
                from: self.me,
                events,
            };
            self.broadcast(&frame(&head, &blob), None);
        }
    }

    fn send_change(&mut self, patches: Vec<Patch>) {
        self.send_strokes();
        let patches: Vec<Patch> = patches.into_iter().filter(Patch::changes).collect();
        if patches.is_empty() || !self.synced {
            return;
        }
        if self.host {
            self.touched
                .extend(patches.iter().map(|p| (p.at.layer, p.at.tx, p.at.ty)));
        }
        self.sent += 1;
        let (tiles, blob) = encode_patches(&patches);
        let head = Head::Change {
            from: self.me,
            seq: self.sent,
            tiles,
        };
        self.broadcast(&frame(&head, &blob), None);
    }

    fn send_users(&self) {
        self.broadcast(&frame(&Head::Users(self.users.clone()), &[]), None);
    }

    /// The left-pixel stacks back in step with the history's (steps dropped
    /// off its old end, or there from before).
    fn fit_steps(&mut self, history: &History) {
        let (undo, redo) = history.stacks();
        for (left, len) in [
            (&mut self.left_undo, undo.len()),
            (&mut self.left_redo, redo.len()),
        ] {
            if left.len() > len {
                left.drain(..left.len() - len);
            }
            while left.len() < len {
                left.insert(0, None);
            }
        }
    }

    fn reset_steps(&mut self, history: &History) {
        self.left_undo.clear();
        self.left_redo.clear();
        self.seen_push = history.push_count();
        self.fit_steps(history);
    }
}

// ---------------------------------------------------------------------------
// The app's side.

impl PainterApp {
    pub(crate) fn collab_host(&mut self, ctx: &egui::Context) -> Result<(), String> {
        // Patches carry 8-bit pixels: a deeper document would need deep ones.
        if self.canvas.depth() != Depth::U8 {
            return Err("Painting together works on 8-bit documents".into());
        }
        let form = &self.workspace.collab_form;
        let relay = Some(form.relay.trim()).filter(|r| !r.is_empty());
        let mut c = Collab::host(ctx, &form.name, form.port, relay, &form.password)?;
        c.reset_steps(&self.layer_state.history);
        c.slot = Some(self.workspace.autosave.slot());
        c.next_id = self.canvas.next_layer_id();
        c.known = self.collab_layer_ids();
        self.collab_tap(&mut c);
        self.workspace.collab = Some(c);
        Ok(())
    }

    /// Have the stroke worker report this painter's strokes, to send.
    fn collab_tap(&self, c: &mut Collab) {
        let (tx, rx) = channel();
        self.stroke_worker.set_tap(Some(tx));
        c.outgoing = Some(replay::Outgoing::new(rx));
    }

    pub(crate) fn collab_join(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let form = &self.workspace.collab_form;
        let mut c = Collab::join(ctx, &form.name, &form.invite, &form.password)?;
        self.collab_tap(&mut c);
        self.workspace.collab = Some(c);
        Ok(())
    }

    fn collab_layer_ids(&self) -> HashSet<u64> {
        self.canvas.layers.iter().map(|l| l.id.0).collect()
    }

    /// Whether the session's document is the one showing (for a guest not
    /// in yet: the one its document will replace).
    pub(crate) fn collab_here(&self, c: &Collab) -> bool {
        c.slot.is_none_or(|s| s == self.workspace.autosave.slot())
    }

    /// Whether the session's document is going (opened over, in place):
    /// then so is the session.
    pub(crate) fn collab_document_replaced(&mut self) {
        let here = self.workspace.collab.as_ref().map(|c| self.collab_here(c));
        if here == Some(true) {
            self.workspace.collab = None;
        }
    }

    /// Something holding on to layers or pixels is going on (a stroke, a
    /// transform, text being typed): others' changes wait for it.
    fn collab_busy(&self) -> bool {
        self.brush_state.is_drawing
            || self.layer_state.float_session.is_some()
            || self.layer_state.liquify.is_some()
            || self.layer_state.puppet.is_some()
            || self.workspace.text.session.is_some()
    }

    /// Each frame: what arrived, what changed here, cursors and checksums.
    pub(crate) fn collab_tick(&mut self) {
        let Some(mut c) = self.workspace.collab.take() else {
            return;
        };
        self.collab_network(&mut c);
        let here = self.collab_here(&c);
        if here {
            c.send_strokes();
        } else {
            // (Painted on another document: not this session's.)
            drop(c.outgoing.as_mut().map(|o| o.take()));
        }
        let strokes = std::mem::take(&mut c.strokes_in);
        if here && c.synced {
            for (from, events, blob) in strokes {
                self.replay_events(&mut c.replays, from, events, &blob);
            }
            self.replay_copy(&mut c.replays);
        }
        if !here {
            // The session's tab was closed: so is the session.
            let tabs = &self.workspace.tabs;
            if !tabs
                .slots(self.workspace.autosave.slot())
                .any(|s| Some(s) == c.slot)
            {
                return;
            }
            if !c.host && c.held.len() > MAX_HELD_AWAY {
                c.held.clear();
                c.resync = true;
            }
            self.workspace.collab = Some(c);
            return;
        }
        if std::mem::take(&mut c.resync) {
            c.broadcast(&frame(&Head::Resync, &[]), None);
        }
        let pushed = c.seen_push != self.layer_state.history.push_count();
        if !self.collab_busy() && (pushed || !c.held.is_empty() || !c.waiting.is_empty()) {
            // The stroke just ended into the history first.
            self.release_canvas();
            self.collab_send_steps(&mut c);
            while let Some((id, head, blob)) = c.held.pop_front() {
                if let Err(why) = self.collab_receive(&mut c, id, head, blob) {
                    log::warn!("Paint together: {why}");
                    if c.host {
                        // A guest sending what doesn't fit is dropped.
                        c.peers.retain(|p| p.id != id);
                        c.users.retain(|u| u.0 != id);
                        c.send_users();
                    }
                }
            }
            // Joiners get the document with all that arrived before them.
            if !c.waiting.is_empty() {
                match self.collab_snapshot() {
                    Ok(doc) => {
                        for id in std::mem::take(&mut c.waiting) {
                            c.send_to(id, doc.clone());
                        }
                    }
                    Err(why) => c.status = why,
                }
            }
        }
        if self.canvas.depth() != Depth::U8 {
            self.workspace.collab_form.error =
                Some("Stopped: the document is no longer 8-bit".into());
            return;
        }
        self.collab_cursor(&mut c);
        let due = c.last_hashes.elapsed() >= HASH_EVERY;
        if c.host && due && c.held.is_empty() && !c.peers.is_empty() {
            c.last_hashes = Instant::now();
            let tiles: Vec<_> = c
                .touched
                .drain()
                .filter_map(|(layer, tx, ty)| {
                    let idx = self.canvas.layer_index_of(LayerId(layer))?;
                    Some((layer, tx, ty, tile_hash(&self.canvas, idx, tx, ty)))
                })
                .collect();
            if !tiles.is_empty() {
                for p in &c.peers {
                    let applied = c.applied.get(&p.id).copied().unwrap_or(0);
                    let head = Head::Hashes {
                        applied,
                        tiles: tiles.clone(),
                    };
                    let _ = p.tx.send(frame(&head, &[]));
                }
            }
        }
        self.workspace.collab = Some(c);
    }

    fn collab_network(&mut self, c: &mut Collab) {
        while let Ok(event) = c.rx.try_recv() {
            match event {
                Net::Joined { id, name, tx } => {
                    c.peers.push(Peer { id, tx });
                    if c.host {
                        c.users.push((id, name.clone()));
                        c.status = format!("{name} joined");
                        if let Some(out) = c.outgoing.as_mut() {
                            out.forget_brush();
                        }
                        c.waiting.push(id);
                        c.send_users();
                    } else {
                        c.status = "Connected".into();
                    }
                }
                Net::Left { id, why } => {
                    c.peers.retain(|p| p.id != id);
                    c.remotes.remove(&id);
                    c.replays.forget(id);
                    c.waiting.retain(|&w| w != id);
                    if c.host {
                        let name = c.users.iter().find(|u| u.0 == id).map(|u| u.1.clone());
                        c.users.retain(|u| u.0 != id);
                        c.status = format!("{} left", name.unwrap_or_default());
                        c.send_users();
                    } else {
                        c.status = format!("Disconnected: {why}");
                        c.users.clear();
                        c.remotes.clear();
                    }
                }
                Net::Msg { id, head, blob } => Self::collab_ingest(c, id, head, blob),
                Net::Status(why) => c.status = why,
            }
        }
    }

    /// A message as it arrives, whatever's showing: cursors and who's in
    /// are taken at once, and the host passes changes on to the others
    /// straight away (in the order it got them: the session's order). The
    /// rest waits to be applied to the document.
    fn collab_ingest(c: &mut Collab, id: u32, head: Head, blob: Vec<u8>) {
        // A guest's word for who sent what is the host's; the host knows
        // which guest it came from.
        let sender = |from: u32| if c.host { id } else { from };
        let head = match head {
            Head::Cursor {
                from,
                pos,
                down,
                color,
                size,
            } => {
                let from = sender(from);
                let r = c.remotes.entry(from).or_insert_with(|| Remote {
                    pos: Vec2::ZERO,
                    down: false,
                    color: Color32::WHITE,
                    size: 1.0,
                });
                let at = Vec2::from(pos);
                let [cr, cg, cb, ca] = color;
                (r.pos, r.down, r.size) = (at, down, size.clamp(1.0, 2000.0));
                r.color = Color32::from_rgba_unmultiplied(cr, cg, cb, ca);
                if c.host {
                    let head = Head::Cursor {
                        from,
                        pos,
                        down,
                        color,
                        size,
                    };
                    c.broadcast(&frame(&head, &[]), Some(id));
                }
                return;
            }
            Head::Stroke { from, events } => {
                let from = sender(from);
                if c.host {
                    let head = Head::Stroke {
                        from,
                        events: events.clone(),
                    };
                    c.broadcast(&frame(&head, &blob), Some(id));
                }
                c.strokes_in.push((from, events, blob));
                return;
            }
            Head::Users(users) if !c.host => {
                c.remotes.retain(|id, _| users.iter().any(|u| u.0 == *id));
                for gone in c.users.iter().filter(|u| !users.contains(u)) {
                    c.replays.forget(gone.0);
                }
                // Someone new needs the brush with the next stroke.
                if let Some(out) = c.outgoing.as_mut() {
                    out.forget_brush();
                }
                c.users = users;
                return;
            }
            Head::Welcome { you } if !c.host => {
                c.me = you;
                return;
            }
            Head::Change { from, seq, tiles } => {
                let from = sender(from);
                if c.host {
                    c.applied.insert(id, seq);
                }
                Head::Change { from, seq, tiles }
            }
            head => head,
        };
        if c.host
            && matches!(
                head,
                Head::Change { .. } | Head::Layers { .. } | Head::Snapshot
            )
        {
            c.broadcast(&frame(&head, &blob), Some(id));
        }
        c.held.push_back((id, head, blob));
    }

    /// The document as the others get it: no history (it's this
    /// painter's).
    fn collab_snapshot(&mut self) -> Result<Arc<Vec<u8>>, String> {
        self.release_canvas();
        let bytes = crate::project::encode_shared(self, true)?;
        Ok(frame(&Head::Snapshot, &bytes))
    }

    /// Send what the history's new steps changed.
    fn collab_send_steps(&mut self, c: &mut Collab) {
        let count = self.layer_state.history.push_count();
        if count == c.seen_push {
            return;
        }
        let new = count.saturating_sub(c.seen_push) as usize;
        c.seen_push = count;
        let (undo, _) = self.layer_state.history.stacks();
        let steps: Vec<&UndoAction> = undo[undo.len().saturating_sub(new)..].iter().collect();
        // (Several at once: a stroke right after another, none sent while
        // painting.)
        let left = patches_of_steps(&self.canvas, &steps);
        let (mut whole, mut layers) = (false, false);
        let (mut changed, mut redraw) = (Vec::new(), Vec::new());
        for (step, patches) in steps.iter().zip(left) {
            if rewrites_document(step) {
                whole = true;
                c.left_undo.push(None);
                continue;
            }
            layers |= changes_layers(step);
            redraw.extend(redraws_of(step));
            changed.extend(patches.iter().cloned());
            c.left_undo.push(Some(patches));
        }
        c.left_redo.clear();
        c.fit_steps(&self.layer_state.history);
        c.next_id = self.canvas.next_layer_id();
        if whole {
            self.collab_send_snapshot(c);
            return;
        }
        // The layers first: the pixels may be on new ones.
        if layers {
            self.collab_send_layers(c, redraw);
        }
        c.send_change(changed);
    }

    /// Resized, cropped, turned or merged: everyone gets the whole
    /// document (a guest's goes through the host).
    fn collab_send_snapshot(&mut self, c: &mut Collab) {
        if !c.synced {
            return;
        }
        c.send_strokes();
        match self.collab_snapshot() {
            Ok(doc) => c.broadcast(&doc, None),
            Err(why) => c.status = why,
        }
        c.known = self.collab_layer_ids();
    }

    /// The layer list, then the pixels of the layers the others haven't
    /// had (a layer duplicated, a removal undone).
    fn collab_send_layers(&mut self, c: &mut Collab, redraw: Vec<(u64, [i32; 4])>) {
        if !c.synced {
            return;
        }
        c.send_strokes();
        let ids = self.collab_layer_ids();
        let removed = c.known.difference(&ids).copied().collect();
        self.collab_broadcast_layers(c, removed, redraw, None);
        let ts = self.canvas.tile_size();
        let mut fresh = Vec::new();
        for &layer in ids.difference(&c.known) {
            let Some(idx) = self.canvas.layer_index_of(LayerId(layer)) else {
                continue;
            };
            for (tx, ty) in self.canvas.layer_tile_keys(idx) {
                let at = TileHead::whole(layer, tx, ty, ts);
                if check(&self.canvas, &at).is_some() {
                    let px = read_rect(&self.canvas, idx, &at);
                    let mask = px.iter().map(|&p| p != Color32::TRANSPARENT).collect();
                    fresh.push(Patch { at, mask, px });
                }
            }
        }
        c.known = ids;
        c.send_change(fresh);
    }

    fn collab_broadcast_layers(
        &mut self,
        c: &Collab,
        removed: Vec<u64>,
        redraw: Vec<(u64, [i32; 4])>,
        except: Option<u32>,
    ) {
        match crate::project::encode_shared(self, false) {
            Ok(bytes) => c.broadcast(&frame(&Head::Layers { removed, redraw }, &bytes), except),
            Err(why) => log::warn!("Paint together: {why}"),
        }
    }

    /// Send what's been done here, before leaving the document for another
    /// tab.
    pub(crate) fn collab_flush(&mut self) {
        let Some(mut c) = self.workspace.collab.take() else {
            return;
        };
        if self.collab_here(&c) && !self.brush_state.is_drawing {
            self.collab_send_steps(&mut c);
        }
        self.workspace.collab = Some(c);
    }

    fn collab_receive(
        &mut self,
        c: &mut Collab,
        id: u32,
        head: Head,
        blob: Vec<u8>,
    ) -> Result<(), String> {
        // Before the host's document comes, only it is taken.
        if !c.synced && !matches!(head, Head::Snapshot) {
            return Ok(());
        }
        match head {
            Head::Change { from, tiles, .. } => {
                let patches = decode_patches(&tiles, &blob)?;
                // Their strokes painted here make way for their pixels.
                self.replay_settle(&mut c.replays, from, &patches);
                self.collab_apply(&patches)?;
                disown(&mut c.left_undo, &patches);
                disown(&mut c.left_redo, &patches);
                if c.host {
                    c.touched
                        .extend(tiles.iter().map(|t| (t.layer, t.tx, t.ty)));
                }
            }
            Head::Fix { applied, tiles } if !c.host => {
                // Only if nothing of ours was still on its way.
                if applied == c.sent {
                    self.collab_apply(&decode_patches(&tiles, &blob)?)?;
                }
            }
            Head::Snapshot => {
                let loaded = crate::project::decode_project(&blob)?;
                if loaded.canvas.depth() != Depth::U8 {
                    return Err("The document isn't 8-bit".into());
                }
                let first = !c.synced;
                let history = if first {
                    // What was open stays, in its own tab.
                    self.keep_document_in_tab();
                    c.next_id = u64::from(c.me) << ID_RANGE;
                    History::new()
                } else {
                    // Steps are by layer id: they still apply.
                    std::mem::take(&mut self.layer_state.history)
                };
                self.install_document(loaded.canvas, history, first);
                self.canvas_mut().set_next_layer_id(c.next_id.max(1));
                // Not on disk here: work to save (and autosave), like
                // anything the others do.
                self.layer_state.remote_edits += 1;
                if first {
                    // Joined from the library: the document shows.
                    self.workspace.library.open = false;
                    self.workspace.library.has_document = true;
                    // (A file name's dots would cut it short.)
                    let host = c
                        .users
                        .iter()
                        .find(|u| u.0 == 0)
                        .map(|u| u.1.replace('.', " "));
                    self.set_title_from(&format!("With {}", host.as_deref().unwrap_or("host")));
                    c.synced = true;
                    c.slot = Some(self.workspace.autosave.slot());
                    c.reset_steps(&self.layer_state.history);
                }
                c.fit_steps(&self.layer_state.history);
                c.known = self.collab_layer_ids();
            }
            Head::Layers { removed, redraw } => {
                let loaded = crate::project::decode_project(&blob)?;
                let size = (loaded.canvas.width(), loaded.canvas.height());
                if size != (self.canvas.width(), self.canvas.height()) {
                    return Err("A layer list for another size".into());
                }
                let sent: Vec<LayerId> = loaded.canvas.layers.iter().map(|l| l.id).collect();
                let removed: Vec<LayerId> = removed.into_iter().map(LayerId).collect();
                self.canvas_mut().adopt_layers(loaded.canvas, &removed);
                self.layer_state.remote_edits += 1;
                let len = self.canvas.layers.len();
                (self.layer_state.layer_ui_colors).resize(len, Color32::from_gray(40));
                self.canvas.render_rigs();
                self.canvas.pose_motions();
                self.mark_all_tiles_dirty();
                self.layer_state.thumbnails_dirty = true;
                for (layer, region) in redraw {
                    if let Some(idx) = self.canvas.layer_index_of(LayerId(layer)) {
                        self.request_vector_redraw(idx, region);
                    }
                }
                c.known = self.collab_layer_ids();
                // Layers added here meanwhile were kept: the host's order
                // is everyone's.
                let mine: Vec<LayerId> = self.canvas.layers.iter().map(|l| l.id).collect();
                if c.host && mine != sent {
                    self.collab_broadcast_layers(c, Vec::new(), Vec::new(), None);
                }
            }
            Head::Resync if c.host => c.waiting.push(id),
            Head::Hashes { applied, tiles } if !c.host => {
                // Only once the host has all we sent: else ours differ for
                // being newer.
                if applied != c.sent {
                    return Ok(());
                }
                let want: Vec<_> = tiles
                    .into_iter()
                    .filter(|&(layer, tx, ty, hash)| {
                        self.canvas
                            .layer_index_of(LayerId(layer))
                            .is_some_and(|idx| tile_hash(&self.canvas, idx, tx, ty) != hash)
                    })
                    .map(|(layer, tx, ty, _)| (layer, tx, ty))
                    .take(MAX_WANT)
                    .collect();
                if !want.is_empty() {
                    c.broadcast(&frame(&Head::Want(want), &[]), None);
                }
            }
            Head::Want(want) if c.host => {
                let ts = self.canvas.tile_size();
                let patches: Vec<Patch> = want
                    .into_iter()
                    .take(MAX_WANT)
                    .filter_map(|(layer, tx, ty)| {
                        let at = TileHead::whole(layer, tx, ty, ts);
                        let idx = check(&self.canvas, &at)?;
                        Some(Patch {
                            mask: vec![true; at.len()],
                            px: read_rect(&self.canvas, idx, &at),
                            at,
                        })
                    })
                    .collect();
                let (tiles, blob) = encode_patches(&patches);
                let applied = c.applied.get(&id).copied().unwrap_or(0);
                c.send_to(id, frame(&Head::Fix { applied, tiles }, &blob));
            }
            _ => {}
        }
        Ok(())
    }

    /// Write others' pixels (all checked first: one bad patch and none is
    /// written). They're not undo steps here.
    fn collab_apply(&mut self, patches: &[Patch]) -> Result<(), String> {
        let places = patches
            .iter()
            .map(|p| {
                let ok = p.mask.len() == p.at.len() && p.px.len() == p.at.len();
                ok.then(|| check(&self.canvas, &p.at)).flatten()
            })
            .collect::<Option<Vec<usize>>>()
            .ok_or("A change that doesn't fit the document")?;
        for (p, idx) in patches.iter().zip(places) {
            write_masked(&self.canvas, idx, &p.at, &p.mask, &p.px);
            if p.at.tx >= 0 && p.at.ty >= 0 {
                self.mark_tile_dirty(p.at.tx as usize, p.at.ty as usize);
            }
        }
        if !patches.is_empty() {
            self.layer_state.thumbnails_dirty = true;
            self.layer_state.remote_edits += 1;
        }
        Ok(())
    }

    /// Whether the next undo (or redo) would swap the whole document back
    /// (a resize, crop, turn or merge): not while painting together, as it
    /// would take back what everyone did since.
    pub(crate) fn collab_blocks_history(&self, redo: bool) -> bool {
        let Some(c) = self
            .workspace
            .collab
            .as_ref()
            .filter(|c| self.collab_here(c))
        else {
            return false;
        };
        if c.peers.is_empty() {
            return false;
        }
        let (undo, redos) = self.layer_state.history.stacks();
        let next = if redo { redos.last() } else { undo.last() };
        next.is_some_and(rewrites_document)
    }

    /// After an undo (or redo) that moved a step: put back what others
    /// painted over this painter's pixels since, and send the result.
    pub(crate) fn collab_after_history(&mut self, redo: bool) {
        let Some(mut c) = self.workspace.collab.take() else {
            return;
        };
        if !self.collab_here(&c) {
            self.workspace.collab = Some(c);
            return;
        }
        let (from, to) = if redo {
            (&mut c.left_redo, &mut c.left_undo)
        } else {
            (&mut c.left_undo, &mut c.left_redo)
        };
        let left = from.pop().flatten();
        // The step just moved now holds the pixels from just before.
        let (undo, redos) = self.layer_state.history.stacks();
        let Some(step) = (if redo { undo.last() } else { redos.last() }) else {
            self.workspace.collab = Some(c);
            return;
        };
        if rewrites_document(step) {
            to.push(None);
            self.collab_send_snapshot(&mut c);
            self.workspace.collab = Some(c);
            return;
        }
        let layers = changes_layers(step);
        let redraw = redraws_of(step);
        let mut patches = Vec::new();
        for snapshot in &step.tiles {
            let at = TileHead::of(snapshot);
            let Some(idx) = check(&self.canvas, &at) else {
                continue;
            };
            let current = snapshot.data.to_vec();
            let now = read_rect(&self.canvas, idx, &at);
            if current.len() != now.len() {
                continue;
            }
            let mine = left.as_ref().and_then(|l| l.iter().find(|p| p.at == at));
            let px = keep_theirs(&current, mine, &now);
            if px != now {
                let back: Vec<bool> = px.iter().zip(&now).map(|(a, b)| a != b).collect();
                write_masked(&self.canvas, idx, &at, &back, &px);
            }
            let mask = px.iter().zip(&current).map(|(a, b)| a != b).collect();
            patches.push(Patch { at, mask, px });
        }
        to.push(Some(patches.clone()));
        if layers {
            self.collab_send_layers(&mut c, redraw);
        }
        c.send_change(patches);
        self.workspace.collab = Some(c);
    }

    fn collab_cursor(&mut self, c: &mut Collab) {
        if !c.synced || c.peers.is_empty() || c.last_cursor.elapsed() < CURSOR_EVERY {
            return;
        }
        let Some(pos) = self.viewport.cursor_canvas else {
            return;
        };
        let down = self.brush_state.is_drawing;
        if c.cursor_sent == Some((pos, down)) {
            return;
        }
        c.cursor_sent = Some((pos, down));
        c.last_cursor = Instant::now();
        let options = &self.brush_state.brush.brush_options;
        let head = Head::Cursor {
            from: c.me,
            pos: pos.into(),
            down,
            color: options.color.to_srgba_unmultiplied(),
            size: options.diameter,
        };
        c.broadcast(&frame(&head, &[]), None);
    }
}

/// A step that swaps whole documents or layer lists (resize, crop, turn,
/// merge): sent as the whole document.
fn rewrites_document(step: &UndoAction) -> bool {
    matches!(
        LayerHistoryOp::structural(step.layer_action.as_ref()),
        Some(LayerHistoryOp::Document(_) | LayerHistoryOp::Replaced(_))
    )
}

/// A step that changes the layer list or a layer's settings or sources
/// (all but wet paint drying, which isn't shared).
fn changes_layers(step: &UndoAction) -> bool {
    let mut op = step.layer_action.as_ref();
    while let Some(LayerHistoryOp::Wet { inner, .. }) = op {
        op = inner.as_deref();
    }
    op.is_some()
}

/// Where a step's vector lines changed without pixels kept: the others
/// draw them again there.
fn redraws_of(step: &UndoAction) -> Vec<(u64, [i32; 4])> {
    let mut found = Vec::new();
    let mut op = step.layer_action.as_ref();
    while let Some(o) = op {
        op = match o {
            LayerHistoryOp::Vector {
                layers,
                inner,
                redraw,
            } => {
                if let Some(region) = redraw {
                    found.extend(layers.iter().map(|(id, _)| (id.0, *region)));
                }
                inner.as_deref()
            }
            LayerHistoryOp::Text { inner, .. }
            | LayerHistoryOp::Height { inner, .. }
            | LayerHistoryOp::Wet { inner, .. } => inner.as_deref(),
            _ => None,
        };
    }
    found
}

/// Others' cursors, with their names (their strokes are painted on the
/// canvas itself).
pub(crate) fn draw_others(app: &PainterApp, painter: &egui::Painter, map: &ScreenMap) {
    let Some(c) = app.workspace.collab.as_ref().filter(|c| app.collab_here(c)) else {
        return;
    };
    for (id, r) in &c.remotes {
        let width = (r.size * map.zoom()).max(1.0);
        let at = map.to_screen(r.pos);
        let ring = egui::Stroke::new(1.5_f32, r.color.to_opaque());
        painter.circle_stroke(at, (width / 2.0).max(3.0), ring);
        let name = c
            .users
            .iter()
            .find(|u| u.0 == *id)
            .map_or("", |u| u.1.as_str());
        painter.text(
            at + egui::vec2(8.0, 8.0),
            egui::Align2::LEFT_TOP,
            name,
            egui::FontId::proportional(12.0),
            r.color.to_opaque(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color32 = Color32::from_rgb(255, 0, 0);
    const BLUE: Color32 = Color32::from_rgb(0, 0, 255);

    fn put(canvas: &Canvas, x: usize, y: usize, c: Color32) {
        let ts = canvas.tile_size();
        let mut tile = canvas
            .get_layer_tile_data(1, 0, 0)
            .unwrap_or_else(|| vec![Color32::TRANSPARENT; ts * ts]);
        tile[y * ts + x] = c;
        canvas.set_layer_tile_data(1, 0, 0, tile);
    }

    fn get(canvas: &Canvas, x: usize, y: usize) -> Color32 {
        let ts = canvas.tile_size();
        canvas
            .get_layer_tile_data(1, 0, 0)
            .map_or(Color32::TRANSPARENT, |t| t[y * ts + x])
    }

    /// A step painting `c` at each of `at`: its snapshot taken first.
    fn step(canvas: &Canvas, at: &[(usize, usize)], c: Color32) -> UndoAction {
        let ts = canvas.tile_size();
        let before = read_rect(canvas, 1, &TileHead::whole(1, 0, 0, ts));
        for &(x, y) in at {
            put(canvas, x, y, c);
        }
        UndoAction {
            tiles: vec![TileSnapshot {
                tx: 0,
                ty: 0,
                layer_id: LayerId(1),
                x0: 0,
                y0: 0,
                width: ts,
                height: ts,
                data: before.into(),
            }],
            selection: None,
            transform: None,
            layer_action: None,
        }
    }

    fn canvas() -> Canvas {
        Canvas::new(128, 128, Color32::WHITE, 64)
    }

    #[test]
    fn two_painters_in_one_tile_both_keep_their_paint() {
        let (a, b) = (canvas(), canvas());
        let from_a = patches_of(&a, &step(&a, &[(1, 1)], RED));
        let from_b = patches_of(&b, &step(&b, &[(2, 2)], BLUE));
        // Through the wire both ways.
        for (patches, onto) in [(from_a, &b), (from_b, &a)] {
            let (heads, blob) = encode_patches(&patches);
            for p in decode_patches(&heads, &blob).unwrap() {
                let idx = check(onto, &p.at).unwrap();
                write_masked(onto, idx, &p.at, &p.mask, &p.px);
            }
        }
        for c in [&a, &b] {
            assert_eq!(get(c, 1, 1), RED);
            assert_eq!(get(c, 2, 2), BLUE);
            assert_eq!(tile_hash(c, 1, 0, 0), tile_hash(&a, 1, 0, 0));
        }
    }

    #[test]
    fn undo_keeps_what_others_painted_over_it() {
        let mut app = crate::project::tests::test_app_pub(canvas());
        let (_tx, rx) = channel();
        let mut c = Collab::new(true, "me", rx, Arc::default());
        c.reset_steps(&app.layer_state.history);
        app.workspace.collab = Some(c);
        // A stroke of mine over two pixels.
        let mine = step(&app.canvas, &[(1, 1), (2, 2)], RED);
        app.layer_state.history.push_action(mine);
        app.collab_tick();
        // Someone paints over one of them.
        put(&app.canvas, 2, 2, BLUE);
        app.apply_history_now(false);
        assert_eq!(get(&app.canvas, 1, 1), Color32::TRANSPARENT);
        assert_eq!(get(&app.canvas, 2, 2), BLUE);
        // Redo brings mine back where it's still mine to bring.
        app.apply_history_now(true);
        assert_eq!(get(&app.canvas, 1, 1), RED);
        assert_eq!(get(&app.canvas, 2, 2), BLUE);
    }

    #[test]
    fn what_doesnt_fit_is_refused() {
        let c = canvas();
        let at = TileHead::whole(1, 0, 0, 64);
        assert!(check(&c, &at).is_some());
        assert!(check(&c, &TileHead { x0: 1, ..at }).is_none());
        assert!(check(&c, &TileHead { w: 0, ..at }).is_none());
        assert!(check(&c, &TileHead { tx: 10_000, ..at }).is_none());
        assert!(check(&c, &TileHead { layer: 99, ..at }).is_none());
        let (heads, blob) = encode_patches(&[Patch {
            at,
            mask: vec![true; at.len()],
            px: vec![RED; at.len()],
        }]);
        assert!(decode_patches(&heads, &blob).is_ok());
        let bigger = [TileHead { h: 32, ..at }, TileHead { h: 32, ..at }, at];
        assert!(decode_patches(&bigger, &blob).is_err());
        let mut buf = (u32::MAX).to_le_bytes().to_vec();
        assert!(take_frame(&mut buf).is_err());
    }

    #[test]
    fn frames_and_invites_round_trip() {
        let mut buf = frame(&Head::Want(vec![(1, 2, 3)]), b"blob").to_vec();
        buf.extend_from_slice(&frame(&Head::Snapshot, &[]));
        let (head, blob) = take_frame(&mut buf).unwrap().unwrap();
        assert!(matches!(head, Head::Want(w) if w == [(1, 2, 3)]));
        assert_eq!(blob, b"blob");
        assert!(matches!(
            take_frame(&mut buf).unwrap(),
            Some((Head::Snapshot, _))
        ));
        assert!(buf.is_empty());
        for room in [None, Some("ab12".to_string())] {
            let invite = Invite {
                addr: "example.org:7878".into(),
                room,
                fingerprint: [7; 32],
            };
            assert_eq!(Invite::parse(&invite.code()).unwrap(), invite);
        }
        assert!(Invite::parse("rp://x#12").is_err());
    }

    /// Host on localhost, join, and a joiner with the wrong fingerprint is
    /// turned away.
    #[test]
    fn a_guest_joins_over_tls_and_a_wrong_certificate_is_refused() {
        let ctx = egui::Context::default();
        let host = Collab::host(&ctx, "host", 0, None, "pw").unwrap();
        let invite = Invite::parse(host.invite.as_ref().unwrap()).unwrap();
        let port = invite.addr.rsplit_once(':').unwrap().1;
        let local = Invite {
            addr: format!("127.0.0.1:{port}"),
            ..invite
        };
        let wait = |c: &Collab| c.rx.recv_timeout(Duration::from_secs(10)).unwrap();

        let guest = Collab::join(&ctx, "guest", &local.code(), "pw").unwrap();
        assert!(matches!(wait(&host), Net::Joined { id: 1, ref name, .. } if name == "guest"));
        assert!(matches!(wait(&guest), Net::Joined { id: 0, .. }));
        assert!(matches!(
            wait(&guest),
            Net::Msg {
                head: Head::Welcome { you: 1 },
                ..
            }
        ));

        let wrong_password = Collab::join(&ctx, "x", &local.code(), "nope").unwrap();
        assert!(
            matches!(wait(&wrong_password), Net::Left { ref why, .. } if why.contains("password"))
        );

        let forged = Invite {
            fingerprint: [0; 32],
            ..Invite::parse(&local.code()).unwrap()
        };
        let impostor = Collab::join(&ctx, "x", &forged.code(), "pw").unwrap();
        assert!(matches!(wait(&impostor), Net::Left { .. }));
    }

    /// Run both apps' frames until `done` (or give up).
    fn pump(
        a: &mut PainterApp,
        b: &mut PainterApp,
        done: impl Fn(&PainterApp, &PainterApp) -> bool,
    ) {
        for _ in 0..1000 {
            a.collab_tick();
            b.collab_tick();
            if done(a, b) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "the apps didn't get there: a {:?} {:?}, b {:?} {:?}",
            ids(a),
            a.workspace
                .collab
                .as_ref()
                .map(|c| (&c.status, c.held.len(), c.slot, c.synced)),
            ids(b),
            b.workspace
                .collab
                .as_ref()
                .map(|c| (&c.status, c.held.len(), c.slot, c.synced)),
        );
    }

    /// A host and a guest in session, the host's document over.
    fn session() -> (PainterApp, PainterApp) {
        let ctx = egui::Context::default();
        let mut a = crate::project::tests::test_app_pub(canvas());
        a.workspace.collab_form.port = 0;
        a.collab_host(&ctx).unwrap();
        let invite = Invite::parse(
            a.workspace
                .collab
                .as_ref()
                .unwrap()
                .invite
                .as_ref()
                .unwrap(),
        );
        let invite = invite.unwrap();
        let port = invite.addr.rsplit_once(':').unwrap().1.to_string();
        let mut b = crate::project::tests::test_app_pub(Canvas::new(32, 32, Color32::WHITE, 64));
        b.workspace.collab_form.invite = Invite {
            addr: format!("127.0.0.1:{port}"),
            ..invite
        }
        .code();
        // Joined from the library, which closes on the host's document.
        b.workspace.library.open = true;
        b.collab_join(&ctx).unwrap();
        pump(&mut a, &mut b, |_, b| b.canvas.width() == 128);
        assert!(!b.workspace.library.open);
        (a, b)
    }

    fn ids(app: &PainterApp) -> Vec<u64> {
        app.canvas.layers.iter().map(|l| l.id.0).collect()
    }

    #[test]
    fn two_apps_paint_together() {
        let (mut a, mut b) = session();
        let step_a = step(&a.canvas, &[(1, 1)], RED);
        a.layer_state.history.push_action(step_a);
        let step_b = step(&b.canvas, &[(2, 2)], BLUE);
        b.layer_state.history.push_action(step_b);
        pump(&mut a, &mut b, |a, b| {
            [a, b]
                .iter()
                .all(|app| get(&app.canvas, 1, 1) == RED && get(&app.canvas, 2, 2) == BLUE)
        });

        // The guest's tile drifts (no step says so); the next checksums
        // put it right.
        put(&b.canvas, 5, 5, BLUE);
        a.workspace.collab.as_mut().unwrap().last_hashes = Instant::now() - HASH_EVERY;
        pump(&mut a, &mut b, |_, b| {
            get(&b.canvas, 5, 5) == Color32::TRANSPARENT
        });
        assert_eq!(get(&b.canvas, 1, 1), RED);
    }

    #[test]
    fn layer_changes_keep_everyones_undo() {
        let (mut a, mut b) = session();
        let painted = step(&b.canvas, &[(3, 3)], BLUE);
        b.layer_state.history.push_action(painted);
        pump(&mut a, &mut b, |a, _| get(&a.canvas, 3, 3) == BLUE);
        // Both add a layer at once: two layers, not one id twice.
        a.add_layer_and_select();
        b.add_layer_and_select();
        pump(&mut a, &mut b, |a, b| {
            a.canvas.layers.len() == 4 && ids(a) == ids(b)
        });
        let mut all = ids(&a);
        all.dedup();
        assert_eq!(all.len(), 4);
        // The guest's undo history is still its own.
        assert_eq!(b.layer_state.history.stacks().0.len(), 2);
        // Its add undone, for everyone.
        b.apply_history_now(false);
        pump(&mut a, &mut b, |a, b| {
            a.canvas.layers.len() == 3 && ids(a) == ids(b)
        });
        b.apply_history_now(false);
        pump(&mut a, &mut b, |a, _| {
            get(&a.canvas, 3, 3) == Color32::TRANSPARENT
        });
    }

    #[test]
    fn a_duplicated_layer_arrives_with_its_pixels() {
        let (mut a, mut b) = session();
        a.canvas_mut().active_layer_idx = 1;
        let painted = step(&a.canvas, &[(1, 1)], RED);
        a.layer_state.history.push_action(painted);
        a.duplicate_layer();
        pump(&mut a, &mut b, |_, b| {
            b.canvas.layers.len() == 3
                && b.canvas
                    .get_layer_tile_data(2, 0, 0)
                    .is_some_and(|t| t[64 + 1] == RED)
        });
    }

    #[test]
    fn undoing_a_resize_is_refused_while_painting_together() {
        let (mut a, mut b) = session();
        let crop = crate::canvas::geometry::ImageOp::Reframe {
            x: 0,
            y: 0,
            w: 64,
            h: 64,
        };
        a.apply_image_op(crop);
        pump(&mut a, &mut b, |_, b| b.canvas.width() == 64);
        a.apply_history_now(false);
        assert_eq!(a.canvas.width(), 64);
    }

    #[test]
    fn on_another_tab_changes_wait_for_coming_back() {
        let (mut a, mut b) = session();
        // The host opens another document: the session's goes to a tab.
        a.workspace.library.has_document = true;
        let painted = step(&a.canvas, &[(1, 1)], RED);
        a.layer_state.history.push_action(painted);
        a.replace_document(Canvas::new(64, 64, Color32::WHITE, 64), History::new());
        assert!(a.workspace.collab.is_some());
        let painted = step(&b.canvas, &[(2, 2)], BLUE);
        b.layer_state.history.push_action(painted);
        pump(&mut a, &mut b, |_, b| get(&b.canvas, 1, 1) == RED);
        for _ in 0..20 {
            a.collab_tick();
            std::thread::sleep(Duration::from_millis(5));
        }
        // Not written into the document showing.
        assert_eq!(a.canvas.width(), 64);
        assert_eq!(get(&a.canvas, 2, 2), Color32::TRANSPARENT);
        a.switch_tab(0);
        pump(&mut a, &mut b, |a, _| {
            a.canvas.width() == 128 && get(&a.canvas, 2, 2) == BLUE
        });
    }

    #[test]
    fn a_text_edit_sends_the_layer_list_not_the_document() {
        let (mut a, mut b) = session();
        let painted = step(&b.canvas, &[(3, 3)], BLUE);
        b.layer_state.history.push_action(painted);
        pump(&mut a, &mut b, |a, _| get(&a.canvas, 3, 3) == BLUE);
        a.canvas_mut().layers[1].name = "Title".into();
        a.layer_state.history.push_action(UndoAction {
            tiles: Vec::new(),
            selection: None,
            transform: None,
            layer_action: Some(LayerHistoryOp::Text {
                layers: vec![(LayerId(1), None)],
                inner: None,
            }),
        });
        pump(&mut a, &mut b, |_, b| b.canvas.layers[1].name == "Title");
        assert_eq!(get(&b.canvas, 3, 3), BLUE);
        assert_eq!(b.layer_state.history.stacks().0.len(), 1);
    }

    #[test]
    fn a_stroke_is_painted_on_the_other_canvas_as_it_is_painted() {
        // The host painting, then a guest (through the host).
        for guest_paints in [false, true] {
            let (mut host, mut guest) = session();
            let (a, b) = if guest_paints {
                (&mut guest, &mut host)
            } else {
                (&mut host, &mut guest)
            };
            a.canvas_mut().active_layer_idx = 1;
            let brush = &mut a.brush_state.brush;
            brush.brush_options.color = RED;
            brush.brush_options.diameter = 12.0;
            brush.brush_options.hardness = 100.0;
            brush.brush_options.pressure_size = false;
            a.start_stroke_with_pressure(Vec2::new(5.0, 20.0), 1.0);
            a.add_stroke_point(Vec2::new(50.0, 20.0), 1.0);
            // The pen still down: painted there with the brush already.
            let painted = |app: &PainterApp| {
                (app.canvas.get_layer_tile_data(1, 0, 0))
                    .is_some_and(|t| t[20 * 64 + 30].a() == 255)
            };
            pump(a, b, |_, b| painted(b));
            assert!(a.brush_state.is_drawing);
            // Lifted: the painter's own pixels, exactly.
            a.finish_stroke();
            a.settle_strokes();
            pump(a, b, |a, b| {
                let tile = |app: &PainterApp| app.canvas.get_layer_tile_data(1, 0, 0);
                painted(a) && tile(a) == tile(b)
            });
        }
    }

    /// Painted here, the stroke goes past the painter's selection (only
    /// the brush is sent); once their pixels come, that part goes.
    #[test]
    fn what_the_painting_here_got_wrong_is_put_back() {
        let (mut a, mut b) = session();
        a.canvas_mut().active_layer_idx = 1;
        a.selection_manager.current_shape = Some(crate::selection::SelectionShape::Rectangle {
            start: Vec2::ZERO,
            end: Vec2::new(25.0, 64.0),
        });
        let brush = &mut a.brush_state.brush;
        brush.brush_options.color = RED;
        brush.brush_options.diameter = 12.0;
        brush.brush_options.hardness = 100.0;
        a.start_stroke_with_pressure(Vec2::new(5.0, 20.0), 1.0);
        a.add_stroke_point(Vec2::new(50.0, 20.0), 1.0);
        let at = |app: &PainterApp, x: usize| {
            (app.canvas.get_layer_tile_data(1, 0, 0)).map_or(0, |t| t[20 * 64 + x].a())
        };
        pump(&mut a, &mut b, |_, b| at(b, 40) == 255);
        a.finish_stroke();
        a.settle_strokes();
        pump(&mut a, &mut b, |_, b| at(b, 40) == 0 && at(b, 10) == 255);
    }

    /// Strokes one after the other, seen together (none sent while
    /// painting): undone, they leave nothing (each kept what it left, not
    /// what the next one painted over it).
    #[test]
    fn steps_seen_together_undo_cleanly() {
        let mut app = crate::project::tests::test_app_pub(canvas());
        let (_tx, rx) = channel();
        let mut c = Collab::new(true, "me", rx, Arc::default());
        c.reset_steps(&app.layer_state.history);
        app.workspace.collab = Some(c);
        let first = step(&app.canvas, &[(1, 1), (2, 2)], RED);
        app.layer_state.history.push_action(first);
        let second = step(&app.canvas, &[(2, 2)], BLUE);
        app.layer_state.history.push_action(second);
        app.collab_tick();
        app.apply_history_now(false);
        assert_eq!(get(&app.canvas, 2, 2), RED);
        app.apply_history_now(false);
        assert_eq!(get(&app.canvas, 1, 1), Color32::TRANSPARENT);
        assert_eq!(get(&app.canvas, 2, 2), Color32::TRANSPARENT);
    }

    /// A real stroke, undone and redone and undone: nothing left of it on
    /// either side.
    #[test]
    fn a_stroke_undone_and_redone_leaves_nothing() {
        for guest_paints in [false, true] {
            let (mut host, mut guest) = session();
            let (a, b) = if guest_paints {
                (&mut guest, &mut host)
            } else {
                (&mut host, &mut guest)
            };
            a.canvas_mut().active_layer_idx = 1;
            let brush = &mut a.brush_state.brush;
            brush.brush_options.color = RED;
            brush.brush_options.diameter = 12.0;
            a.start_stroke_with_pressure(Vec2::new(5.0, 20.0), 1.0);
            a.add_stroke_point(Vec2::new(30.0, 20.0), 1.0);
            a.add_stroke_point(Vec2::new(50.0, 22.0), 1.0);
            a.finish_stroke();
            a.settle_strokes();
            let tile = |app: &PainterApp| app.canvas.get_layer_tile_data(1, 0, 0);
            pump(a, b, |a, b| tile(a).is_some() && tile(a) == tile(b));
            let left =
                |app: &PainterApp| tile(app).map_or(0, |t| t.iter().filter(|p| p.a() > 0).count());
            for redo in [false, true, false] {
                a.apply_history_now(redo);
                pump(a, b, |a, b| tile(a) == tile(b));
            }
            assert_eq!((left(a), left(b)), (0, 0), "guest painting: {guest_paints}");
        }
    }

    /// Undone straight after it's painted, before the session has seen
    /// it (the same frame): still nothing left.
    #[test]
    fn a_stroke_undone_at_once_leaves_nothing() {
        let (mut a, mut b) = session();
        a.canvas_mut().active_layer_idx = 1;
        let brush = &mut a.brush_state.brush;
        brush.brush_options.color = RED;
        brush.brush_options.diameter = 12.0;
        let tile = |app: &PainterApp| app.canvas.get_layer_tile_data(1, 0, 0);
        let left =
            |app: &PainterApp| tile(app).map_or(0, |t| t.iter().filter(|p| p.a() > 0).count());
        for y in [20.0, 24.0] {
            a.start_stroke_with_pressure(Vec2::new(5.0, y), 1.0);
            a.add_stroke_point(Vec2::new(50.0, y), 1.0);
            a.finish_stroke();
            a.settle_strokes();
            // The first one seen by the session; not the second.
            if y == 20.0 {
                pump(&mut a, &mut b, |a, b| {
                    tile(a).is_some() && tile(a) == tile(b)
                });
            }
        }
        a.apply_history_now(false);
        a.apply_history_now(true);
        a.apply_history_now(false);
        a.apply_history_now(false);
        // The other side takes what's come before this side's next frame
        // (the real order: the undo's pixels go at once, the frame's pen
        // samples at its end).
        for _ in 0..20 {
            b.collab_tick();
            std::thread::sleep(Duration::from_millis(5));
        }
        pump(&mut a, &mut b, |a, b| tile(a) == tile(b));
        // Whatever of the strokes is still on its way has come and been
        // painted by now.
        for _ in 0..40 {
            a.collab_tick();
            b.collab_tick();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!((left(&a), left(&b)), (0, 0));
    }

    /// A step undone before the session saw it (the same frame) is undone
    /// as itself, not with the record of the step before it.
    #[test]
    fn a_step_undone_before_it_was_seen_is_undone_whole() {
        let mut app = crate::project::tests::test_app_pub(canvas());
        let (_tx, rx) = channel();
        let mut c = Collab::new(true, "me", rx, Arc::default());
        c.reset_steps(&app.layer_state.history);
        app.workspace.collab = Some(c);
        let first = step(&app.canvas, &[(1, 1), (2, 2)], RED);
        app.layer_state.history.push_action(first);
        app.collab_tick();
        let second = step(&app.canvas, &[(2, 2)], BLUE);
        app.layer_state.history.push_action(second);
        app.apply_history_now(false);
        assert_eq!(get(&app.canvas, 2, 2), RED);
        app.apply_history_now(false);
        assert_eq!(get(&app.canvas, 2, 2), Color32::TRANSPARENT);
    }

    /// What the others paint is work to save: a guest's joined canvas, and
    /// the host's after a guest painted on it.
    #[test]
    fn what_others_paint_counts_as_unsaved() {
        let (mut a, mut b) = session();
        assert!(b.has_unsaved_work());
        a.mark_saved();
        assert!(!a.has_unsaved_work());
        let painted = step(&b.canvas, &[(4, 4)], BLUE);
        b.layer_state.history.push_action(painted);
        pump(&mut a, &mut b, |a, _| get(&a.canvas, 4, 4) == BLUE);
        assert!(a.has_unsaved_work());
    }

    /// Big strokes over many tiles, undone and redone while the session
    /// runs: undone to the start, nothing left on either side.
    #[test]
    fn big_strokes_undone_and_redone_leave_nothing() {
        for guest_paints in [false, true] {
            big_strokes(guest_paints);
        }
    }

    fn big_strokes(guest_paints: bool) {
        let ctx = egui::Context::default();
        let mut host =
            crate::project::tests::test_app_pub(Canvas::new(512, 512, Color32::WHITE, 64));
        host.workspace.collab_form.port = 0;
        host.collab_host(&ctx).unwrap();
        let invite = host
            .workspace
            .collab
            .as_ref()
            .unwrap()
            .invite
            .clone()
            .unwrap();
        let invite = Invite::parse(&invite).unwrap();
        let port = invite.addr.rsplit_once(':').unwrap().1.to_string();
        let mut guest =
            crate::project::tests::test_app_pub(Canvas::new(32, 32, Color32::WHITE, 64));
        guest.workspace.collab_form.invite = Invite {
            addr: format!("127.0.0.1:{port}"),
            ..invite
        }
        .code();
        guest.collab_join(&ctx).unwrap();
        pump(&mut host, &mut guest, |_, g| g.canvas.width() == 512);
        let (a, b) = if guest_paints {
            (&mut guest, &mut host)
        } else {
            (&mut host, &mut guest)
        };
        a.canvas_mut().active_layer_idx = 1;
        let brush = &mut a.brush_state.brush;
        brush.brush_options.color = RED;
        brush.brush_options.diameter = 133.0;
        brush.brush_options.hardness = 100.0;
        let painted = |app: &PainterApp| {
            let canvas = &app.canvas;
            canvas
                .layer_tile_keys(1)
                .into_iter()
                .filter_map(|(tx, ty)| canvas.get_layer_tile_data(1, tx, ty))
                .map(|t| t.iter().filter(|p| p.a() > 0).count())
                .sum::<usize>()
        };
        // A frame each, the host's checksums every time.
        let frames = |a: &mut PainterApp, b: &mut PainterApp, n: usize| {
            for _ in 0..n {
                for app in [&mut *a, &mut *b] {
                    app.sync_stroke_worker();
                    if let Some(c) = app.workspace.collab.as_mut() {
                        c.last_hashes = Instant::now() - HASH_EVERY;
                    }
                    app.collab_tick();
                }
                std::thread::sleep(Duration::from_millis(3));
            }
        };
        // (Kept short: the suite's timing tests run beside it.)
        for i in 0..4 {
            let y = 80.0 + 60.0 * i as f32;
            a.start_stroke_with_pressure(Vec2::new(60.0, y), 1.0);
            for k in 1..5 {
                let at = Vec2::new(60.0 + 50.0 * k as f32, y + 10.0 * (k % 3) as f32);
                a.add_stroke_point(at, 1.0);
                frames(a, b, 1);
            }
            a.finish_stroke();
            // Sometimes undone and redone at once, sometimes a while after.
            if i % 2 == 0 {
                a.apply_history(false);
                a.apply_history(true);
            }
            frames(a, b, i % 3);
        }
        for redo in [false, false, true, false, true, true, false] {
            a.apply_history(redo);
            frames(a, b, 1);
        }
        while !a.layer_state.history.stacks().0.is_empty() {
            a.apply_history(false);
            frames(a, b, 1);
        }
        frames(a, b, 60);
        assert_eq!(
            (painted(a), painted(b)),
            (0, 0),
            "guest painting: {guest_paints}"
        );
    }
}
