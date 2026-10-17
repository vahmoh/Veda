// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The test microphone: audio that automation scripts make Veda hear.
//!
//! [`MicServer`] listens on a local TCP port; inside Veda, `testmic`
//! (started with the boot argument from [`MicServer::boot_arg`]) connects
//! to it through QEMU's NAT (the host is 10.0.2.2) and attaches to the audio
//! service as the microphone. The server streams 16 kHz mono 16-bit PCM in
//! real time: queued audio when there is some, silence otherwise.
//!
//! With `mic-echo GAIN DELAY`, the machine's own sound output (the WAV
//! file QEMU records) comes back into the microphone, delayed and quieter,
//! as loudspeakers next to a microphone would: the agent must not hear
//! itself. With `mic-music TITLE GAIN`, one of Veda's sample tracks (as
//! `musicgen` made it) plays into the microphone beside what is queued, as
//! music on another device in the room would: the machine plays nothing
//! of it, so its echo canceller knows nothing of it.
//!
//! Scripts queue speech with `say "..."`, synthesised by Deepgram's
//! text-to-speech REST API on the host ([`synthesize`], with the key from
//! `$DEEPGRAM_API_KEY`, which goes to curl through a pipe, never into a
//! file or onto a command line). Results are cached under
//! `target/veda/tts-cache`, so a phrase is only paid for once.

use std::collections::VecDeque;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audio::qoa::{self, QoaFile};
use audio::resample::Resampler;
use audio::wav::{self, SampleFormat, WavInfo};

use crate::util::{self, Result};
use crate::voice;

/// Sample rate of the test microphone.
pub const RATE: u32 = 16_000;
/// Samples sent per packet (20 ms).
const CHUNK: usize = (RATE / 50) as usize;
/// The voice that plays the user in tests (different from the agent's).
pub const USER_VOICE: &str = "aura-2-orion-en";

struct Shared {
    queue: Mutex<VecDeque<i16>>,
    connected: AtomicBool,
    stop: AtomicBool,
    echo: Mutex<Option<Echo>>,
    room: Mutex<Option<Room>>,
}

/// A sound from elsewhere in the room, heard once beside what is queued.
struct Room {
    sound: Vec<i16>,
    /// The next sample heard.
    at: usize,
}

impl Room {
    /// The next sample heard (silence once it has ended).
    fn next(&mut self) -> i16 {
        let Some(&s) = self.sound.get(self.at) else { return 0 };
        self.at += 1;
        s
    }
}

/// The machine's sound output, coming back into the microphone.
struct Echo {
    wav: PathBuf,
    gain: f64,
    /// Bytes of the file read so far.
    offset: u64,
    /// The recording's channels and the converter from its rate to the
    /// microphone's, once its header has been read.
    input: Option<(usize, Resampler)>,
    /// The output at 16 kHz, delayed: what the microphone hears next.
    delay_line: VecDeque<i16>,
}

impl Echo {
    /// Reads what QEMU has written since last time.
    fn refill(&mut self) {
        let Ok(mut f) = std::fs::File::open(&self.wav) else { return };
        use std::io::{Read, Seek, SeekFrom};
        if self.input.is_none() {
            // QEMU's header: 16-bit PCM, the data's length 0 while it
            // records (to the end of the file).
            let mut header = Vec::new();
            if Read::by_ref(&mut f).take(4096).read_to_end(&mut header).is_err() {
                return;
            }
            let len = f.metadata().map_or(0, |m| m.len());
            let Ok(info) = WavInfo::parse_prefix(&header, len) else { return };
            if info.format != SampleFormat::S16 {
                return;
            }
            self.offset = info.data_offset as u64;
            self.input = Some((info.channels as usize, Resampler::new(info.rate, RATE, 1)));
        }
        let Some((channels, resampler)) = self.input.as_mut() else { return };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if f.read_to_end(&mut bytes).is_err() {
            return;
        }
        let frame = 2 * *channels;
        let whole = bytes.len() / frame * frame;
        self.offset += whole as u64;
        // One microphone hears all the loudspeakers.
        let mono: Vec<i16> = bytes[..whole]
            .chunks_exact(frame)
            .map(|fr| {
                let sum: i32 = fr.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as i32).sum();
                (sum / *channels as i32) as i16
            })
            .collect();
        resampler.push(&mono);
        let mut out = [0i16; 1024];
        loop {
            let n = resampler.pull(&mut out);
            if n == 0 {
                break;
            }
            let gain = self.gain;
            self.delay_line.extend(out[..n].iter().map(|&s| (s as f64 * gain).clamp(-32768.0, 32767.0) as i16));
        }
    }
}

/// The host side of the test microphone.
pub struct MicServer {
    port: u16,
    shared: Arc<Shared>,
}

impl MicServer {
    /// Listens on a free local port and starts streaming.
    pub fn start() -> Result<MicServer> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("test microphone: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            connected: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            echo: Mutex::new(None),
            room: Mutex::new(None),
        });
        let s = shared.clone();
        std::thread::Builder::new()
            .name("testmic".into())
            .spawn(move || serve(listener, s))
            .map_err(|e| e.to_string())?;
        Ok(MicServer { port, shared })
    }

    /// The kernel command line argument that starts `testmic` in Veda.
    pub fn boot_arg(&self) -> String {
        format!("run=testmic:10.0.2.2,{},{}", self.port, RATE)
    }

    /// Queues samples to be "heard".
    pub fn enqueue(&self, samples: &[i16]) {
        self.shared.queue.lock().unwrap().extend(samples.iter().copied());
    }

    /// From now on the machine's sound output (QEMU's WAV recording)
    /// comes back into the microphone `delay_ms` later and `gain_db`
    /// louder (negative: quieter).
    pub fn echo(&self, wav: PathBuf, gain_db: f64, delay_ms: f64) {
        let delay = (delay_ms / 1000.0 * RATE as f64) as usize;
        let mut echo = Echo {
            wav,
            gain: 10f64.powf(gain_db / 20.0),
            offset: 0,
            input: None,
            delay_line: VecDeque::from(vec![0i16; delay]),
        };
        // Only what plays from now on comes back.
        echo.refill();
        echo.delay_line = VecDeque::from(vec![0i16; delay]);
        *self.shared.echo.lock().unwrap() = Some(echo);
    }

    /// From now on `sound` (16 kHz) plays into the microphone once,
    /// `gain_db` louder (negative: quieter), beside what is queued: a sound
    /// from elsewhere in the room, such as music on another device. It
    /// replaces the one playing.
    pub fn room_sound(&self, sound: &[i16], gain_db: f64) {
        let gain = 10f64.powf(gain_db / 20.0);
        let sound = sound.iter().map(|&s| (s as f64 * gain).clamp(-32768.0, 32767.0) as i16).collect();
        *self.shared.room.lock().unwrap() = Some(Room { sound, at: 0 });
    }

    /// Stops the sound from elsewhere in the room.
    pub fn room_quiet(&self) {
        *self.shared.room.lock().unwrap() = None;
    }

    /// Queues `secs` of the user's voice-like buzz (see `voice`): enough
    /// for a voice detector, though no recogniser would make words of it.
    pub fn tone(&self, secs: f64, peak: f64) {
        self.enqueue(&voice::buzz(voice::USER, RATE, secs, peak));
    }

    pub fn silence(&self, secs: f64) {
        let n = (secs * RATE as f64) as usize;
        self.shared.queue.lock().unwrap().extend(std::iter::repeat_n(0i16, n));
    }

    /// Samples still to be sent.
    pub fn queued(&self) -> usize {
        self.shared.queue.lock().unwrap().len()
    }

    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::SeqCst)
    }

    /// Waits until everything queued has been sent.
    pub fn wait_drained(&self, timeout: Duration) -> Result {
        let start = Instant::now();
        while self.queued() > 0 {
            if start.elapsed() > timeout {
                return Err(if self.connected() {
                    "the test microphone did not finish playing in time".into()
                } else {
                    "testmic never connected (is it in the image and on the command line?)".into()
                });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
}

impl Drop for MicServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// Accepts the guest's connection (again after it drops) and streams.
fn serve(listener: TcpListener, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        let Ok((stream, _)) = listener.accept() else { continue };
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        shared.connected.store(true, Ordering::SeqCst);
        stream_to(stream, &shared);
        shared.connected.store(false, Ordering::SeqCst);
    }
}

/// Sends a 20 ms packet every 20 ms until the connection fails.
fn stream_to(mut stream: TcpStream, shared: &Shared) {
    let _ = stream.set_nodelay(true);
    let period = Duration::from_millis(20);
    let mut next = Instant::now();
    let mut packet = Vec::with_capacity(CHUNK * 2);
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        packet.clear();
        {
            let mut echo = shared.echo.lock().unwrap();
            if let Some(e) = echo.as_mut() {
                e.refill();
            }
            let mut q = shared.queue.lock().unwrap();
            let mut room = shared.room.lock().unwrap();
            for _ in 0..CHUNK {
                let back = echo.as_mut().and_then(|e| e.delay_line.pop_front()).unwrap_or(0);
                let beside = room.as_mut().map_or(0, Room::next);
                let s = q.pop_front().unwrap_or(0).saturating_add(back).saturating_add(beside);
                packet.extend_from_slice(&s.to_le_bytes());
            }
        }
        if stream.write_all(&packet).is_err() {
            return;
        }
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > Duration::from_millis(200) {
            // Fell far behind (the host was busy): do not burst.
            next = now;
        }
    }
}

/// Speech for `text` as 16 kHz mono samples, from the cache or from
/// Deepgram's text-to-speech API (needs `$DEEPGRAM_API_KEY`).
pub fn synthesize(text: &str, voice: &str) -> Result<Vec<i16>> {
    let dir = util::cache_dir().join("tts-cache");
    util::create_dir_all(&dir)?;
    // The cache's names are FNV-1a hashes of what was said and how: kept
    // as they are, so that what has been paid for stays found.
    let file = dir.join(format!("{:016x}.pcm", fnv64(&format!("{voice}\0{RATE}\0{text}"))));
    if let Ok(bytes) = std::fs::read(&file)
        && !bytes.is_empty()
    {
        return Ok(pcm_from_bytes(&bytes));
    }
    let key = std::env::var("DEEPGRAM_API_KEY")
        .map_err(|_| "`say` needs a Deepgram API key in $DEEPGRAM_API_KEY (or a cached phrase)".to_string())?;
    let bytes = deepgram_speak(text, voice, &key)?;
    // Runs side by side may cache the same phrase: each writes its own file
    // and moves it in whole.
    util::write_atomically(&file, &bytes)?;
    Ok(pcm_from_bytes(&bytes))
}

fn pcm_from_bytes(bytes: &[u8]) -> Vec<i16> {
    bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect()
}

/// Deepgram's text-to-speech request for `text` in `voice`, as curl's
/// arguments: the key's header comes from its standard input (`@-`).
fn speak_request(text: &str, voice: &str) -> Vec<String> {
    let url =
        format!("https://api.deepgram.com/v1/speak?model={voice}&encoding=linear16&sample_rate={RATE}&container=none");
    let body = serde_json::json!({ "text": text }).to_string();
    [
        "-sS",
        "--fail-with-body",
        "--connect-timeout",
        "30",
        "--max-time",
        "120",
        "-X",
        "POST",
        &url,
        "-H",
        "@-",
        "-H",
        "Content-Type: application/json",
        "--data-raw",
        &body,
    ]
    .map(String::from)
    .to_vec()
}

/// Calls `POST /v1/speak` with curl, which reads the header with the key
/// from a pipe and writes the speech to its standard output.
fn deepgram_speak(text: &str, voice: &str, key: &str) -> Result<Vec<u8>> {
    let mut curl = Command::new("curl")
        .args(speak_request(text, voice))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running curl: {e}"))?;
    let header = format!("Authorization: Token {key}\n");
    let sent = curl.stdin.take().map(|mut stdin| stdin.write_all(header.as_bytes()));
    let output = curl.wait_with_output().map_err(|e| format!("running curl: {e}"))?;
    if let Some(Err(e)) = sent {
        return Err(format!("giving curl the key: {e}"));
    }
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let detail = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "Deepgram text-to-speech failed: {} {}",
            err.trim(),
            detail.chars().take(200).collect::<String>()
        ));
    }
    if output.stdout.len() < 2 {
        return Err("Deepgram text-to-speech returned no audio".into());
    }
    Ok(output.stdout)
}

/// Reads a WAV file (in any of the formats Veda plays) as 16 kHz mono
/// samples.
pub fn read_wav(path: &Path) -> Result<Vec<i16>> {
    let data = util::read(path)?;
    let info = WavInfo::parse(&data).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut stereo = vec![0i16; info.frames as usize * 2];
    let frames = wav::read_stereo(&info, &data, 0, &mut stereo);
    let mono: Vec<i16> =
        stereo[..frames * 2].as_chunks::<2>().0.iter().map(|[l, r]| ((*l as i32 + *r as i32) / 2) as i16).collect();
    Ok(Resampler::process_all(info.rate, RATE, 1, &mono))
}

/// A QOA file as 16 kHz mono samples (all its channels mixed).
pub fn read_qoa(path: &Path) -> Result<Vec<i16>> {
    let data = util::read(path)?;
    let file = QoaFile::open(&data).map_err(|e| format!("{}: {e}", path.display()))?;
    let info = file.info();
    let mut frame = vec![0i16; qoa::FRAME_LEN * info.channels];
    let mut mono = Vec::with_capacity(info.frames as usize);
    for i in 0..file.frame_count() {
        let n = file.decode_frame(i, &mut frame).map_err(|e| format!("{}: {e}", path.display()))?;
        mono.extend(frame[..n * info.channels].chunks_exact(info.channels).map(|c| {
            let sum: i32 = c.iter().map(|&s| s as i32).sum();
            (sum / info.channels as i32) as i16
        }));
    }
    Ok(Resampler::process_all(info.rate, RATE, 1, &mono))
}

/// One of Veda's sample tracks, by its title, as `musicgen` made it (16 kHz
/// mono).
pub fn read_sample_track(title: &str) -> Result<Vec<i16>> {
    let dir = util::generated_dir().join("musicgen").join("samples").join("Music");
    let path = dir.join(format!("{title}.qoa"));
    if !path.is_file() {
        return Err(format!("no sample track \"{title}\" in {} (musicgen makes them)", dir.display()));
    }
    read_qoa(&path)
}

/// The 64-bit FNV-1a hash of `s`.
fn fnv64(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key reaches curl through its standard input alone: the
    /// arguments hold no secret, and nothing is written to a file.
    #[test]
    fn the_key_stays_off_the_command_line() {
        let args = speak_request("hello \"there\"", USER_VOICE);
        assert!(args.windows(2).any(|w| w == ["-H", "@-"]), "{args:?}");
        assert!(args.windows(2).any(|w| w[0] == "--data-raw" && w[1] == r#"{"text":"hello \"there\""}"#));
        assert!(!args.iter().any(|a| a.contains("Token") || a.starts_with("-o")));
        assert!(args.windows(2).any(|w| w == ["--max-time", "120"]));
        // The cache keeps the names it had.
        assert_eq!(fnv64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv64("a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn streams_queued_audio_in_real_time() {
        let mic = MicServer::start().unwrap();
        let port: u16 = mic.boot_arg().split(',').nth(1).unwrap().parse().unwrap();
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        mic.enqueue(&[1000; 1600]);
        let start = Instant::now();
        let mut got = vec![0u8; 6400];
        std::io::Read::read_exact(&mut conn, &mut got).unwrap();
        // 200 ms of audio takes about 200 ms to arrive.
        assert!(start.elapsed() >= Duration::from_millis(150), "{:?}", start.elapsed());
        let samples = pcm_from_bytes(&got);
        assert!(samples.iter().filter(|&&s| s == 1000).count() >= 1500);
        mic.wait_drained(Duration::from_secs(2)).unwrap();
    }

    /// `secs` of a 1 kHz tone at `rate` Hz on every channel, as 16-bit
    /// samples.
    fn tone(rate: u32, channels: usize, secs: f64) -> Vec<i16> {
        let frames = (rate as f64 * secs) as usize;
        let at = |i: usize| (8000.0 * (std::f64::consts::TAU * 1000.0 * i as f64 / rate as f64).sin()) as i16;
        (0..frames).flat_map(|i| std::iter::repeat_n(at(i), channels)).collect()
    }

    /// The level (RMS) and the number of times the signal crosses zero.
    fn measure(s: &[i16]) -> (f64, usize) {
        let rms = (s.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
        (rms, s.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count())
    }

    #[test]
    fn wav_files_become_the_microphones_rate() {
        let path = std::env::temp_dir().join(format!("veda-mic-{}.wav", std::process::id()));
        std::fs::write(&path, wav::write(&tone(48_000, 2, 1.0), 2, 48_000, &Default::default())).unwrap();
        let read = read_wav(&path);
        let _ = std::fs::remove_file(&path);
        let samples = read.unwrap();
        assert_eq!(samples.len(), RATE as usize);
        // The tone, as loud (8000 / √2) and as high (2000 crossings a second)
        // as it was.
        let (rms, crossings) = measure(&samples[100..15_900]);
        assert!((rms - 5657.0).abs() < 60.0, "{rms}");
        assert!(crossings.abs_diff(1975) <= 4, "{crossings}");
        assert!(read_wav(Path::new("/nonexistent.wav")).is_err());
    }

    #[test]
    fn a_sound_in_the_room_plays_beside_what_is_queued() {
        let mic = MicServer::start().unwrap();
        let port: u16 = mic.boot_arg().split(',').nth(1).unwrap().parse().unwrap();
        // Queued before connecting, so that both start with the first packet.
        mic.enqueue(&[1000; 320]);
        mic.room_sound(&[400; 640], -6.0);
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut got = vec![0u8; 2 * 960];
        std::io::Read::read_exact(&mut conn, &mut got).unwrap();
        let samples = pcm_from_bytes(&got);
        // The queue and the room (6 dB quieter) at once, then the room alone
        // until it ends, then silence: it plays once.
        assert!(samples[..320].iter().all(|&s| s == 1000 + 200), "{:?}", &samples[..4]);
        assert!(samples[320..640].iter().all(|&s| s == 200), "{:?}", &samples[320..324]);
        assert!(samples[640..].iter().all(|&s| s == 0));
        mic.room_sound(&[300; 32_000], 0.0);
        mic.room_quiet();
        assert!(mic.shared.room.lock().unwrap().is_none());
    }

    #[test]
    fn qoa_files_become_the_microphones_rate() {
        let path = std::env::temp_dir().join(format!("veda-mic-{}.qoa", std::process::id()));
        std::fs::write(&path, qoa::encode(&tone(44_100, 2, 1.0), 2, 44_100).unwrap()).unwrap();
        let read = read_qoa(&path);
        let _ = std::fs::remove_file(&path);
        let samples = read.unwrap();
        assert!(samples.len().abs_diff(RATE as usize) <= 1, "{}", samples.len());
        let (rms, crossings) = measure(&samples[100..15_900]);
        assert!((rms - 5657.0).abs() < 120.0, "{rms}");
        assert!(crossings.abs_diff(1975) <= 4, "{crossings}");
        assert!(read_sample_track("No Such Track").is_err_and(|e| e.contains("No Such Track")));
    }

    #[test]
    fn the_echo_follows_a_growing_recording() {
        // QEMU's recording: 48 kHz stereo, the data's length 0 until it ends.
        let path = std::env::temp_dir().join(format!("veda-echo-{}.wav", std::process::id()));
        let mut file = wav::write(&tone(48_000, 2, 0.5), 2, 48_000, &Default::default());
        let data = file.windows(4).position(|w| w == b"data").unwrap();
        file[data + 4..data + 8].copy_from_slice(&0u32.to_le_bytes());
        std::fs::write(&path, &file).unwrap();
        let mut echo =
            Echo { wav: path.clone(), gain: 0.5, offset: 0, input: None, delay_line: VecDeque::from(vec![0; 160]) };
        echo.refill();
        let first = echo.delay_line.len();
        // Half a second, 6 dB quieter, after 10 ms of the delay's silence
        // (the converter holds back the frames its filter has yet to see).
        assert!((160 + 7900..=160 + 8000).contains(&first), "{first}");
        // More of the recording: the echo continues where it was.
        let more: Vec<u8> = tone(48_000, 2, 1.0)[48_000..].iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&more).unwrap();
        echo.refill();
        let _ = std::fs::remove_file(&path);
        let echoed: Vec<i16> = echo.delay_line.iter().copied().collect();
        assert!((160 + 15_900..=160 + 16_000).contains(&echoed.len()), "{}", echoed.len());
        assert!(echoed[..160].iter().all(|&s| s == 0));
        let (rms, crossings) = measure(&echoed[400..16_000]);
        assert!((rms - 2828.0).abs() < 40.0, "{rms}");
        assert!(crossings.abs_diff(1950) <= 4, "{crossings}");
    }
}
