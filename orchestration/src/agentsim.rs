// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A stand-in for the Deepgram Voice Agent API, for deterministic tests.
//!
//! [`AgentSim`] listens on a local port; the agent in Veda connects to
//! it (QEMU's NAT makes the host 10.0.2.2) when booted with the argument
//! from [`AgentSim::boot_arg`]. It speaks the same WebSocket protocol as
//! Deepgram: it answers the `Settings` message with `Welcome` and
//! `SettingsApplied`, and then does what the script says — send a function
//! call and collect its result, play voice, report that the user started
//! speaking — while recording everything the agent sends.
//!
//! It also stands in for Deepgram's streaming speech recognition, which
//! the agent uses to hear its name while asleep: the script decides what
//! was "heard" (`agent-hear TEXT`) once the agent streams audio to it.
//!
//! Script commands (see `script.rs`): `agent-connected`, `agent-call NAME
//! ARGS-JSON`, `agent-result TEXT`, `agent-expect TEXT`, `agent-speak
//! SECONDS`, `agent-interrupt`, `agent-audio BYTES`, `agent-hear TEXT`, and
//! `agent-asleep` (the agent is not woken at start).
//!
//! Each connection is read by a thread of its own, and written by the
//! script: neither waits for the other longer than a slice of time, so a
//! guest slow to read what the script sends never keeps its microphone's
//! stream from being read.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;
use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::{Message, WebSocket};

use crate::util::Result;
use crate::voice;

/// What the simulator has received.
#[derive(Default)]
struct Log {
    /// Text messages from the agent, in order.
    texts: Vec<String>,
    /// Bytes of microphone audio received.
    audio_bytes: usize,
    /// The level of each piece of microphone audio received (dBFS).
    mic_levels: Vec<f32>,
    /// Speech recognition: connections, and audio received.
    listen_connections: u32,
    listen_audio_bytes: usize,
}

/// A WebSocket connection from the agent. Its reader holds the lock only
/// while it waits for a message, a few milliseconds at a time, and a
/// writer only while it hands a message over or the socket takes a slice
/// of it, so reading and writing take turns.
type Socket = Arc<Mutex<WebSocket<TcpStream>>>;

/// How long a reader waits for a message before letting writers in, and a
/// writer for the socket to take what it sends before letting the reader
/// in.
const READ_SLICE: Duration = Duration::from_millis(10);
const WRITE_SLICE: Duration = Duration::from_millis(10);
/// How long a message may take to go out whole: a guest that reads
/// nothing for so long has stopped.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// A conversation: its connection, and whether the agent has configured
/// it (sent `Settings`).
struct Conversation {
    socket: Socket,
    configured: AtomicBool,
}

struct Shared {
    log: Mutex<Log>,
    /// The open conversation.
    agent: Mutex<Option<Arc<Conversation>>>,
    /// The open speech recognition connection.
    listen: Mutex<Option<Socket>>,
}

/// Clears `slot` when it is dropped, if it still holds `mine` (a later
/// connection may have taken its place): however the connection ended.
struct Leaving<'a, T> {
    slot: &'a Mutex<Option<Arc<T>>>,
    mine: Arc<T>,
}

impl<T> Drop for Leaving<'_, T> {
    fn drop(&mut self) {
        let mut slot = self.slot.lock().unwrap();
        if slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, &self.mine)) {
            *slot = None;
        }
    }
}

/// The simulated Voice Agent service.
pub struct AgentSim {
    port: u16,
    shared: Arc<Shared>,
    next_call: u32,
}

impl AgentSim {
    pub fn start() -> Result<AgentSim> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("agent simulator: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let shared =
            Arc::new(Shared { log: Mutex::new(Log::default()), agent: Mutex::new(None), listen: Mutex::new(None) });
        let s = shared.clone();
        std::thread::Builder::new()
            .name("agentsim".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let s2 = s.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = serve(stream, &s2) {
                            eprintln!("agent simulator: {e}");
                        }
                    });
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(AgentSim { port, shared, next_call: 1 })
    }

    /// The kernel command line arguments pointing the agent here, at
    /// `host` (this computer as the guest's network sees it), and with
    /// `wake` starting a conversation at once.
    pub fn boot_arg(&self, host: &str, wake: bool) -> String {
        format!(
            "agent.endpoint=ws://{host}:{0}/v1/agent/converse agent.listen=ws://{host}:{0}/v1/listen{1}",
            self.port,
            if wake { " agent.wake" } else { "" }
        )
    }

    /// Once the agent streams audio for recognition, says that `text` was
    /// heard (a final transcript at the end of an utterance). Pieces split
    /// by " | " come as final transcripts of their own, 0.3 s apart, and
    /// only the last ends the utterance (like "Hey Veda," | "what time is
    /// it?" from Deepgram).
    pub fn hear(&self, text: &str, timeout: Duration) -> Result {
        let start = Instant::now();
        loop {
            let streaming =
                self.shared.listen.lock().unwrap().is_some() && self.shared.log.lock().unwrap().listen_audio_bytes > 0;
            if streaming {
                break;
            }
            if start.elapsed() > timeout {
                return Err("the agent did not stream speech for recognition".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let pieces: Vec<&str> = text.split(" | ").collect();
        for (i, piece) in pieces.iter().enumerate() {
            if i > 0 {
                std::thread::sleep(Duration::from_millis(300));
            }
            let result = json!({
                "type": "Results",
                "is_final": true,
                "speech_final": i + 1 == pieces.len(),
                "channel": { "alternatives": [{ "transcript": piece, "confidence": 0.98 }] },
            });
            let socket = self.shared.listen.lock().unwrap().clone().ok_or("the recognition stream closed")?;
            send(&socket, Message::text(result.to_string()))?;
        }
        Ok(())
    }

    /// How many recognition streams the agent opened.
    pub fn listen_connections(&self) -> u32 {
        self.shared.log.lock().unwrap().listen_connections
    }

    /// Waits until the agent has no recognition stream open.
    pub fn wait_not_listening(&self, timeout: Duration) -> Result {
        let start = Instant::now();
        while self.shared.listen.lock().unwrap().is_some() {
            if start.elapsed() > timeout {
                return Err("the agent kept listening".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    fn send(&self, message: Message) -> Result {
        let conversation =
            self.shared.agent.lock().unwrap().clone().ok_or("the agent is not connected to the simulator")?;
        send(&conversation.socket, message)
    }

    pub fn send_json(&self, json: &str) -> Result {
        self.send(Message::text(json))
    }

    /// Waits until the agent is connected, and has configured the
    /// conversation it has open now (a conversation before it does not
    /// count).
    pub fn wait_connected(&self, timeout: Duration) -> Result {
        let start = Instant::now();
        loop {
            let open = self.shared.agent.lock().unwrap().clone();
            if open.is_some_and(|c| c.configured.load(Ordering::SeqCst)) {
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err("the agent did not connect to the simulator".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn texts(&self) -> Vec<String> {
        self.shared.log.lock().unwrap().texts.clone()
    }

    /// How many pieces of microphone audio have arrived.
    pub fn mic_levels_len(&self) -> usize {
        self.shared.log.lock().unwrap().mic_levels.len()
    }

    /// The loudest microphone audio received after the first `skip`
    /// pieces (dBFS, 20 ms windows).
    pub fn mic_peak_since(&self, skip: usize) -> f32 {
        self.shared.log.lock().unwrap().mic_levels.iter().skip(skip).copied().fold(-120.0, f32::max)
    }

    pub fn audio_bytes(&self) -> usize {
        self.shared.log.lock().unwrap().audio_bytes
    }

    /// Waits until the agent sent a text message containing `needle`
    /// (after the first `skip` messages); returns it.
    pub fn wait_text(&self, needle: &str, skip: usize, timeout: Duration) -> Result<String> {
        let start = Instant::now();
        loop {
            if let Some(t) = self.texts().into_iter().skip(skip).find(|t| t.contains(needle)) {
                return Ok(t);
            }
            if start.elapsed() > timeout {
                return Err(format!("the agent did not send \"{needle}\" in time"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Asks the agent to run a function and returns its result.
    pub fn call(&mut self, name: &str, args_json: &str, timeout: Duration) -> Result<String> {
        let id = format!("call-{}", self.next_call);
        self.next_call += 1;
        let skip = self.texts().len();
        // Deepgram gives the arguments as JSON in a string.
        let request = json!({
            "type": "FunctionCallRequest",
            "functions": [{ "id": id, "name": name, "arguments": args_json, "client_side": true }],
        });
        self.send_json(&request.to_string())?;
        let response = self.wait_text(&format!("\"id\":\"{id}\""), skip, timeout)?;
        Ok(content(&response).unwrap_or(response))
    }

    /// The agent "speaks" for `secs` seconds: the agent's voice-like buzz
    /// (see `voice`) at 24 kHz.
    pub fn speak(&self, secs: f64) -> Result {
        self.send_json(
            r#"{"type":"AgentStartedSpeaking","total_latency":"0.5","tts_latency":"0.1","ttt_latency":"0.4"}"#,
        )?;
        let pcm: Vec<u8> =
            voice::buzz(voice::AGENT, 24_000, secs, 6000.0).iter().flat_map(|s| s.to_le_bytes()).collect();
        for chunk in pcm.chunks(4800) {
            self.send(Message::binary(chunk.to_vec()))?;
        }
        self.send_json(r#"{"type":"AgentAudioDone"}"#)
    }
}

/// The result in a `FunctionCallResponse`, as the language model reads it
/// (the response carries it as a JSON string).
fn content(response: &str) -> Option<String> {
    serde_json::from_str::<Value>(response).ok()?["content"].as_str().map(String::from)
}

/// Whether an error of the socket's is only that it would wait: the
/// message stays with tungstenite, and goes out when it is flushed again.
fn would_wait(e: &tungstenite::Error) -> bool {
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    matches!(e, tungstenite::Error::Io(e) if matches!(e.kind(), WouldBlock | TimedOut))
}

/// Sends a message on a connection: hands it over, then has the socket
/// take it a slice of time at a time, letting the reader in between.
fn send(socket: &Socket, message: Message) -> Result {
    let failed = |e: tungstenite::Error| format!("agent simulator: {e}");
    match socket.lock().unwrap().write(message) {
        Err(e) if !would_wait(&e) => return Err(failed(e)),
        _ => {}
    }
    let start = Instant::now();
    loop {
        match socket.lock().unwrap().flush() {
            Ok(()) => return Ok(()),
            Err(e) if !would_wait(&e) => return Err(failed(e)),
            Err(_) => {}
        }
        if start.elapsed() > SEND_TIMEOUT {
            return Err(format!("agent simulator: the agent read nothing for {} s", SEND_TIMEOUT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The next message on a connection, or `None` once it has closed. Pings
/// are answered and the closing handshake completed by tungstenite.
fn receive(socket: &Socket) -> Result<Option<Message>> {
    loop {
        let read = socket.lock().unwrap().read();
        match read {
            Ok(message) => return Ok(Some(message)),
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                // Writers' turn.
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return Ok(None),
            Err(tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake)) => {
                return Ok(None);
            }
            Err(e) => return Err(format!("agent simulator: {e}")),
        }
    }
}

/// One connection from the agent: the upgrade (with Deepgram's token
/// authentication), then the conversation or speech recognition its path
/// asks for.
fn serve(stream: TcpStream, shared: &Shared) -> Result {
    let _ = stream.set_nodelay(true);
    let mut path = String::new();
    // The error type is tungstenite's: what the handshake answers with.
    #[allow(clippy::result_large_err)]
    let check = |request: &Request, response: Response| -> std::result::Result<Response, ErrorResponse> {
        path = request.uri().path().to_string();
        let token = request.headers().get("authorization").and_then(|v| v.to_str().ok());
        if token.is_some_and(|t| t.len() > 6 && t[..6].eq_ignore_ascii_case("token ")) {
            Ok(response)
        } else {
            let mut refusal = ErrorResponse::new(None);
            *refusal.status_mut() = StatusCode::UNAUTHORIZED;
            Err(refusal)
        }
    };
    let ws = tungstenite::accept_hdr(stream, check).map_err(|e| format!("the WebSocket upgrade: {e}"))?;
    ws.get_ref().set_read_timeout(Some(READ_SLICE)).map_err(|e| e.to_string())?;
    ws.get_ref().set_write_timeout(Some(WRITE_SLICE)).map_err(|e| e.to_string())?;
    let socket: Socket = Arc::new(Mutex::new(ws));
    if path.starts_with("/v1/listen") {
        return serve_listen(&socket, shared);
    }
    let conversation = Arc::new(Conversation { socket: socket.clone(), configured: AtomicBool::new(false) });
    *shared.agent.lock().unwrap() = Some(conversation.clone());
    let _leaving = Leaving { slot: &shared.agent, mine: conversation.clone() };
    send(&socket, Message::text(r#"{"type":"Welcome","request_id":"sim-1"}"#))?;
    while let Some(message) = receive(&socket)? {
        match message {
            Message::Text(text) => {
                let settings = text.contains("\"type\":\"Settings\"");
                shared.log.lock().unwrap().texts.push(text.to_string());
                if settings {
                    send(&socket, Message::text(r#"{"type":"SettingsApplied"}"#))?;
                    conversation.configured.store(true, Ordering::SeqCst);
                }
            }
            Message::Binary(audio) => {
                let mut log = shared.log.lock().unwrap();
                log.audio_bytes += audio.len();
                // 20 ms at a time (16 kHz, 16-bit).
                for chunk in audio.chunks(640) {
                    let n = chunk.len() / 2;
                    if n == 0 {
                        continue;
                    }
                    let sum: f64 = chunk
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| {
                            let x = i16::from_le_bytes(*b) as f64 / 32768.0;
                            x * x
                        })
                        .sum();
                    let db = if sum <= 0.0 { -120.0 } else { (10.0 * (sum / n as f64).log10()) as f32 };
                    log.mic_levels.push(db);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A speech recognition stream: audio in; transcripts come from the
/// script (`agent-hear`). `CloseStream` ends it as Deepgram does.
fn serve_listen(socket: &Socket, shared: &Shared) -> Result {
    *shared.listen.lock().unwrap() = Some(socket.clone());
    let _leaving = Leaving { slot: &shared.listen, mine: socket.clone() };
    shared.log.lock().unwrap().listen_connections += 1;
    while let Some(message) = receive(socket)? {
        match message {
            Message::Text(text) => {
                if text.contains("CloseStream") {
                    let _ = send(socket, Message::text(r#"{"type":"Metadata","duration":1.0}"#));
                    let _ =
                        socket.lock().unwrap().close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }));
                    break;
                }
            }
            Message::Binary(audio) => shared.log.lock().unwrap().listen_audio_bytes += audio.len(),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::client::IntoClientRequest;

    /// Connects to the simulator as the agent does (`token`: the
    /// Authorization header's value, if any).
    fn connect(sim: &AgentSim, path: &str, token: Option<&str>) -> tungstenite::Result<WebSocket<TcpStream>> {
        let mut request = format!("ws://127.0.0.1:{}{path}", sim.port).into_client_request()?;
        if let Some(token) = token {
            request.headers_mut().insert("Authorization", token.parse().unwrap());
        }
        let stream = TcpStream::connect(("127.0.0.1", sim.port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        tungstenite::client(request, stream).map(|(ws, _)| ws).map_err(|e| match e {
            tungstenite::HandshakeError::Failure(e) => e,
            tungstenite::HandshakeError::Interrupted(_) => tungstenite::Error::ConnectionClosed,
        })
    }

    fn text(ws: &mut WebSocket<TcpStream>) -> String {
        loop {
            if let Message::Text(t) = ws.read().unwrap() {
                return t.to_string();
            }
        }
    }

    #[test]
    fn converses_as_deepgram_does() {
        let mut sim = AgentSim::start().unwrap();
        // Deepgram refuses a connection without a key.
        match connect(&sim, "/v1/agent/converse", None) {
            Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), StatusCode::UNAUTHORIZED),
            other => panic!("expected 401, got {:?}", other.map(|_| ())),
        }
        let mut agent = connect(&sim, "/v1/agent/converse", Some("Token k")).unwrap();
        assert!(text(&mut agent).contains("Welcome"));
        agent.send(Message::text(r#"{"type":"Settings","agent":{}}"#)).unwrap();
        assert!(text(&mut agent).contains("SettingsApplied"));
        sim.wait_connected(Duration::from_secs(10)).unwrap();
        // Microphone audio: a full-scale square wave is 0 dBFS.
        let mut audio = Vec::new();
        for i in 0..320 {
            audio.extend_from_slice(&(if i % 2 == 0 { i16::MAX } else { -i16::MAX }).to_le_bytes());
        }
        agent.send(Message::binary(audio)).unwrap();
        // A function call, whose result comes back as a JSON string.
        let result = std::thread::scope(|s| {
            let caller = s.spawn(|| sim.call("get_time", r#"{"zone":"UTC"}"#, Duration::from_secs(10)));
            let request: Value = serde_json::from_str(&text(&mut agent)).unwrap();
            assert_eq!(request["type"], "FunctionCallRequest");
            let function = &request["functions"][0];
            assert_eq!((&function["name"], &function["arguments"]), (&json!("get_time"), &json!(r#"{"zone":"UTC"}"#)));
            let response = json!({
                "type": "FunctionCallResponse",
                "id": function["id"],
                "name": "get_time",
                "content": r#"{"ok":true,"result":"12:00 \u00e9"}"#,
            });
            agent.send(Message::text(response.to_string())).unwrap();
            caller.join().unwrap()
        });
        assert_eq!(result.unwrap(), r#"{"ok":true,"result":"12:00 \u00e9"}"#);
        let start = Instant::now();
        while sim.mic_levels_len() == 0 && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!((sim.mic_peak_since(0) - 0.0).abs() < 0.01);
        agent.close(None).unwrap();
        while agent.read().is_ok() {}
    }

    #[test]
    fn recognises_what_the_script_says() {
        let sim = AgentSim::start().unwrap();
        let mut listen = connect(&sim, "/v1/listen?model=nova-3", Some("token k")).unwrap();
        listen.send(Message::binary(vec![0u8; 640])).unwrap();
        sim.hear("Hey Veda, | what time is it?", Duration::from_secs(10)).unwrap();
        let first: Value = serde_json::from_str(&text(&mut listen)).unwrap();
        let second: Value = serde_json::from_str(&text(&mut listen)).unwrap();
        assert_eq!(first["channel"]["alternatives"][0]["transcript"], "Hey Veda,");
        assert_eq!((&first["speech_final"], &second["speech_final"]), (&json!(false), &json!(true)));
        assert_eq!(sim.listen_connections(), 1);
        listen.send(Message::text(r#"{"type":"CloseStream"}"#)).unwrap();
        assert!(text(&mut listen).contains("Metadata"));
        sim.wait_not_listening(Duration::from_secs(10)).unwrap();
    }

    /// A conversation that ends after another opened leaves the new one
    /// open; a new conversation counts as connected once it is configured.
    #[test]
    fn a_new_conversation_takes_the_place_of_the_old() {
        let sim = AgentSim::start().unwrap();
        let mut first = connect(&sim, "/v1/agent/converse", Some("Token k")).unwrap();
        assert!(text(&mut first).contains("Welcome"));
        first.send(Message::text(r#"{"type":"Settings"}"#)).unwrap();
        assert!(text(&mut first).contains("SettingsApplied"));
        sim.wait_connected(Duration::from_secs(10)).unwrap();
        // The agent starts again: a second conversation, not yet
        // configured, does not count.
        let mut second = connect(&sim, "/v1/agent/converse", Some("Token k")).unwrap();
        assert!(text(&mut second).contains("Welcome"));
        let start = Instant::now();
        while !sim.shared.agent.lock().unwrap().as_ref().is_some_and(|c| !c.configured.load(Ordering::SeqCst)) {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sim.wait_connected(Duration::from_millis(300)).is_err());
        // The first one goes: the second stays the conversation.
        first.close(None).unwrap();
        while first.read().is_ok() {}
        drop(first);
        std::thread::sleep(Duration::from_millis(300));
        sim.send_json(r#"{"type":"Ping"}"#).unwrap();
        assert!(text(&mut second).contains("Ping"));
        second.send(Message::text(r#"{"type":"Settings"}"#)).unwrap();
        sim.wait_connected(Duration::from_secs(10)).unwrap();
    }

    /// The script's messages go out a slice at a time: an agent that reads
    /// none of them keeps sending its microphone's sound, which is read.
    #[test]
    fn a_guest_slow_to_read_is_still_heard() {
        let sim = AgentSim::start().unwrap();
        let mut agent = connect(&sim, "/v1/agent/converse", Some("Token k")).unwrap();
        assert!(text(&mut agent).contains("Welcome"));
        let start = Instant::now();
        while sim.shared.agent.lock().unwrap().is_none() {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        let conversation = sim.shared.agent.lock().unwrap().clone().unwrap();
        // More than the sockets' buffers hold, which the agent does not read.
        let sender = std::thread::spawn(move || send(&conversation.socket, Message::binary(vec![0u8; 64 << 20])));
        std::thread::sleep(Duration::from_millis(300));
        agent.get_ref().set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        for _ in 0..10 {
            agent.send(Message::binary(vec![1u8; 640])).unwrap();
        }
        let start = Instant::now();
        while sim.audio_bytes() < 6400 {
            assert!(start.elapsed() < Duration::from_secs(5), "the microphone's sound was not read");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(agent);
        assert!(sender.join().unwrap().is_err(), "the agent went away before it read the message");
    }

    #[test]
    fn reads_function_results() {
        let msg = r#"{"type":"FunctionCallResponse","id":"call-1","content":"{\"ok\":true,\"text\":\"a\\nb é\"}"}"#;
        assert_eq!(content(msg).as_deref(), Some(r#"{"ok":true,"text":"a\nb é"}"#));
        assert_eq!(content(r#"{"content":"cut"#), None);
    }
}
