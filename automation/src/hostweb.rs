// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The host's end of `nettest local` and of systest's network checks: a web
//! server on this computer that answers `GET /hello` with "hello from the
//! host", and a UDP echo, so that the scripts of the network cards check
//! DHCP, ping and TCP through QEMU's NAT without the Internet (`nettest
//! local` in a script), and the checks of the C library and of Rust's
//! standard library check TCP and UDP to another machine. Programs in Veda
//! reach them at their network's gateway, which QEMU's NAT makes this
//! computer.
//!
//! The browser's scripts have their pages here too, its media's among
//! them: a video (videogen's countdown, AV1 and FLAC in WebM), the same
//! through Media Source Extensions, a sound file, and Web Audio's;
//! their scripts say how they played. Pages that move (a box spinning, a
//! page scrolling itself, a box crossing the page on a click) measure and
//! check its frames. A page reads a file the user
//! chooses, as before an upload, another counts its visits in IndexedDB,
//! a file comes to be downloaded, and a PDF to be shown. Files are sent
//! whole or in the byte ranges asked for, as media players ask.

use std::borrow::Cow;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::util::{self, Result};

/// What the server answers, and what nettest looks for.
const HELLO: &str = "hello from the host\n";

/// The browser's scripts' page (`/page`): its script makes its last line,
/// and it links to `/second`.
const PAGE: &str = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Veda test page</title>\n\
<style>body { font-family: sans-serif; margin: 48px; } h1 { color: #0a7; }</style></head>\n\
<body><h1>Hello from the host</h1>\n<p><a id=\"next\" href=\"/second\">To the second page</a></p>\n\
<p id=\"script\">Scripts do not run.</p>\n\
<script>document.getElementById('script').textContent = 'Scripts run: ' + 6 * 7;</script>\n</body></html>\n";

/// The page `/page` links to.
const SECOND: &str = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Second page</title></head>\n\
<body><h1>The second page</h1><p>The link was followed.</p></body></html>\n";

/// What a page sees of the local time (`/zone`): the time zone, as
/// JavaScript names it, and its offset from UTC.
const ZONE: &str = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Time zone</title></head>\n\
<body><p id=\"zone\">No script ran.</p>\n\
<script>document.getElementById('zone').textContent = 'Zone ' + Intl.DateTimeFormat().resolvedOptions().timeZone + \
', ' + -new Date().getTimezoneOffset() + ' minutes east of UTC';</script>\n</body></html>\n";

/// What a session keeps (`/cookie`): the cookies the page was sent, then
/// one it sets, which the page sees when the same session loads it again.
const COOKIE: &str = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Cookies</title></head>\n\
<body><p id=\"cookies\">No script ran.</p>\n\
<script>document.getElementById('cookies').textContent = 'Cookies: ' + (document.cookie || 'none');\n\
document.cookie = 'seen=yes; max-age=3600; path=/';</script>\n</body></html>\n";

/// Text of the world's scripts and emoji (`/scripts`), for the eye: what
/// the system's fonts show of each.
const SCRIPTS: &str = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Scripts</title>\n\
<style>body { font-family: sans-serif; margin: 24px 40px; font-size: 19px; } p { margin: 5px 0; }</style></head>\n<body>\n\
<p>English: The quick brown fox jumps over the lazy dog.</p>\n\
<p lang=\"el\">Ελληνικά: Ζαφείρι δέξου πάγκαλο</p>\n<p lang=\"ru\">Русский: Съешь же ещё этих мягких булок</p>\n\
<p lang=\"ar\" dir=\"rtl\">العربية: نص حكيم له سر قاطع وذو شأن عظيم</p>\n<p lang=\"he\" dir=\"rtl\">עברית: דג סקרן שט בים מאוכזב</p>\n\
<p lang=\"hi\">हिन्दी: ऋषियों को सताने वाले दुष्ट राक्षसों के राजा</p>\n<p lang=\"bn\">বাংলা: আমার সোনার বাংলা</p>\n\
<p lang=\"ta\">தமிழ்: யாமறிந்த மொழிகளிலே தமிழ்மொழி</p>\n<p lang=\"th\">ไทย: เป็นมนุษย์สุดประเสริฐเลิศคุณค่า</p>\n\
<p lang=\"zh-CN\">中文：我能吞下玻璃而不伤身体</p>\n<p lang=\"ja\">日本語：いろはにほへと ちりぬるを</p>\n\
<p lang=\"ko\">한국어: 다람쥐 헌 쳇바퀴에 타고파</p>\n<p lang=\"am\">አማርኛ: ሰላም ለዓለም</p>\n\
<p>Emoji: 😀 🎉 🌍 ❤️ 👍🏽 🇯🇵</p>\n<p style=\"font-family: serif\">Serif: The quick brown fox, Ελληνικά, Русский</p>\n\
<p style=\"font-family: monospace\">Monospace: fn main() { println!(\"0O1lI\"); }</p>\n</body></html>\n";

/// What the browser's frames are measured with: a box spinning, a CSS
/// animation that the compositor runs (`/spin`)...
const SPIN: &str = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Spinning</title>\n\
<style>body { font-family: sans-serif; margin: 48px; } #box { width: 160px; height: 160px; margin: 80px; \
background: #0a7; animation: spin 2s linear infinite; } @keyframes spin { to { transform: rotate(360deg); } }</style>\n\
</head><body><h1>A box spinning</h1><div id=\"box\"></div></body></html>\n";

/// ... and a long page that scrolls itself, a little each frame, from its
/// foot back to its head (`/scroll`).
const SCROLL: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Scrolling</title>
<style>body { font-family: sans-serif; margin: 24px 40px; font-size: 18px; } p { margin: 8px 0; }</style>
</head><body><h1>A long page scrolling itself</h1><div id="lines"></div>
<script>
const lines = document.getElementById('lines');
for (let i = 1; i <= 400; i++) {
    const p = document.createElement('p');
    p.textContent = `Line ${i}: the quick brown fox jumps over the lazy dog.`;
    lines.appendChild(p);
}
function step() {
    if (window.scrollY + window.innerHeight >= document.body.scrollHeight)
        window.scrollTo(0, 0);
    else
        window.scrollBy(0, 4);
    requestAnimationFrame(step);
}
requestAnimationFrame(step);
</script>
</body></html>
"#;

/// What the browser's frames are checked with (`/motion`): a click starts
/// a box moving across the page and a line counting its steps, and both
/// stop where they always do, three seconds later.
const MOTION: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Motion</title>
<style>body { font-family: sans-serif; margin: 32px 40px; }
#box { position: absolute; left: 40px; top: 200px; width: 120px; height: 120px; background: #0a7; }
#box.moving { animation: across 3s linear forwards; }
@keyframes across { to { transform: translate(560px, 180px); } }</style>
</head><body><h1>Motion</h1>
<p id="steps">Click to start.</p>
<p><a href="/second">A link to point at</a></p>
<div id="box"></div>
<script>
const steps = document.getElementById('steps'), box = document.getElementById('box');
let started = false;
document.addEventListener('click', () => {
    if (started)
        return;
    started = true;
    box.classList.add('moving');
    let n = 0;
    const timer = setInterval(() => {
        n++;
        steps.textContent = n < 60 ? `Step ${n} of 60` : 'Stopped after 60 steps';
        if (n >= 60)
            clearInterval(timer);
    }, 50);
});
</script>
</body></html>
"#;

/// The browser's video page (`/video`): the countdown (`COUNTDOWN`), which
/// the first click or key plays, with its sound (WebKit lets pages play
/// sound only once the user has done something on them). Its script says
/// how it played: the picture's size, how far it got, how many pictures
/// were shown and dropped (the video sink's count), and the colour of the
/// second of the colour bars it starts with, drawn into a canvas (yellow);
/// before it plays, what it waits for.
const VIDEO: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Video</title>
<style>body { font-family: sans-serif; margin: 24px 40px; } video { width: 640px; height: 360px; background: #000; }</style>
</head><body><h1>Video</h1>
<video id="video" src="/media/countdown.webm" preload="auto"></video>
<p id="state">Click or press a key to play.</p>
<script>
const video = document.getElementById('video'), state = document.getElementById('state');
let bar = 'not seen', asked = false;
function look() {
    const canvas = document.createElement('canvas');
    canvas.width = 640;
    canvas.height = 360;
    const context = canvas.getContext('2d');
    context.drawImage(video, 0, 0, 640, 360);
    const [r, g, b] = context.getImageData(120, 180, 1, 1).data;
    bar = r > 150 && g > 150 && b < 60 ? 'yellow' : `rgb(${r}, ${g}, ${b})`;
}
function show(what) {
    const quality = video.getVideoPlaybackQuality();
    state.textContent = `${what}: ${video.videoWidth}x${video.videoHeight}, ${video.currentTime.toFixed(1)} s, `
        + `${quality.totalVideoFrames} pictures, ${quality.droppedVideoFrames} dropped, the second bar ${bar}`;
}
function play() {
    if (asked)
        return;
    asked = true;
    video.play().catch(error => { state.textContent = `Video not played: ${error}`; });
}
document.addEventListener('click', play);
document.addEventListener('keydown', play);
video.addEventListener('timeupdate', () => {
    if (bar === 'not seen' && video.currentTime > 0.2 && video.currentTime < 0.8)
        look();
    show('Playing');
});
video.addEventListener('ended', () => {
    const quality = video.getVideoPlaybackQuality();
    show(quality.totalVideoFrames >= 300 && quality.droppedVideoFrames <= 12 ? 'Played smoothly' : 'Played');
});
video.addEventListener('error', () => { state.textContent = `Video error ${video.error.code}: ${video.error.message}`; });
const waiting = setInterval(() => {
    if (video.currentTime > 0 || video.error)
        return clearInterval(waiting);
    state.textContent = `${asked ? 'Waiting' : 'Click or press a key to play'}: the page ${document.visibilityState}, `
        + `ready state ${video.readyState}, network state ${video.networkState}`;
}, 500);
</script>
</body></html>
"#;

/// A sound file in an `<audio>` element (`/audio`): the tone
/// (`/media/tone.wav`), played on the first click or key; its script says
/// how far it played.
const AUDIO: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Audio</title>
<style>body { font-family: sans-serif; margin: 24px 40px; }</style>
</head><body><h1>Audio</h1>
<audio id="audio" src="/media/tone.wav" preload="auto" controls></audio>
<p id="state">Click or press a key to play.</p>
<script>
const audio = document.getElementById('audio'), state = document.getElementById('state');
let asked = false;
function play() {
    if (asked)
        return;
    asked = true;
    audio.play().catch(error => { state.textContent = `Audio not played: ${error}`; });
}
document.addEventListener('click', play);
document.addEventListener('keydown', play);
audio.addEventListener('timeupdate', () => {
    state.textContent = `Audio playing: ${audio.currentTime.toFixed(1)} s of ${audio.duration.toFixed(1)} s`;
});
audio.addEventListener('ended', () => { state.textContent = `Audio played: ${audio.duration.toFixed(1)} s`; });
audio.addEventListener('error', () => { state.textContent = `Audio error ${audio.error.code}: ${audio.error.message}`; });
</script>
</body></html>
"#;

/// The countdown through Media Source Extensions (`/mse`), fetched and
/// appended a piece at a time, as players do, and played on the first
/// click or key.
const MSE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Media Source</title>
<style>body { font-family: sans-serif; margin: 24px 40px; } video { width: 640px; height: 360px; background: #000; }</style>
</head><body><h1>Media Source Extensions</h1>
<video id="video"></video>
<p id="state">The media source has not opened.</p>
<script>
const video = document.getElementById('video'), state = document.getElementById('state');
const type = 'video/webm; codecs="av01.0.04M.08, flac"';
const buffered = () => video.buffered.length
    ? `${video.buffered.start(0).toFixed(1)} to ${video.buffered.end(0).toFixed(1)} s` : 'nothing';
let asked = false;
function play() {
    if (asked)
        return;
    asked = true;
    video.play().catch(error => { state.textContent = `Media Source not played: ${error}`; });
}
if (!window.MediaSource || !MediaSource.isTypeSupported(type)) {
    state.textContent = `Media Source: ${type} is not supported`;
} else {
    const source = new MediaSource();
    video.src = URL.createObjectURL(source);
    source.addEventListener('sourceopen', async () => {
        const buffer = source.addSourceBuffer(type);
        const data = await (await fetch('/media/countdown.webm')).arrayBuffer();
        for (let at = 0; at < data.byteLength; at += 65536) {
            buffer.appendBuffer(data.slice(at, at + 65536));
            await new Promise(done => buffer.addEventListener('updateend', done, { once: true }));
        }
        source.endOfStream();
        state.textContent = `Media Source: buffered ${buffered()}; click or press a key to play.`;
    });
    document.addEventListener('click', play);
    document.addEventListener('keydown', play);
    video.addEventListener('timeupdate', () => {
        state.textContent = `Media Source playing: ${video.currentTime.toFixed(1)} s of ${buffered()}`;
    });
    video.addEventListener('ended', () => {
        state.textContent = `Media Source played: ${video.videoWidth}x${video.videoHeight}, buffered ${buffered()}, `
            + `${video.currentTime.toFixed(1)} s`;
    });
    video.addEventListener('error', () => {
        state.textContent = `Media Source error ${video.error.code}: ${video.error.message}`;
    });
}
</script>
</body></html>
"#;

/// Web Audio (`/webaudio`), on the first click or key: a tone the page
/// makes, 660 Hz for a second and a half, which its analyser hears, then
/// a sound file (`/media/tone.wav`) decoded and played; its script says
/// what came of each.
const WEB_AUDIO: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Web Audio</title>
<style>body { font-family: sans-serif; margin: 24px 40px; }</style>
</head><body><h1>Web Audio</h1>
<p id="state">Click or press a key to start.</p>
<script>
const state = document.getElementById('state');
let context = null, heard = 'nothing', decoded = 'nothing';
const show = () => {
    state.textContent = `Web Audio ${context.state} at ${context.sampleRate} Hz (the page ${document.visibilityState}): `
        + `the analyser hears ${heard}; decoded ${decoded}`;
};
function start() {
    if (context)
        return;
    context = new AudioContext();
    const oscillator = context.createOscillator(), gain = context.createGain(), analyser = context.createAnalyser();
    oscillator.frequency.value = 660;
    gain.gain.value = 0.25;
    analyser.fftSize = 2048;
    oscillator.connect(gain).connect(analyser).connect(context.destination);
    oscillator.start();
    oscillator.stop(context.currentTime + 1.5);
    const listen = setInterval(() => {
        if (context.currentTime < 0.3)
            return show();
        const bins = new Float32Array(analyser.frequencyBinCount);
        analyser.getFloatFrequencyData(bins);
        let peak = 0;
        for (let i = 1; i < bins.length; i++) {
            if (bins[i] > bins[peak])
                peak = i;
        }
        const hz = peak * context.sampleRate / analyser.fftSize;
        heard = Math.abs(hz - 660) < 30 ? 'the tone' : `${Math.round(hz)} Hz`;
        if (heard === 'the tone' || context.currentTime > 1.4)
            clearInterval(listen);
        show();
    }, 100);
    fetch('/media/tone.wav').then(response => response.arrayBuffer()).then(data => context.decodeAudioData(data))
        .then(sound => {
            decoded = `${sound.duration.toFixed(1)} s of ${sound.numberOfChannels} channels`;
            const source = context.createBufferSource();
            source.buffer = sound;
            source.connect(context.destination);
            source.start(context.currentTime + 2);
            show();
        }, error => {
            decoded = `nothing: ${error}`;
            show();
        });
}
document.addEventListener('click', start);
document.addEventListener('keydown', start);
</script>
</body></html>
"#;

/// A file the user chooses, read as a page reads one it is to upload
/// (`/upload`): its file input fills the page, so that a click anywhere
/// chooses, and its script says what it read: how many bytes (all of the
/// file's, or not), of which file, and the first two of them.
const UPLOAD: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Upload</title>
<style>body { font-family: sans-serif; margin: 24px 40px; }
input { position: fixed; inset: 0; width: 100%; height: 100%; opacity: 0; }</style></head>
<body><h1>A file to upload</h1><p id="state">No file chosen.</p><input id="file" type="file">
<script>
const state = document.getElementById('state');
document.getElementById('file').addEventListener('change', event => {
  const file = event.target.files[0];
  if (!file)
    return;
  const reader = new FileReader();
  reader.onload = () => {
    const bytes = new Uint8Array(reader.result);
    const first = Array.from(bytes.slice(0, 2), b => b.toString(16).padStart(2, '0')).join(' ');
    state.textContent = 'Read ' + (bytes.length === file.size ? 'all ' : 'only ') + bytes.length + ' bytes of '
      + file.name + ', the first ' + first;
  };
  reader.onerror = () => state.textContent = 'Could not read ' + file.name + ': ' + reader.error;
  reader.readAsArrayBuffer(file);
});
</script>
</body></html>
"#;

/// What a site keeps (`/storage`): its script counts the page's visits
/// in IndexedDB, which the network process keeps in the engine's data,
/// and says how many it stored, or why it could not.
const STORAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Storage</title></head>
<body><p id="state">No script ran.</p>
<script>
const state = document.getElementById('state');
const request = indexedDB.open('veda', 1);
request.onupgradeneeded = () => request.result.createObjectStore('counts');
request.onerror = () => state.textContent = 'IndexedDB did not open: ' + request.error;
request.onsuccess = () => {
  const db = request.result;
  const visit = db.transaction('counts', 'readwrite');
  const counts = visit.objectStore('counts');
  const get = counts.get('visits');
  get.onsuccess = () => counts.put((get.result || 0) + 1, 'visits');
  visit.oncomplete = () => db.transaction('counts').objectStore('counts').get('visits').onsuccess =
    event => state.textContent = 'Visits stored: ' + event.target.result;
  visit.onabort = () => state.textContent = 'The visit was not stored: ' + visit.error;
};
</script>
</body></html>
"#;

/// A file to download (`/files/veda-download.txt`): sent as bytes of no
/// type a page shows, which the browser saves in Downloads under its name.
const DOWNLOAD: &str = "Downloaded from the host.\n";

/// What the PDF (`/files/veda.pdf`) says.
const PDF_TEXT: &str = "A PDF from the host";

/// A PDF of one page (`/files/veda.pdf`), as WebKit shows it in its viewer
/// (PDF.js): `PDF_TEXT` in Helvetica, which it does not embed, above a
/// green square.
fn pdf() -> Vec<u8> {
    let content = format!("BT /F1 32 Tf 72 680 Td ({PDF_TEXT}) Tj ET\n0 0.67 0.47 rg 72 300 300 300 re f\n");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> \
         /Contents 5 0 R >>"
            .to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (n, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", n + 1).as_bytes());
    }
    // The cross-reference table: where each object starts, in lines of 20
    // bytes.
    let table = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    let trailer = format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{table}\n%%EOF\n", objects.len() + 1);
    pdf.extend_from_slice(trailer.as_bytes());
    pdf
}

/// The countdown that videogen films for Videos (AV1 and FLAC in
/// Matroska, 640x360, 12 s: colour bars, then a flash and a beep every
/// second), which the browser's media pages play as WebM; under the
/// generated media, which every image build makes.
const COUNTDOWN: &str = "videogen/samples/Videos/Countdown.mkv";

/// A second of A (440 Hz) at -12 dBFS, 48 kHz stereo, as a WAV file
/// (`/media/tone.wav`), its ends faded in and out over 20 ms.
fn tone() -> Vec<u8> {
    const RATE: u32 = 48_000;
    let samples: Vec<i16> = (0..RATE)
        .flat_map(|n| {
            let t = n as f64 / RATE as f64;
            let fade = (t / 0.02).min((1.0 - t) / 0.02).min(1.0);
            let s = (8192.0 * fade * (std::f64::consts::TAU * 440.0 * t).sin()) as i16;
            [s, s]
        })
        .collect();
    audio::wav::write(&samples, 2, RATE, &Default::default())
}

/// The bytes a request asks for (`Range: bytes=FIRST-LAST`, `FIRST-` or
/// `-SUFFIX`) of `len`: `None` for all of them (no range, or one this
/// server does not take, as several), `Some(None)` for a range past the
/// end, else the first and the last.
fn range(head: &[u8], len: usize) -> Option<Option<(usize, usize)>> {
    let head = std::str::from_utf8(head).ok()?;
    let value = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("range").then(|| value.trim())
    })?;
    let (first, last) = value.strip_prefix("bytes=")?.split_once('-')?;
    let (first, last) = match (first.trim(), last.trim()) {
        ("", suffix) => (len.saturating_sub(suffix.parse().ok()?), len.checked_sub(1)?),
        (first, "") => (first.parse().ok()?, len.saturating_sub(1)),
        (first, last) => (first.parse().ok()?, last.parse::<usize>().ok()?.min(len.saturating_sub(1))),
    };
    Some((first < len && first <= last).then_some((first, last)))
}

/// Where a guest on QEMU's NAT reaches this computer.
const GATEWAY: &str = "10.0.2.2";

/// The servers, which stop when this is dropped.
pub struct HostWeb {
    port: u16,
    /// The UDP echo's port.
    echo_port: u16,
    stop: Arc<AtomicBool>,
}

impl HostWeb {
    /// Listens on a free local port and answers, a thread for each
    /// connection; echoes datagrams on another.
    pub fn start() -> Result<HostWeb> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("the host's web server: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let echo = UdpSocket::bind("127.0.0.1:0").map_err(|e| format!("the host's UDP echo: {e}"))?;
        let echo_port = echo.local_addr().map_err(|e| e.to_string())?.port();
        echo.set_read_timeout(Some(Duration::from_millis(200))).map_err(|e| e.to_string())?;
        let stopping = stop.clone();
        std::thread::Builder::new()
            .name("hostecho".into())
            .spawn(move || {
                let mut buf = [0u8; 2048];
                while !stopping.load(Ordering::SeqCst) {
                    if let Ok((n, from)) = echo.recv_from(&mut buf) {
                        let _ = echo.send_to(&buf[..n], from);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let stopping = stop.clone();
        std::thread::Builder::new()
            .name("hostweb".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        return;
                    }
                    if let Ok(stream) = stream {
                        std::thread::spawn(move || answer(stream));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(HostWeb { port, echo_port, stop })
    }

    /// Where programs in Veda reach the web server: `host:port`.
    pub fn address(&self) -> String {
        format!("{GATEWAY}:{}", self.port)
    }

    /// The kernel command line's argument that has nettest check the
    /// network against the server.
    pub fn boot_arg(&self) -> String {
        format!("run=nettest:local,gateway,{}", self.port)
    }

    /// The kernel command line's argument that starts systest with the
    /// servers' addresses, for the checks of the C library and of Rust's
    /// standard library.
    pub fn systest_arg(&self) -> String {
        format!("run=systest:host,{GATEWAY},{},{}", self.port, self.echo_port)
    }
}

impl Drop for HostWeb {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accepting thread.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// Answers one request: `GET /hello`, the browser's pages (`/page`,
/// `/second`, `/zone`, `/cookie`, `/scripts`, `/video`, `/audio`, `/mse`,
/// `/webaudio`, `/upload`, `/storage`), their media
/// (`/media/countdown.webm`, `/media/tone.wav`), a file to download
/// (`/files/veda-download.txt`) and a PDF (`/files/veda.pdf`), or else
/// nothing found.
fn answer(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 8192 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let path = head.strip_prefix(b"GET ").and_then(|rest| rest.split(|&b| b == b' ').next()).unwrap_or_default();
    let html = "text/html; charset=utf-8";
    let found: Option<(&str, Cow<'static, [u8]>)> = match path {
        b"/hello" => Some(("text/plain", HELLO.as_bytes().into())),
        b"/page" => Some((html, PAGE.as_bytes().into())),
        b"/second" => Some((html, SECOND.as_bytes().into())),
        b"/zone" => Some((html, ZONE.as_bytes().into())),
        b"/cookie" => Some((html, COOKIE.as_bytes().into())),
        b"/scripts" => Some((html, SCRIPTS.as_bytes().into())),
        b"/spin" => Some((html, SPIN.as_bytes().into())),
        b"/scroll" => Some((html, SCROLL.as_bytes().into())),
        b"/motion" => Some((html, MOTION.as_bytes().into())),
        b"/video" => Some((html, VIDEO.as_bytes().into())),
        b"/audio" => Some((html, AUDIO.as_bytes().into())),
        b"/mse" => Some((html, MSE.as_bytes().into())),
        b"/webaudio" => Some((html, WEB_AUDIO.as_bytes().into())),
        b"/upload" => Some((html, UPLOAD.as_bytes().into())),
        b"/storage" => Some((html, STORAGE.as_bytes().into())),
        b"/files/veda-download.txt" => Some(("application/octet-stream", DOWNLOAD.as_bytes().into())),
        b"/files/veda.pdf" => Some(("application/pdf", pdf().into())),
        b"/media/countdown.webm" => {
            std::fs::read(util::generated_dir().join(COUNTDOWN)).ok().map(|video| ("video/webm", video.into()))
        }
        b"/media/tone.wav" => Some(("audio/wav", tone().into())),
        _ => None,
    };
    let Some((kind, body)) = found else {
        let _ = stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    };
    let (status, part, bytes) = match range(&head, body.len()) {
        None => ("200 OK", String::new(), &body[..]),
        Some(Some((first, last))) => (
            "206 Partial Content",
            format!("Content-Range: bytes {first}-{last}/{}\r\n", body.len()),
            &body[first..=last],
        ),
        Some(None) => ("416 Range Not Satisfiable", format!("Content-Range: bytes */{}\r\n", body.len()), &[][..]),
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n{part}\
         Connection: close\r\n\r\n",
        bytes.len()
    );
    let _ = stream.write_all(header.as_bytes()).and_then(|()| stream.write_all(bytes));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(port: u16, path: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: 10.0.2.2\r\nUser-Agent: Veda\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn datagrams_come_back() {
        let server = HostWeb::start().unwrap();
        let arg = server.systest_arg();
        assert!(arg.starts_with("run=systest:host,10.0.2.2,"), "{arg}");
        let port: u16 = arg.rsplit(',').next().unwrap().parse().unwrap();
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        socket.send_to(b"echo", ("127.0.0.1", port)).unwrap();
        let mut buf = [0u8; 16];
        let (n, from) = socket.recv_from(&mut buf).unwrap();
        assert_eq!((&buf[..n], from.port()), (&b"echo"[..], port));
    }

    #[test]
    fn nettest_finds_the_host_saying_hello() {
        let server = HostWeb::start().unwrap();
        let port: u16 = server.boot_arg().rsplit(',').next().unwrap().parse().unwrap();
        assert!(server.boot_arg().starts_with("run=nettest:local,gateway,"));
        let hello = get(port, "/hello");
        assert!(hello.starts_with("HTTP/1.1 200 OK\r\n"), "{hello}");
        assert!(hello.ends_with("\r\n\r\nhello from the host\n"), "{hello}");
        assert!(hello.contains(&format!("Content-Length: {}\r\n", HELLO.len())));
        assert!(get(port, "/").starts_with("HTTP/1.1 404 Not Found\r\n"));
    }

    #[test]
    fn the_browser_pages_are_html() {
        let server = HostWeb::start().unwrap();
        let port: u16 = server.boot_arg().rsplit(',').next().unwrap().parse().unwrap();
        for (path, text) in [
            ("/page", "Scripts do not run."),
            ("/second", "The link was followed."),
            ("/zone", "Zone "),
            ("/cookie", "seen=yes"),
            ("/scripts", "日本語"),
            ("/spin", "animation: spin"),
            ("/scroll", "window.scrollBy"),
            ("/motion", "Stopped after 60 steps"),
            ("/video", "src=\"/media/countdown.webm\""),
            ("/audio", "src=\"/media/tone.wav\""),
            ("/mse", "MediaSource.isTypeSupported"),
            ("/webaudio", "decodeAudioData"),
            ("/upload", "readAsArrayBuffer"),
            ("/storage", "indexedDB.open"),
        ] {
            let page = get(port, path);
            assert!(page.starts_with("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n"), "{page}");
            assert!(page.contains(text), "{path}: {page}");
        }
        let download = get(port, "/files/veda-download.txt");
        assert!(download.starts_with("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n"), "{download}");
        assert!(download.ends_with(DOWNLOAD), "{download}");
        let pdf = get(port, "/files/veda.pdf");
        assert!(pdf.starts_with("HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\n"), "{pdf}");
        assert!(pdf.contains("\r\n\r\n%PDF-1.4\n") && pdf.ends_with("%%EOF\n"), "{pdf}");
    }

    /// The PDF's cross-reference table, which a reader goes by, has where
    /// each of its objects starts, and the trailer where the table does.
    #[test]
    fn the_pdf_is_where_its_table_says() {
        let pdf = String::from_utf8(pdf()).unwrap();
        let (_, last) = pdf.trim_end_matches("\n%%EOF\n").rsplit_once("startxref\n").unwrap();
        let table: usize = last.parse().unwrap();
        assert!(pdf[table..].starts_with("xref\n0 6\n0000000000 65535 f \n"), "{pdf}");
        let entries = pdf[table..].lines().skip(3).take(5);
        for (n, entry) in entries.enumerate() {
            assert_eq!(entry.len(), 19, "{entry:?}: 20 bytes with its line's end");
            let offset: usize = entry[..10].parse().unwrap();
            assert!(pdf[offset..].starts_with(&format!("{} 0 obj\n", n + 1)), "object {}: {entry}", n + 1);
        }
        let content = pdf.split_once("stream\n").unwrap().1.split_once("endstream").unwrap().0;
        assert!(pdf.contains(&format!("<< /Length {} >>", content.len())));
        assert!(content.contains(&format!("({PDF_TEXT}) Tj")));
    }

    /// The response to `GET path` with a `Range` header of `range`, as
    /// bytes.
    fn get_range(port: u16, path: &str, range: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: 10.0.2.2\r\nrange: {range}\r\n\r\n").unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        response
    }

    #[test]
    fn media_comes_whole_or_in_the_ranges_asked_for() {
        let server = HostWeb::start().unwrap();
        let port: u16 = server.boot_arg().rsplit(',').next().unwrap().parse().unwrap();
        let wav = tone();
        let info = audio::wav::WavInfo::parse(&wav).unwrap();
        assert_eq!((info.rate, info.channels, info.duration_ms()), (48_000, 2, 1000));
        let whole = get_range(port, "/media/tone.wav", "bytes=0-");
        assert!(whole.starts_with(b"HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\n"));
        assert!(whole.ends_with(&wav), "the whole file, from its first byte");
        let part = get_range(port, "/media/tone.wav", "bytes=4-11");
        let text = String::from_utf8_lossy(&part);
        assert!(text.contains(&format!("Content-Range: bytes 4-11/{}\r\n", wav.len())), "{text}");
        assert!(part.ends_with(&[&b"\r\n\r\n"[..], &wav[4..12]].concat()), "{text}");
        let end = get_range(port, "/media/tone.wav", "bytes=-4");
        assert!(end.ends_with(&wav[wav.len() - 4..]));
        let past = get_range(port, "/media/tone.wav", &format!("bytes={}-", wav.len()));
        assert!(past.starts_with(b"HTTP/1.1 416 Range Not Satisfiable\r\n"));
        // The video is videogen's, there once an image has been built: its
        // first bytes, Matroska's.
        let video = get_range(port, "/media/countdown.webm", "bytes=0-3");
        if util::generated_dir().join(COUNTDOWN).is_file() {
            assert!(video.starts_with(b"HTTP/1.1 206 Partial Content\r\nContent-Type: video/webm\r\n"));
            assert!(video.ends_with(&[0x1a, 0x45, 0xdf, 0xa3]));
        } else {
            assert!(video.starts_with(b"HTTP/1.1 404 Not Found\r\n"));
        }
    }

    #[test]
    fn ranges_are_read_as_http_has_them() {
        let head = |range: &str| format!("GET /a HTTP/1.1\r\nHost: h\r\nRange: {range}\r\n\r\n").into_bytes();
        assert_eq!(range(&head("bytes=0-"), 100), Some(Some((0, 99))));
        assert_eq!(range(&head("bytes=10-19"), 100), Some(Some((10, 19))));
        assert_eq!(range(&head("bytes=90-200"), 100), Some(Some((90, 99))), "the end is the file's");
        assert_eq!(range(&head("bytes=-30"), 100), Some(Some((70, 99))));
        assert_eq!(range(&head("bytes=-300"), 100), Some(Some((0, 99))));
        assert_eq!(range(&head("bytes=100-"), 100), Some(None), "past the end");
        assert_eq!(range(&head("bytes=20-10"), 100), Some(None));
        assert_eq!(range(&head("bytes=0-1,5-6"), 100), None, "several ranges: all of it");
        assert_eq!(range(&head("lines=1-2"), 100), None);
        assert_eq!(range(b"GET /a HTTP/1.1\r\n\r\n", 100), None);
    }
}
