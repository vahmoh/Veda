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
//! Script commands (see `automate.rs`): `agent-connected`, `agent-call NAME
//! ARGS-JSON`, `agent-result TEXT`, `agent-expect TEXT`, `agent-speak
//! SECONDS`, `agent-interrupt`, `agent-send JSON`, `agent-audio BYTES`,
//! `agent-hear TEXT`, and `agent-asleep` (the agent is not woken at start).

use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;
use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::{Message, WebSocket};

use crate::util::Result;

/// What the simulator has received.
#[derive(Default)]
struct Log {
    /// Text messages from the agent, in order.
    texts: Vec<String>,
    /// Bytes of microphone audio received.
    audio_bytes: usize,
    /// The level of each piece of microphone audio received (dBFS).
    mic_levels: Vec<f32>,
    connections: u32,
    /// Speech recognition: connections, audio received, text messages.
    listen_connections: u32,
    listen_audio_bytes: usize,
    listen_texts: Vec<String>,
}

/// A WebSocket connection from the agent. Its reader holds the lock only
/// while it waits for a message, a few milliseconds at a time, so the
/// script's messages go out between reads, whole.
type Socket = Arc<Mutex<WebSocket<TcpStream>>>;

/// How long a reader waits for a message before letting writers in.
const READ_SLICE: Duration = Duration::from_millis(10);

struct Shared {
    log: Mutex<Log>,
    /// The open conversation.
    agent: Mutex<Option<Socket>>,
    /// The open speech recognition connection.
    listen: Mutex<Option<Socket>>,
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
        let socket = self.shared.agent.lock().unwrap().clone().ok_or("the agent is not connected to the simulator")?;
        send(&socket, message)
    }

    pub fn send_json(&self, json: &str) -> Result {
        self.send(Message::text(json))
    }

    /// Waits until the agent is connected (and configured).
    pub fn wait_connected(&self, timeout: Duration) -> Result {
        let start = Instant::now();
        loop {
            if self.shared.agent.lock().unwrap().is_some() && self.texts().iter().any(|t| t.contains("\"Settings\"")) {
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

    /// The agent "speaks" for `secs` seconds: a voice-like buzz at
    /// 24 kHz (harmonics, a wandering pitch, syllables four times a
    /// second).
    pub fn speak(&self, secs: f64) -> Result {
        use std::f64::consts::TAU;
        self.send_json(
            r#"{"type":"AgentStartedSpeaking","total_latency":"0.5","tts_latency":"0.1","ttt_latency":"0.4"}"#,
        )?;
        let n = (secs * 24_000.0) as usize;
        let mut pcm = Vec::with_capacity(n * 2);
        let mut phase = 0.0f64;
        for i in 0..n {
            let t = i as f64 / 24_000.0;
            let pitch = 190.0 + 30.0 * (t * 1.1 * TAU).sin();
            phase += TAU * pitch / 24_000.0;
            let mut buzz = 0.0;
            let mut k = 1.0;
            while k * pitch < 5000.0 {
                buzz += (phase * k).sin() / k;
                k += 1.0;
            }
            let syllable = (t * 4.0 * TAU + 1.0).sin().max(0.0).sqrt();
            let s = (buzz * (0.1 + 0.9 * syllable) * 6000.0) as i16;
            pcm.extend_from_slice(&s.to_le_bytes());
        }
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

/// Sends a message on a connection.
fn send(socket: &Socket, message: Message) -> Result {
    socket.lock().unwrap().send(message).map_err(|e| format!("agent simulator: {e}"))
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
    let socket: Socket = Arc::new(Mutex::new(ws));
    if path.starts_with("/v1/listen") {
        return serve_listen(&socket, shared);
    }
    *shared.agent.lock().unwrap() = Some(socket.clone());
    shared.log.lock().unwrap().connections += 1;
    send(&socket, Message::text(r#"{"type":"Welcome","request_id":"sim-1"}"#))?;
    while let Some(message) = receive(&socket)? {
        match message {
            Message::Text(text) => {
                let settings = text.contains("\"type\":\"Settings\"");
                shared.log.lock().unwrap().texts.push(text.to_string());
                if settings {
                    send(&socket, Message::text(r#"{"type":"SettingsApplied"}"#))?;
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
    *shared.agent.lock().unwrap() = None;
    Ok(())
}

/// A speech recognition stream: audio in; transcripts come from the
/// script (`agent-hear`). `CloseStream` ends it as Deepgram does.
fn serve_listen(socket: &Socket, shared: &Shared) -> Result {
    *shared.listen.lock().unwrap() = Some(socket.clone());
    shared.log.lock().unwrap().listen_connections += 1;
    while let Some(message) = receive(socket)? {
        match message {
            Message::Text(text) => {
                let close = text.contains("CloseStream");
                shared.log.lock().unwrap().listen_texts.push(text.to_string());
                if close {
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
    *shared.listen.lock().unwrap() = None;
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

    #[test]
    fn reads_function_results() {
        let msg = r#"{"type":"FunctionCallResponse","id":"call-1","content":"{\"ok\":true,\"text\":\"a\\nb é\"}"}"#;
        assert_eq!(content(msg).as_deref(), Some(r#"{"ok":true,"text":"a\nb é"}"#));
        assert_eq!(content(r#"{"content":"cut"#), None);
    }
}
