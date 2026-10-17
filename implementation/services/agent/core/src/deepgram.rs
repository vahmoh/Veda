// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The Deepgram APIs the agent uses.
//!
//! * The **Voice Agent API** (`wss://agent.deepgram.com/v1/agent/converse`):
//!   one WebSocket carries the microphone (binary, 16 kHz linear16) to
//!   Deepgram and the agent's voice (binary, 24 kHz linear16) back, with
//!   JSON messages in between: [`settings`] configures speech recognition,
//!   the language model with its prompt and functions, and the voice;
//!   [`ServerMessage`] is everything Deepgram reports (turns, function
//!   calls, barge-in); the `*_message` functions build what the agent sends.
//! * **Streaming speech-to-text** (`wss://api.deepgram.com/v1/listen`):
//!   while asleep the agent listens for its name ([`listen_url`],
//!   [`parse_listen_result`]).
//! * **REST**: the key check, the catalogs of models and voices
//!   ([`parse_think_models`], [`parse_voices`]) and text-to-speech for voice
//!   previews ([`speak_url`]).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::{Value, json};

/// The Voice Agent endpoint.
pub const AGENT_URL: &str = "wss://agent.deepgram.com/v1/agent/converse";
/// Lists the language models the agent can think with.
pub const THINK_MODELS_URL: &str = "https://agent.deepgram.com/v1/agent/settings/think/models";
/// Lists speech-to-text and text-to-speech models.
pub const MODELS_URL: &str = "https://api.deepgram.com/v1/models";
/// Answers 200 for a valid key (used to check it).
pub const PROJECTS_URL: &str = "https://api.deepgram.com/v1/projects";
/// Sample rate of the microphone audio sent.
pub const INPUT_RATE: u32 = 16_000;
/// Sample rate of the voice received.
pub const OUTPUT_RATE: u32 = 24_000;

/// Whether `url` is one of Deepgram's own services, reached over TLS: the
/// only place the API key goes. Any other endpoint (a simulator in tests)
/// is sent the word `test` in its place.
pub fn is_deepgram(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("wss://").or_else(|| url.strip_prefix("https://")) else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // No user name before the host ("deepgram.com@elsewhere").
    if authority.contains('@') {
        return false;
    }
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => authority,
    };
    let host = host.to_ascii_lowercase();
    host == "deepgram.com" || host.ends_with(".deepgram.com")
}

/// Who said something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    /// The role as the protocol names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }

    /// The role a protocol name names.
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "user" => Some(Role::User),
            "assistant" => Some(Role::Assistant),
            _ => None,
        }
    }
}

/// Everything a conversation is configured with.
#[derive(Debug, Clone)]
pub struct SessionSettings {
    /// `"flux-general-en"` (turn-aware, recommended), `"nova-3"`, ...
    pub listen_model: String,
    /// Words to recognise reliably (the agent's name, the user's name).
    pub keyterms: Vec<String>,
    pub think_provider: String,
    pub think_model: String,
    pub prompt: String,
    /// Function definitions ([`crate::tools::definitions`]).
    pub functions: Vec<Value>,
    pub voice: String,
    pub speed: f32,
}

/// Whether a listen model is a Flux model (API version 2).
pub fn is_flux(model: &str) -> bool {
    model.starts_with("flux")
}

/// The `Settings` message that opens a conversation.
pub fn settings(s: &SessionSettings) -> Value {
    let mut listen = json!({
        "type": "deepgram",
        "version": if is_flux(&s.listen_model) { "v2" } else { "v1" },
        "model": s.listen_model.as_str(),
    });
    if !is_flux(&s.listen_model) {
        listen["language"] = json!("en");
        listen["smart_format"] = json!(true);
    }
    if !s.keyterms.is_empty() {
        listen["keyterms"] = json!(s.keyterms);
    }
    let mut speak = json!({ "type": "deepgram", "model": s.voice.as_str() });
    if (s.speed - 1.0).abs() > 0.01 {
        speak["speed"] = speed(s.speed);
    }
    // Each conversation starts afresh: no earlier turns, no greeting (how
    // the last one ended is in the prompt, for reference).
    let agent = json!({
        "listen": { "provider": listen },
        "think": {
            "provider": {
                "type": s.think_provider.as_str(),
                "model": s.think_model.as_str(),
                "temperature": 0.7,
            },
            "prompt": s.prompt.as_str(),
            "functions": s.functions,
        },
        "speak": { "provider": speak },
    });
    json!({
        "type": "Settings",
        "tags": ["veda"],
        // Do not let Deepgram keep the user's conversations to improve its
        // models.
        "mip_opt_out": true,
        "flags": { "history": true },
        "audio": {
            "input": { "encoding": "linear16", "sample_rate": INPUT_RATE },
            "output": { "encoding": "linear16", "sample_rate": OUTPUT_RATE, "container": "none" },
        },
        "agent": agent,
    })
}

/// The result of a function the agent asked for.
pub fn function_response(id: &str, name: &str, content: &str) -> Value {
    json!({ "type": "FunctionCallResponse", "id": id, "name": name, "content": content })
}

/// Text the agent should treat as the user's turn (Veda uses it for
/// system notices such as an approval decision or a fired reminder).
pub fn inject_user_message(content: &str) -> Value {
    json!({ "type": "InjectUserMessage", "content": content })
}

/// Replaces the conversation's instructions.
pub fn update_prompt(prompt: &str) -> Value {
    json!({ "type": "UpdatePrompt", "prompt": prompt })
}

/// Changes the voice and its speaking rate.
pub fn update_speak(voice: &str, rate: f32) -> Value {
    let mut provider = json!({ "type": "deepgram", "model": voice });
    if (rate - 1.0).abs() > 0.01 {
        provider["speed"] = speed(rate);
    }
    json!({ "type": "UpdateSpeak", "speak": { "provider": provider } })
}

/// A speaking rate (0.7 to 1.5) as Deepgram is sent it: the decimal it was
/// set with (1.2), not the `f64` an `f32` widens to (1.2000000476837158).
fn speed(rate: f32) -> Value {
    let rate = rate.clamp(0.7, 1.5);
    alloc::format!("{rate}").parse::<f64>().map_or(Value::Null, Value::from)
}

/// Changes the language model.
pub fn update_think(provider: &str, model: &str) -> Value {
    json!({ "type": "UpdateThink", "think": { "provider": { "type": provider, "model": model } } })
}

/// Keeps a conversation open while no audio goes out.
pub fn keep_alive() -> Value {
    json!({ "type": "KeepAlive" })
}

/// A function the language model wants the agent to run.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCall {
    pub id: String,
    pub name: String,
    /// The arguments as given (a JSON object as text).
    pub arguments: String,
    /// Run by the client (always, for functions without an endpoint).
    pub client_side: bool,
}

impl FunctionCall {
    /// The arguments parsed (an empty object if they are not a JSON object).
    pub fn args(&self) -> Value {
        match serde_json::from_str::<Value>(&self.arguments) {
            Ok(v @ Value::Object(_)) => v,
            _ => json!({}),
        }
    }
}

/// A message from the Voice Agent API.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Welcome {
        request_id: String,
    },
    SettingsApplied,
    /// Something said, by the user (recognised) or the agent (generated).
    ConversationText {
        role: Role,
        content: String,
    },
    /// The user started talking: stop playing the agent's voice at once.
    UserStartedSpeaking,
    AgentThinking {
        content: String,
    },
    FunctionCallRequest {
        calls: Vec<FunctionCall>,
    },
    /// The user spoke again: these calls are no longer wanted.
    FunctionCallCancelled {
        ids: Vec<String>,
    },
    /// `total_latency` in seconds, when given. Deepgram no longer sends
    /// this (the agent's voice itself starts its turn); simulators may.
    AgentStartedSpeaking {
        latency: Option<f64>,
    },
    /// All audio of the agent's turn has been sent.
    AgentAudioDone,
    /// After each turn: from the end of the user's speech to the agent's
    /// voice (`total_latency`), and the language model's share
    /// (`ttt_text_latency`), in seconds.
    LatencyReport {
        total: Option<f64>,
        think: Option<f64>,
    },
    /// A turn of the conversation, for keeping its history.
    History {
        role: Role,
        content: String,
    },
    /// Setting changes took effect (`UpdatePrompt`, `UpdateSpeak`, ...).
    Updated {
        what: String,
    },
    InjectionRefused {
        message: String,
    },
    Error {
        code: String,
        description: String,
    },
    Warning {
        code: String,
        description: String,
    },
    /// Anything else (future additions).
    Other {
        kind: String,
    },
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

/// A duration in seconds (a number, or a number in a string).
fn seconds(v: &Value, key: &str) -> Option<f64> {
    match v.get(key) {
        Some(Value::String(s)) => s.parse().ok(),
        Some(n) => n.as_f64(),
        None => None,
    }
}

/// Parses a JSON message from the Voice Agent API.
pub fn parse_server_message(json: &str) -> Result<ServerMessage, String> {
    let v = serde_json::from_str::<Value>(json).map_err(|e| e.to_string())?;
    let kind = v["type"].as_str().ok_or("message without a type")?;
    Ok(match kind {
        "Welcome" => ServerMessage::Welcome { request_id: text(&v, "request_id") },
        "SettingsApplied" => ServerMessage::SettingsApplied,
        "ConversationText" => ServerMessage::ConversationText {
            role: v["role"].as_str().and_then(Role::parse).ok_or("bad role")?,
            content: text(&v, "content"),
        },
        "UserStartedSpeaking" => ServerMessage::UserStartedSpeaking,
        "AgentThinking" => ServerMessage::AgentThinking { content: text(&v, "content") },
        "FunctionCallRequest" => {
            let calls = v["functions"]
                .as_array()
                .map(|fs| {
                    fs.iter()
                        .map(|f| FunctionCall {
                            id: text(f, "id"),
                            name: text(f, "name"),
                            arguments: match f.get("arguments") {
                                Some(Value::String(s)) => s.clone(),
                                Some(other) => other.to_string(),
                                None => "{}".into(),
                            },
                            client_side: f.get("client_side").and_then(Value::as_bool).unwrap_or(true),
                        })
                        .collect()
                })
                .unwrap_or_default();
            ServerMessage::FunctionCallRequest { calls }
        }
        "FunctionCallCancelled" => ServerMessage::FunctionCallCancelled {
            ids: v["functions"].as_array().map(|fs| fs.iter().map(|f| text(f, "id")).collect()).unwrap_or_default(),
        },
        "AgentStartedSpeaking" => ServerMessage::AgentStartedSpeaking { latency: seconds(&v, "total_latency") },
        "AgentAudioDone" => ServerMessage::AgentAudioDone,
        "LatencyReport" => {
            ServerMessage::LatencyReport { total: seconds(&v, "total_latency"), think: seconds(&v, "ttt_text_latency") }
        }
        "History" => match v["role"].as_str().and_then(Role::parse) {
            Some(role) => ServerMessage::History { role, content: text(&v, "content") },
            None => ServerMessage::Other { kind: "History".into() },
        },
        "PromptUpdated" | "SpeakUpdated" | "ThinkUpdated" | "ListenUpdated" => {
            ServerMessage::Updated { what: kind.to_string() }
        }
        "InjectionRefused" => ServerMessage::InjectionRefused { message: text(&v, "message") },
        "Error" => ServerMessage::Error { code: text(&v, "code"), description: text(&v, "description") },
        "Warning" => ServerMessage::Warning { code: text(&v, "code"), description: text(&v, "description") },
        other => ServerMessage::Other { kind: other.to_string() },
    })
}

/// The streaming speech-to-text URL used to hear the agent's name.
pub fn listen_url(keyterms: &[&str]) -> String {
    let mut url = format!(
        "wss://api.deepgram.com/v1/listen?model=nova-3&encoding=linear16&sample_rate={INPUT_RATE}&channels=1\
         &interim_results=true&endpointing=300&punctuate=true&smart_format=false&mip_opt_out=true"
    );
    for k in keyterms {
        url.push_str("&keyterm=");
        url.push_str(&encode(k));
    }
    url
}

/// A transcript from streaming speech-to-text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    pub text: String,
    /// This stretch of audio will not be transcribed again.
    pub is_final: bool,
    /// The speaker paused: the utterance is complete.
    pub speech_final: bool,
}

/// Parses a `Results` message from streaming speech-to-text.
pub fn parse_listen_result(json: &str) -> Option<Heard> {
    let v = serde_json::from_str::<Value>(json).ok()?;
    if v["type"].as_str() != Some("Results") {
        return None;
    }
    let text = v.pointer("/channel/alternatives/0/transcript")?.as_str()?.to_string();
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    Some(Heard { text, is_final: flag("is_final"), speech_final: flag("speech_final") })
}

/// The text-to-speech URL for voice previews (24 kHz linear16).
pub fn speak_url(voice: &str) -> String {
    format!(
        "https://api.deepgram.com/v1/speak?model={}&encoding=linear16&sample_rate={OUTPUT_RATE}&container=none",
        encode(voice)
    )
}

/// The body of a text-to-speech request.
pub fn speak_body(text: &str) -> String {
    json!({ "text": text }).to_string()
}

/// What a URL's query component keeps as it is: RFC 3986's unreserved
/// characters.
const UNRESERVED: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// Percent-encodes `s` for a query component.
fn encode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, UNRESERVED).to_string()
}

/// A language model the agent can think with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThinkModel {
    pub id: String,
    pub name: String,
    pub provider: String,
    /// Deepgram's price tier: `"Standard"`, `"Advanced"` or `""` (unknown).
    pub tier: &'static str,
}

/// Models in Deepgram's lower ("Standard") price tier.
const STANDARD: &[&str] = &[
    "gpt-4o-mini",
    "gpt-4.1-mini",
    "gpt-4.1-nano",
    "gpt-5-mini",
    "gpt-5-nano",
    "gpt-5.4-mini",
    "gpt-5.4-nano",
    "gpt-5.6-luna",
    "claude-haiku-4-5",
    "claude-3-5-haiku-latest",
    "gemini-2.0-flash-lite",
    "gemini-2.5-flash",
    "gemini-2.5-flash-lite",
    "gemini-3-flash-preview",
    "gemini-3.1-flash-lite",
    "gemini-3.5-flash",
    "gemini-3.8-flash",
];

/// Models in Deepgram's higher ("Advanced") price tier.
const ADVANCED: &[&str] = &[
    "gpt-4o",
    "gpt-4.1",
    "gpt-5",
    "gpt-5.1",
    "gpt-5.2",
    "gpt-5.4",
    "gpt-5.5",
    "gpt-5.6-terra",
    "claude-sonnet-4-5",
    "claude-sonnet-4-6",
    "claude-sonnet-5",
    "gemini-3-pro-preview",
    "gemini-3.1-pro-preview",
];

/// Deepgram's price tier of a language model.
pub fn tier(model: &str) -> &'static str {
    if STANDARD.contains(&model) {
        "Standard"
    } else if ADVANCED.contains(&model) || model.contains("sonnet") || model.contains("-pro") {
        "Advanced"
    } else {
        ""
    }
}

/// Providers Deepgram runs itself (others need the user's own account).
pub fn managed_provider(provider: &str) -> bool {
    matches!(provider, "open_ai" | "anthropic" | "google" | "nvidia")
}

/// Parses the think-model catalog, keeping models Deepgram runs itself.
pub fn parse_think_models(json: &str) -> Vec<ThinkModel> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    v["models"]
        .as_array()
        .map(|ms| {
            ms.iter()
                .filter_map(|m| {
                    let id = m["id"].as_str()?.to_string();
                    let provider = m["provider"].as_str()?.to_string();
                    managed_provider(&provider).then(|| ThinkModel {
                        name: m["name"].as_str().unwrap_or(&id).to_string(),
                        tier: tier(&id),
                        id,
                        provider,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A voice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    /// `"aura-2-helena-en"`.
    pub id: String,
    /// `"Helena"`.
    pub name: String,
    pub accent: String,
    /// `"feminine, caring, natural"`.
    pub description: String,
}

/// Parses the model catalog: the English Aura-2 voices.
pub fn parse_voices(json: &str) -> Vec<Voice> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    let mut voices: Vec<Voice> = v["tts"]
        .as_array()
        .map(|ms| {
            ms.iter()
                .filter(|m| m["architecture"].as_str() == Some("aura-2"))
                .filter(|m| {
                    m["languages"]
                        .as_array()
                        .is_some_and(|l| l.iter().any(|x| matches!(x.as_str(), Some("en" | "en-US"))))
                })
                .filter_map(|m| {
                    let id = m["canonical_name"].as_str()?.to_string();
                    let raw = m["name"].as_str().unwrap_or(&id);
                    let mut name = String::new();
                    let mut chars = raw.chars();
                    if let Some(c) = chars.next() {
                        name.extend(c.to_uppercase());
                        name.push_str(chars.as_str());
                    }
                    let tags: Vec<&str> = m["metadata"]["tags"]
                        .as_array()
                        .map(|t| t.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    Some(Voice {
                        id,
                        name,
                        accent: m["metadata"]["accent"].as_str().unwrap_or("").to_string(),
                        description: tags.join(", "),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    voices.sort_by(|a, b| a.name.cmp(&b.name));
    voices.dedup_by(|a, b| a.id == b.id);
    voices
}

/// Speech-to-text models for conversations: (id, name, description).
pub const LISTEN_MODELS: &[(&str, &str, &str)] = &[
    ("flux-general-en", "Flux", "English, knows when you have finished speaking (recommended)"),
    ("flux-general-multi", "Flux multilingual", "Several languages, with turn detection"),
    ("nova-3", "Nova-3", "English, Deepgram's general recognition model"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn sample() -> SessionSettings {
        SessionSettings {
            listen_model: "flux-general-en".into(),
            keyterms: vec!["Veda".into()],
            think_provider: "open_ai".into(),
            think_model: "gpt-4.1-mini".into(),
            prompt: "You are Veda.".into(),
            functions: vec![json!({ "name": "open_app", "description": "Opens an app", "parameters": {} })],
            voice: "aura-2-helena-en".into(),
            speed: 1.0,
        }
    }

    #[test]
    fn builds_the_settings_message() {
        let v = settings(&sample());
        assert_eq!(v["type"].as_str(), Some("Settings"));
        assert_eq!(v["mip_opt_out"].as_bool(), Some(true));
        assert_eq!(v.pointer("/audio/input/sample_rate").and_then(Value::as_u64), Some(16_000));
        assert_eq!(v.pointer("/audio/output/sample_rate").and_then(Value::as_u64), Some(24_000));
        assert_eq!(v.pointer("/agent/listen/provider/version").and_then(Value::as_str), Some("v2"));
        assert_eq!(v.pointer("/agent/listen/provider/keyterms/0").and_then(Value::as_str), Some("Veda"));
        assert!(v.pointer("/agent/listen/provider/language").is_none());
        assert_eq!(v.pointer("/agent/think/provider/model").and_then(Value::as_str), Some("gpt-4.1-mini"));
        assert_eq!(v.pointer("/agent/think/functions/0/name").and_then(Value::as_str), Some("open_app"));
        assert_eq!(v.pointer("/agent/speak/provider/model").and_then(Value::as_str), Some("aura-2-helena-en"));
        assert!(v.pointer("/agent/speak/provider/speed").is_none());
        // Each conversation starts afresh.
        assert!(v.pointer("/agent/context").is_none() && v.pointer("/agent/greeting").is_none());

        let mut s = sample();
        s.listen_model = "nova-3".into();
        s.speed = 1.2;
        let v = settings(&s);
        assert_eq!(v.pointer("/agent/listen/provider/version").and_then(Value::as_str), Some("v1"));
        assert_eq!(v.pointer("/agent/listen/provider/language").and_then(Value::as_str), Some("en"));
        assert!((v.pointer("/agent/speak/provider/speed").and_then(Value::as_f64).unwrap() - 1.2).abs() < 1e-6);
        // As it was set, not as an `f32` widens.
        assert!(v.to_string().contains(r#""speed":1.2,"#) || v.to_string().contains(r#""speed":1.2}"#));
        assert!(update_speak("aura-2-thalia-en", 0.85).to_string().contains(r#""speed":0.85"#));
    }

    #[test]
    fn the_key_goes_only_to_deepgram() {
        assert!(is_deepgram(AGENT_URL) && is_deepgram(&listen_url(&["Veda"])) && is_deepgram(PROJECTS_URL));
        assert!(is_deepgram("wss://agent.deepgram.com:443/v1/agent/converse"));
        for url in [
            "ws://agent.deepgram.com/v1/agent/converse",
            "ws://10.0.2.2:5000/",
            "wss://10.0.2.2:5000/",
            "wss://agent.deepgram.com.example.org/",
            "wss://deepgram.com@example.org/",
            "wss://example.org/?host=agent.deepgram.com",
            "wss://notdeepgram.com/",
            "",
        ] {
            assert!(!is_deepgram(url), "{url}");
        }
    }

    #[test]
    fn parses_server_messages() {
        assert_eq!(
            parse_server_message(r#"{"type":"Welcome","request_id":"r1"}"#).unwrap(),
            ServerMessage::Welcome { request_id: "r1".into() }
        );
        assert_eq!(
            parse_server_message(r#"{"type":"ConversationText","role":"user","content":"Open my notes"}"#).unwrap(),
            ServerMessage::ConversationText { role: Role::User, content: "Open my notes".into() }
        );
        let call = parse_server_message(
            r#"{"type":"FunctionCallRequest","functions":[{"id":"f1","name":"open_app","arguments":"{\"app\":\"editor\"}","client_side":true}]}"#,
        )
        .unwrap();
        let ServerMessage::FunctionCallRequest { calls } = call else { panic!() };
        assert_eq!(calls[0].name, "open_app");
        assert_eq!(calls[0].args()["app"].as_str(), Some("editor"));
        assert_eq!(
            parse_server_message(r#"{"type":"AgentStartedSpeaking","total_latency":"0.8","tts_latency":"0.1"}"#)
                .unwrap(),
            ServerMessage::AgentStartedSpeaking { latency: Some(0.8) }
        );
        assert_eq!(
            parse_server_message(r#"{"type":"FunctionCallCancelled","functions":[{"id":"f1","name":"x"}]}"#).unwrap(),
            ServerMessage::FunctionCallCancelled { ids: vec!["f1".into()] }
        );
        assert_eq!(
            parse_server_message(r#"{"type":"Error","description":"Bad key","code":"INVALID_AUTH"}"#).unwrap(),
            ServerMessage::Error { code: "INVALID_AUTH".into(), description: "Bad key".into() }
        );
        assert_eq!(
            parse_server_message(
                r#"{"type":"LatencyReport","stt_latency":0.12,"ttt_text_latency":0.36,"tts_latency":0.18,"total_latency":0.64}"#
            )
            .unwrap(),
            ServerMessage::LatencyReport { total: Some(0.64), think: Some(0.36) }
        );
        assert_eq!(
            parse_server_message(r#"{"type":"LatencyReport"}"#).unwrap(),
            ServerMessage::LatencyReport { total: None, think: None }
        );
        assert_eq!(
            parse_server_message(r#"{"type":"SomethingNew","x":1}"#).unwrap(),
            ServerMessage::Other { kind: "SomethingNew".into() }
        );
        assert!(parse_server_message("not json").is_err());
        assert!(parse_server_message(r#"{"no":"type"}"#).is_err());
        // Arguments given as an object rather than a string still work.
        let ServerMessage::FunctionCallRequest { calls } = parse_server_message(
            r#"{"type":"FunctionCallRequest","functions":[{"id":"a","name":"n","arguments":{"x":1}}]}"#,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(calls[0].args()["x"].as_i64(), Some(1));
    }

    #[test]
    fn parses_catalogs_and_transcripts() {
        let think = r#"{"models":[{"id":"gpt-4.1-mini","name":"GPT-4.1 mini","provider":"open_ai"},{"id":"gpt-4o","name":"GPT-4o","provider":"open_ai"},{"id":"openai/gpt-oss-20b","name":"x","provider":"groq"}]}"#;
        let models = parse_think_models(think);
        assert_eq!(models.len(), 2);
        assert_eq!((models[0].tier, models[1].tier), ("Standard", "Advanced"));
        let tts = r#"{"stt":[],"tts":[
            {"name":"helena","canonical_name":"aura-2-helena-en","architecture":"aura-2","languages":["en","en-US"],"metadata":{"accent":"American","tags":["feminine","caring"]}},
            {"name":"carina","canonical_name":"aura-2-carina-es","architecture":"aura-2","languages":["es"],"metadata":{}},
            {"name":"asteria","canonical_name":"aura-asteria-en","architecture":"aura","languages":["en"],"metadata":{}}]}"#;
        let voices = parse_voices(tts);
        assert_eq!(voices.len(), 1);
        assert_eq!((voices[0].name.as_str(), voices[0].description.as_str()), ("Helena", "feminine, caring"));
        assert_eq!(
            parse_listen_result(
                r#"{"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":"hey veda"}]}}"#
            ),
            Some(Heard { text: "hey veda".into(), is_final: true, speech_final: false })
        );
        assert_eq!(
            parse_listen_result(
                r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":""}]}}"#
            )
            .map(|h| (h.text.is_empty(), h.speech_final)),
            Some((true, true))
        );
        assert_eq!(parse_listen_result(r#"{"type":"Metadata","duration":1.5}"#), None);
        assert!(listen_url(&["Veda"]).ends_with("&keyterm=Veda"));
        assert_eq!(
            speak_url("aura-2-helena-en"),
            "https://api.deepgram.com/v1/speak?model=aura-2-helena-en&encoding=linear16&sample_rate=24000&container=none"
        );
    }
}
