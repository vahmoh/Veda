// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Whether the user is there: what in a conversation the agent tells the
//! window system, which takes it as input (`display::user_active`), so
//! that a conversation held hands-free does not end with the screen
//! locking for want of input.
//!
//! * **What counts**, in a conversation the user asked for (a click on the
//!   agent, Super+Space, its name): Deepgram hearing the user start to
//!   speak, the words it heard when they stop, and the agent's voice while
//!   it plays (an answer read out at length).
//! * **What does not:** whatever the agent hears while it only listens for
//!   its name, its name included, and anything in a conversation that a
//!   notice started (a reminder that came due), whoever then speaks in it.
//!   Talk near an unattended computer (a television, people in the room)
//!   reaches the microphone all day: if it counted, the computer would not
//!   lock while anyone talked near it. Nor do the words the agent gives
//!   Deepgram as the user's turn (what was said with its name, the
//!   computer's notices), should they come back as the user's: only what
//!   Deepgram heard counts.
//! * **A conversation the user asked for is theirs**, as a film they play
//!   is: talk in it holds the lock off until it ends, with a goodbye or
//!   after a quiet spell.
//!
//! The window system is told soon after what showed the user, and at most
//! every [`REPORT_NS`].

use crate::deepgram::{Role, ServerMessage};

/// How often the window system is told at most (ns): often enough against
/// the shortest time the screen waits for input (a minute), and seldom
/// enough to cost nothing.
pub const REPORT_NS: u64 = 5_000_000_000;

/// What a conversation has shown of the user (see the module's comment).
#[derive(Debug, Clone, Default)]
pub struct Presence {
    /// The user asked for the conversation: only then does it count.
    asked: bool,
    /// Deepgram heard the user start to speak, and has not given the words
    /// yet.
    speaking: bool,
    /// Something showed the user since the window system was last told ...
    seen: bool,
    /// ... then.
    told: Option<u64>,
}

impl Presence {
    /// A conversation's: `asked` if the user asked for it, rather than a
    /// notice starting it.
    pub fn new(asked: bool) -> Presence {
        Presence { asked, ..Presence::default() }
    }

    /// The user asked for the conversation as it went on (a click on the
    /// agent while it told a reminder): it counts from now on.
    pub fn ask(&mut self) {
        self.asked = true;
    }

    /// Deepgram said `m`.
    pub fn heard(&mut self, m: &ServerMessage) {
        match m {
            ServerMessage::UserStartedSpeaking => {
                self.speaking = true;
                self.seen |= self.asked;
            }
            // What it heard of them: the words come as they stop.
            ServerMessage::ConversationText { role: Role::User, .. } if core::mem::take(&mut self.speaking) => {
                self.seen |= self.asked;
            }
            _ => {}
        }
    }

    /// Whether to tell the window system at `now` that the user is there,
    /// `voice` saying whether the agent's voice plays; if so, it is taken
    /// as told.
    pub fn tell(&mut self, now: u64, voice: bool) -> bool {
        self.seen |= voice && self.asked;
        if !self.seen || self.told.is_some_and(|t| now.saturating_sub(t) < REPORT_NS) {
            return false;
        }
        self.seen = false;
        self.told = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second (ns).
    const S: u64 = 1_000_000_000;

    fn user(words: &str) -> ServerMessage {
        ServerMessage::ConversationText { role: Role::User, content: words.into() }
    }

    #[test]
    fn the_users_turns_count_in_a_conversation_they_asked_for() {
        let mut p = Presence::new(true);
        // Nothing yet.
        assert!(!p.tell(S, false));
        // They start to speak: the window system hears of it at once ...
        p.heard(&ServerMessage::UserStartedSpeaking);
        assert!(p.tell(2 * S, false));
        assert!(!p.tell(3 * S, false));
        // ... and of their words when they stop, once it may be told again.
        p.heard(&user("What is on the screen?"));
        assert!(!p.tell(4 * S, false));
        assert!(p.tell(7 * S, false));
        // Then nothing until they speak again.
        assert!(!p.tell(60 * S, false));
        p.heard(&ServerMessage::UserStartedSpeaking);
        assert!(p.tell(61 * S, false));
    }

    #[test]
    fn the_agents_voice_counts_while_it_plays() {
        let mut p = Presence::new(true);
        assert!(p.tell(S, true));
        assert!(!p.tell(2 * S, true));
        assert!(p.tell(6 * S, true));
        assert!(p.tell(11 * S, true));
        // Over: nothing more.
        assert!(!p.tell(20 * S, false));
    }

    #[test]
    fn words_the_agent_was_given_are_not_the_users() {
        let mut p = Presence::new(true);
        // What was said with its name, and a notice, come back as the
        // user's words, with nothing heard before them.
        p.heard(&user("Hey Veda, what time is it?"));
        p.heard(&user("[System] A reminder came due: call Anna."));
        // Nor do the agent's words, its thinking or the history count.
        p.heard(&ServerMessage::ConversationText { role: Role::Assistant, content: "It is noon.".into() });
        p.heard(&ServerMessage::AgentThinking { content: "".into() });
        p.heard(&ServerMessage::History { role: Role::User, content: "Hello".into() });
        assert!(!p.tell(10 * S, false));
        // Words heard count once: a notice given after them does not.
        p.heard(&ServerMessage::UserStartedSpeaking);
        p.heard(&user("Thanks."));
        assert!(p.tell(11 * S, false));
        p.heard(&user("[System] The user allowed an action and it was done."));
        assert!(!p.tell(30 * S, false));
    }

    #[test]
    fn nothing_counts_in_a_conversation_a_notice_started() {
        let mut p = Presence::new(false);
        // A reminder, told at an unattended computer with a television on.
        p.heard(&ServerMessage::UserStartedSpeaking);
        p.heard(&user("And now the weather."));
        assert!(!p.tell(S, true));
        assert!(!p.tell(30 * S, false));
        // The user clicks the agent: from then on, it is theirs.
        p.ask();
        assert!(!p.tell(31 * S, false));
        p.heard(&ServerMessage::UserStartedSpeaking);
        assert!(p.tell(32 * S, false));
    }
}
