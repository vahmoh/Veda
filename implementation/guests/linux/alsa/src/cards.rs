// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The sound card to drive, as Linux lists its cards and their devices
//! (`/proc/asound`): the first that plays to the PC's own speakers or
//! headphones (a display's, HDMI or DisplayPort, only after a while, if
//! there is no other), and its built-in microphones.

use std::time::{Duration, Instant};

/// How long a display's sound (HDMI, DisplayPort) waits for the PC's own:
/// a laptop's speakers' card may come after its GPU's.
const DISPLAYS_WAIT: Duration = Duration::from_secs(10);

/// A sound card: its number and name, and its devices.
pub struct Card {
    pub number: String,
    pub name: String,
    pub playback: String,
    pub capture: Option<String>,
}

/// A PCM device, as `/proc/asound/pcm` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Device {
    card: u32,
    device: u32,
    /// Its id (SOF's digital microphones' is `DMIC (*)`).
    id: String,
    playback: bool,
    capture: bool,
}

/// The devices `/proc/asound/pcm` lists:
/// `CC-DD: id : name : playback 1 : capture 1`.
fn devices(list: &str) -> Vec<Device> {
    list.lines()
        .filter_map(|l| {
            let (at, rest) = l.split_once(": ")?;
            let (card, device) = at.split_once('-')?;
            let fields: Vec<&str> = rest.split(" : ").collect();
            let has = |stream: &str| fields.iter().skip(2).any(|f| f.trim().starts_with(stream));
            Some(Device {
                card: card.trim().parse().ok()?,
                device: device.trim().parse().ok()?,
                id: fields.first()?.trim().to_string(),
                playback: has("playback"),
                capture: has("capture"),
            })
        })
        .collect()
}

impl Device {
    /// Whether it plays to a display (HDMI, DisplayPort), not the PC's own
    /// speakers or headphones.
    fn is_display(&self) -> bool {
        ["HDMI", "DisplayPort"].iter().any(|d| self.id.starts_with(d))
    }
}

/// The device to play: the first of the cards `listed` that is not a
/// display's, or else, with `displays`, a display's.
fn output<'a>(devices: &'a [Device], listed: &[u32], displays: bool) -> Option<&'a Device> {
    let playing = || devices.iter().filter(|d| d.playback && listed.contains(&d.card));
    playing().find(|d| !d.is_display()).or_else(|| playing().find(|_| displays))
}

/// The device of card `card` that records its built-in microphones: an
/// audio DSP's digital microphones (SOF's `DMIC`), if it has them (its
/// other capture devices are then its codec's, a headset's microphone on
/// the jack, and the same microphones at 16 kHz); else its first.
fn microphones(devices: &[Device], card: u32) -> Option<&Device> {
    let captures = || devices.iter().filter(move |d| d.card == card && d.capture);
    captures().find(|d| d.id.split_whitespace().next() == Some("DMIC")).or_else(|| captures().next())
}

/// The card to play ([`output`]), once there is one, and its microphones.
/// Linux lists a card (`/proc/asound/cards`) once it has made all of its
/// devices, the mixer last (devtmpfs makes their nodes as it does): a card
/// is taken once it is listed, and the list is read first.
pub fn find_card() -> Card {
    let started = Instant::now();
    let mut said = false;
    loop {
        // ` 0 [Intel          ]: HDA-Intel - HDA Intel`
        let cards: Vec<(u32, String)> = std::fs::read_to_string("/proc/asound/cards")
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (number, rest) = l.trim_start().split_once(" [")?;
                Some((number.parse().ok()?, rest.split_once(" - ")?.1.trim().to_string()))
            })
            .collect();
        let devices = devices(&std::fs::read_to_string("/proc/asound/pcm").unwrap_or_default());
        let listed: Vec<u32> = cards.iter().map(|c| c.0).collect();
        if let Some(d) = output(&devices, &listed, started.elapsed() >= DISPLAYS_WAIT) {
            let capture = microphones(&devices, d.card).map(|m| format!("/dev/snd/pcmC{}D{}c", m.card, m.device));
            return Card {
                number: d.card.to_string(),
                name: cards.iter().find(|c| c.0 == d.card).map(|c| c.1.clone()).unwrap_or_default(),
                playback: format!("/dev/snd/pcmC{}D{}p", d.card, d.device),
                capture,
            };
        }
        if !said {
            println!("alsa: waiting for a sound card");
            said = true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What Linux lists of its cards (`/proc/asound/cards`): it changes when a
/// card comes or goes.
pub fn listed() -> String {
    std::fs::read_to_string("/proc/asound/cards").unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As Linux lists a card with an audio DSP (SOF's, on an Alder Lake
    /// laptop), after a GPU's HDMI audio.
    const SOF: &str = "\
00-03: HDMI 0 : HDMI 0 : playback 1
01-00: HDA Analog (*) :  : playback 1 : capture 1
01-03: HDMI1 (*) : HDMI 1 : playback 1
01-06: DMIC (*) :  : capture 1
01-07: DMIC16kHz (*) :  : capture 1
01-31: HDA Analog Deep Buffer (*) :  : playback 1
";

    #[test]
    fn devices_are_read_as_listed() {
        let d = devices(SOF);
        assert_eq!(d.len(), 6);
        assert_eq!(d[1], Device { card: 1, device: 0, id: "HDA Analog (*)".into(), playback: true, capture: true });
        assert_eq!(d[3], Device { card: 1, device: 6, id: "DMIC (*)".into(), playback: false, capture: true });
        assert!(!d[5].capture && d[5].playback);
    }

    #[test]
    fn the_pcs_own_sound_comes_before_a_displays() {
        let d = devices(SOF);
        // The GPU's HDMI card 0, the DSP's card 1: the DSP's codec plays.
        assert_eq!(output(&d, &[0, 1], false).map(|o| (o.card, o.device)), Some((1, 0)));
        // The GPU's alone: only once displays may play.
        assert_eq!(output(&d, &[0], false), None);
        assert_eq!(output(&d, &[0], true).map(|o| (o.card, o.device)), Some((0, 3)));
        // A card is taken once it is listed.
        assert_eq!(output(&d, &[], true), None);
    }

    #[test]
    fn the_microphones_are_the_dsps_digital_ones() {
        let d = devices(SOF);
        assert_eq!(microphones(&d, 1).map(|m| m.device), Some(6));
        // A card without them: its first capture device; none, without.
        let hda = devices("00-00: ALC294 Analog : ALC294 Analog : playback 1 : capture 1\n");
        assert_eq!(microphones(&hda, 0).map(|m| m.device), Some(0));
        assert_eq!(microphones(&d, 0), None);
    }
}
