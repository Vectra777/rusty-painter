//! The relay for painting together (the app's `app/collab.rs`) when the
//! host can't be reached directly. Run it where both sides can reach it:
//! `relay [port]`, 7879 by default.
//!
//! A host holds a room open. Each guest asking for that room is announced
//! to the host, which opens a connection back for it, and the relay copies
//! bytes between the two. The painting's TLS runs end to end through it:
//! the relay never sees pixels or passwords.
//!
//! Protocol, one line each, before the bytes are piped:
//! - `HOST <room>`, answered `OK`; the relay then writes `CONNECT <n>` per
//!   guest, and the host's `PING`s are read and dropped.
//! - `JOIN <room>`, answered `OK` once the host picks it up.
//! - `ACCEPT <room> <n>`: the host's connection for guest `n`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Guests waiting for their host to pick them up, per room.
const MAX_WAITING: usize = 16;

struct Room {
    control: TcpStream,
    waiting: HashMap<u64, TcpStream>,
    next: u64,
}

type Rooms = Arc<Mutex<HashMap<String, Room>>>;

fn main() {
    let port = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(7879u16);
    let listener = TcpListener::bind(("0.0.0.0", port)).expect("the port is free");
    eprintln!("Relay listening on port {port}");
    let rooms = Rooms::default();
    for stream in listener.incoming().flatten() {
        let rooms = rooms.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve(stream, &rooms) {
                eprintln!("{e}");
            }
        });
    }
}

/// A line, read a byte at a time so nothing after it is taken.
fn read_line(s: &mut TcpStream) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut b = [0];
    while line.len() < 128 {
        if s.read(&mut b)? == 0 || b[0] == b'\n' {
            break;
        }
        line.push(b[0]);
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

fn valid_room(room: &str) -> bool {
    (1..=32).contains(&room.len()) && room.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn serve(mut s: TcpStream, rooms: &Rooms) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let line = read_line(&mut s)?;
    let words: Vec<&str> = line.split(' ').collect();
    let lock = || rooms.lock().unwrap_or_else(|e| e.into_inner());
    match words[..] {
        ["HOST", room] if valid_room(room) => {
            {
                let mut rooms = lock();
                if rooms.contains_key(room) {
                    return s.write_all(b"ERR That room is taken\n");
                }
                let room_entry = Room {
                    control: s.try_clone()?,
                    waiting: HashMap::new(),
                    next: 0,
                };
                rooms.insert(room.to_string(), room_entry);
            }
            s.write_all(b"OK\n")?;
            // Held while the host is there (it pings every 30 s).
            s.set_read_timeout(Some(Duration::from_secs(120)))?;
            let mut b = [0; 64];
            while matches!(s.read(&mut b), Ok(n) if n > 0) {}
            // (Guests still waiting go with it.)
            lock().remove(room);
            Ok(())
        }
        ["JOIN", room] => {
            let mut rooms = lock();
            let Some(room) = rooms.get_mut(room) else {
                return s.write_all(b"ERR No such room\n");
            };
            // A guest the host never picks up waits until the host leaves;
            // the cap keeps a room from filling with them.
            if room.waiting.len() >= MAX_WAITING {
                return s.write_all(b"ERR The room is busy\n");
            }
            let n = room.next;
            room.next += 1;
            room.control
                .write_all(format!("CONNECT {n}\n").as_bytes())?;
            room.waiting.insert(n, s);
            Ok(())
        }
        ["ACCEPT", room, n] => {
            let guest = n
                .parse()
                .ok()
                .and_then(|n| lock().get_mut(room)?.waiting.remove(&n));
            let Some(mut guest) = guest else {
                return Ok(());
            };
            guest.write_all(b"OK\n")?;
            s.set_read_timeout(None)?;
            guest.set_read_timeout(None)?;
            pipe(s, guest)
        }
        _ => s.write_all(b"ERR Unknown request\n"),
    }
}

/// Copy bytes both ways until either side goes.
fn pipe(a: TcpStream, b: TcpStream) -> std::io::Result<()> {
    let (mut a_read, mut b_write) = (a.try_clone()?, b.try_clone()?);
    let there = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a_read, &mut b_write);
        let _ = b_write.shutdown(Shutdown::Both);
    });
    let (mut a, mut b) = (a, b);
    let _ = std::io::copy(&mut b, &mut a);
    let _ = a.shutdown(Shutdown::Both);
    let _ = there.join();
    Ok(())
}
