// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! A card's link as Veda's network service was told it. The service is
//! told whether the link is up when the card attaches, then whenever the
//! card's carrier is not what it was told: never against an earlier look
//! at the carrier, so that a carrier that changes while the card attaches
//! is told as soon as the session starts. One does: Linux's virtio_net
//! raises its carrier from a work item just after the card is switched on,
//! which may run while the attach waits for the service's answer.

/// Whether the network service was told a card's link is up: as the
/// session's attach told it, then as each change since.
pub struct Reported {
    up: bool,
}

impl Reported {
    /// Attaches a card with its link as `carrier` says it is now: `attach`
    /// is given whether it is up, which is what the service is told.
    /// Returns the session `attach` made, and what the service was told.
    pub fn attach<S, E>(
        carrier: impl FnOnce() -> bool,
        attach: impl FnOnce(bool) -> Result<S, E>,
    ) -> Result<(S, Reported), E> {
        let up = carrier();
        Ok((attach(up)?, Reported { up }))
    }

    /// What to tell the service now that the card's carrier is `up`:
    /// whether the link is up, if the service was told otherwise (it is
    /// told this from then on).
    pub fn change(&mut self, up: bool) -> Option<bool> {
        (up != self.up).then(|| {
            self.up = up;
            up
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// The carrier rises while the attach waits for the service, as
    /// virtio_net's does just after the card is switched on: the service
    /// was told the link is down, and is told it is up as the session
    /// starts.
    #[test]
    fn a_carrier_that_rises_while_the_card_attaches_is_told() {
        let carrier = Cell::new(false);
        let rise = |up| {
            carrier.set(true);
            Ok::<_, ()>(up)
        };
        let (attached_up, mut link) = Reported::attach(|| carrier.get(), rise).unwrap();
        assert!(!attached_up, "the attach told the service the link is down");
        assert_eq!(link.change(carrier.get()), Some(true), "the session tells it the link is up");
        assert_eq!(link.change(carrier.get()), None, "once");
    }

    #[test]
    fn each_change_is_told_once() {
        let (attached_up, mut link) = Reported::attach(|| true, Ok::<_, ()>).unwrap();
        assert!(attached_up);
        let told = [true, false, false, true, true].map(|up| link.change(up));
        assert_eq!(told, [None, Some(false), None, Some(true), None]);
    }

    #[test]
    fn a_failed_attach_has_no_session() {
        let failed = Reported::attach(|| true, |_| Err::<(), _>("no service"));
        assert!(matches!(failed, Err("no service")));
    }
}
