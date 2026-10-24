//! Server List Ping (Minecraft 1.7+): онлайн, версия, MOTD.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct PingInfo {
    pub online: i64,
    pub max: i64,
    pub players: Vec<String>,
    pub version: String,
    pub motd: String,
    pub latency_ms: Option<u128>,
}

fn varint(mut n: u32, out: &mut Vec<u8>) {
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n != 0 {
            out.push(b | 0x80);
        } else {
            out.push(b);
            return;
        }
    }
}

fn read_varint(s: &mut TcpStream) -> std::io::Result<u32> {
    let mut num = 0u32;
    for i in 0..5 {
        let mut b = [0u8];
        s.read_exact(&mut b)?;
        num |= ((b[0] & 0x7F) as u32) << (7 * i);
        if b[0] & 0x80 == 0 {
            return Ok(num);
        }
    }
    Err(std::io::Error::other("VarInt too long"))
}

fn packet(id: u32, data: &[u8]) -> Vec<u8> {
    let mut body = vec![];
    varint(id, &mut body);
    body.extend_from_slice(data);
    let mut out = vec![];
    varint(body.len() as u32, &mut out);
    out.extend(body);
    out
}

fn flatten(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a.iter().map(flatten).collect(),
        serde_json::Value::Object(o) => {
            let mut t = o.get("text").map(flatten).unwrap_or_default();
            if let Some(serde_json::Value::Array(extra)) = o.get("extra") {
                for e in extra {
                    t.push_str(&flatten(e));
                }
            }
            t
        }
        _ => String::new(),
    }
}

pub fn strip_colors(s: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for c in s.chars() {
        if skip {
            skip = false;
        } else if c == '§' {
            skip = true;
        } else {
            out.push(c);
        }
    }
    out
}

pub fn ping(host: &str, port: u16, timeout: Duration) -> std::io::Result<PingInfo> {
    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .map_err(|_| std::io::Error::other("bad address"))?;
    let mut s = TcpStream::connect_timeout(&addr, timeout)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    let mut hs = vec![];
    varint(u32::MAX, &mut hs); // протокол -1
    varint(host.len() as u32, &mut hs);
    hs.extend_from_slice(host.as_bytes());
    hs.extend_from_slice(&port.to_be_bytes());
    varint(1, &mut hs);
    let mut req = packet(0, &hs);
    req.extend(packet(0, &[]));
    s.write_all(&req)?;
    read_varint(&mut s)?;
    read_varint(&mut s)?;
    let len = read_varint(&mut s)? as usize;
    if len > 1 << 21 {
        return Err(std::io::Error::other("ответ слишком большой"));
    }
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf)?;
    let data: serde_json::Value =
        serde_json::from_slice(&buf).map_err(|e| std::io::Error::other(e.to_string()))?;
    let t0 = Instant::now();
    let mut latency = None;
    if s.write_all(&packet(1, &1i64.to_be_bytes())).is_ok() {
        let mut tail = [0u8; 8];
        if read_varint(&mut s).is_ok() && read_varint(&mut s).is_ok() && s.read_exact(&mut tail).is_ok() {
            latency = Some(t0.elapsed().as_millis());
        }
    }
    let players = &data["players"];
    Ok(PingInfo {
        online: players["online"].as_i64().unwrap_or(0),
        max: players["max"].as_i64().unwrap_or(0),
        players: players["sample"]
            .as_array()
            .map(|a| a.iter().filter_map(|p| p["name"].as_str().map(String::from)).collect())
            .unwrap_or_default(),
        version: data["version"]["name"].as_str().unwrap_or("?").to_string(),
        motd: strip_colors(&flatten(&data["description"])),
        latency_ms: latency,
    })
}
