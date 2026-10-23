<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# The voice agent

Veda's voice agent is a system service, not an application: it sleeps until
called, talks through [Deepgram](https://deepgram.com), and works in
applications through the same code as the keyboard and mouse, asking before
anything that is hard to undo. This document covers how it works, consent,
memory, settings, agent-compatible applications and testing.

**Contents:** [How a conversation works](#how-a-conversation-works) · [The voice pipeline](#the-voice-pipeline) · [What it can do](#what-it-can-do) · [Consent and safety](#consent-and-safety) · [Memory](#memory) · [Settings](#settings) · [Making an application agent-compatible](#making-an-application-agent-compatible) · [Testing](#testing) · [Limitations](#limitations)

## How a conversation works

Say its name ("Hey Veda, open my shopping list"), click its ring at the
right of the taskbar, or press **Super+Space**. What you say with the name
is the first thing it hears. Interrupt it whenever you like: it stops
speaking at once. It falls asleep after a goodbye, or after a quiet spell
(40 seconds by default).

The service is `implementation/services/agent`; what it decides without
I/O is in `agent-core` (`core/`), tested on the host.

* **Waking** opens a conversation with Deepgram's Voice Agent API
  (`wss://agent.deepgram.com/v1/agent/converse`) and sends its `Settings`:
  16 kHz microphone audio in, 24 kHz voice out, Flux recognition (which
  decides when you have finished a turn), the language model, the voice,
  the functions and the instructions (`agent_core::prompt`). They include
  what it remembers about you, what is on the screen, every application's
  actions and how the last conversation ended (for reference: each
  conversation starts afresh). `mip_opt_out`, here and on the name
  listener's recognition alike, keeps the audio out of Deepgram's model
  improvement. Deepgram must confirm within ten seconds,
  or the conversation ends.
* **Doing.** The language model calls functions, which a worker thread
  runs, answering with JSON. One that needs consent answers "waiting for
  approval", and its outcome comes later as a notice
  (`agent_core::notice`). A call you cancel by speaking again is dropped,
  unless it had started: it then runs to the end, and if it may have
  changed something, the agent is told what it did.
* **Errors** (the network, Deepgram, the key) are shown for ten seconds;
  then the agent sleeps and listens for its name again.

## The voice pipeline

### Waking by name

With "Answer when someone says its name" on
(`implementation/services/agent/source/listen.rs`), a voice detector
(`audio::vad`) runs on the computer, and nothing is sent anywhere while
nobody speaks. Speech streams to Deepgram's Nova-3 recogniser, with the
agent's name as a key term, until 3 s of quiet, 8 s without a word, or a
minute. `agent_core::wake` wakes the agent when the name starts or ends a
sentence ("Hey Veda, …", "… Veda?"), not when it is only mentioned; one
letter off counts ("Vida"), common words aside. What follows the name, to
the end of the sentence, comes with the call.

While the computer plays something, only a sound louder than its echo can
be starts recognition, as the echo gate decides. Other wordless sound (a
television, music from elsewhere) is taken for a background: until it
stops, only a sound clearly louder than its usual peaks starts recognition.
Recognition is capped at ten minutes an hour for the agent as a whole, so a
television or music playing all day cannot run up the bill.

### Echo cancellation and the echo gate

The audio service cancels the echo in the microphone (`audio::aec`, with
the mixed speaker output as the reference), but no canceller removes
everything, and the recogniser would take what is left of the agent's voice
for you. So while the agent's voice is audible, and for its echo's length
after, the echo gate (`agent_core::gate`) sends silence instead of the
microphone, unless you are clearly louder than the echo could be; it then
sends the last 300 ms too, so your first words are not lost. Music on its
own is not held back.

The gate opens only when both the cleaned microphone and the microphone as
recorded are louder than their echo: a canceller that loses track for a
moment raises the first, but only someone talking raises the second.

### Ducking and playback

During a conversation, the audio service ducks every other sound by 20 dB
so that the agent hears you. The agent's voice is buffered against network
jitter, and flushed at once when you start talking (`UserStartedSpeaking`).

## What it can do

The agent's own functions (`agent_core::tools`):

| Function | What it does |
|----------|--------------|
| `get_status` | Time, open windows, volume, network |
| `list_apps`, `open_app` | Installed applications; open one (with a file) |
| `app_actions`, `use_app`, `read_app` | An application's actions; run one; what it shows |
| `window` | List, focus, minimise, maximise, restore, snap or close windows; show the desktop |
| `files` | List, find, read, write, copy, move, rename and delete (to the Trash) your home's files and folders; list, restore from and empty the Trash |
| `volume`, `wifi`, `wallpaper` | Sound, Wi-Fi networks, the wallpaper |
| `notify`, `timer` | A notification; timers and reminders (they wake the agent) |
| `memory` | Remember, search and forget what it knows about you |
| `system`, `tasks` | System details, lock the screen, restart and shut down; running programs |
| `end_conversation` | Go back to sleep once its goodbye has played (unless you talk over it or ask for more) |

Applications' own actions run through `use_app`: "open the shopping list
and add milk" becomes `use_app(editor, open_file …)` then
`use_app(editor, write …)`.

## Consent and safety

The agent service enforces consent, not the language model: a call that
needs it is held, and runs only after you click Allow.

| Risk | For example |
|------|-------------|
| **Routine**: runs at once | Looking, navigating, writing a new file, copying, moving and renaming (which never replace what is there), playing music |
| **Sensitive**, hard to undo or reaching beyond the computer: asked, unless always allowed | Replacing a file, moving files to the Trash, renaming the agent, changing its language model, a terminal command that loses nothing |
| **Destructive**: asked every time | Emptying the Trash, discarding unsaved work, ending programs, a terminal command that deletes, replaces or ends something (or a program), shutting down, forgetting more than one memory at once |

* **What you allow is what runs.** A call is made definite before its risk
  is judged: a request to forget names its memories (which the approval
  lists), a move's destination is worked out as it will be used. Where risk
  depends on arguments, an application offers two actions: the Terminal's
  `run_command` refuses what its `run_risky_command` runs, and the Text
  Editor's `save` replaces only what the agent itself saved, its `save_as`
  anything.
* **Only the desktop shell may answer** approvals, and only Settings may
  change the agent's settings, both recognised by the process identity that
  init attaches to every connection (system service names cannot be taken
  by other programs).
* **Requests** appear in the agent's window
  (`implementation/shell/components/desktop/source/agent.rs`), or as
  notifications while it is closed, show what will happen and on what, and
  expire after three minutes. *Always allow* is offered for sensitive
  actions only, and Settings can take it back. Stored permissions are
  checked when read back: only a sensitive action of the agent's own
  functions, or of an application's description, stays allowed.
* **The agent cannot touch the firewall** at all: Settings offers it no
  action for it, the network service takes its settings from Settings
  alone, and only the desktop shell, whose buttons the agent cannot press,
  answers its questions (whether an application may accept connections).
* **Information, never instructions.** The prompt says so of what the agent
  reads from applications and files, and nothing in a document can approve
  anything. What an application says about itself enters the prompt marked
  as its own words, one line each. In a notice, data from elsewhere follows
  the computer's sentence as JSON, with the marker `[System]` taken out, so
  that it cannot pass for the computer.
* **Only the user's files.** The file actions check every path they touch
  first (`agent_core::files`), including where an item goes back to from
  the Trash (whose records any program can write): none may be in a private
  directory, and what they create goes in the home folder or `/tmp`. They
  reach the file system through a connection of their own, apart from the
  agent's private store's.
* **Reminders.** Until you speak, a conversation that a due reminder (or
  another notice) started cannot set timers, so a reminder cannot keep
  waking the agent.
* **Not while the screen is locked.** The agent watches the lock
  (`display::watch_lock`, `implementation/services/agent/source/lock.rs`):
  as the screen locks, a conversation ends and the agent stops listening
  for its name, so that nobody at a locked computer can ask it for what the
  lock keeps from them. It takes the screen for locked until the window
  system says otherwise, as it starts and while the window system is away.
  A reminder that comes due meanwhile is told once the screen is unlocked.
  Asked to lock the screen, it gives the conversation the result first;
  the lock then ends the conversation.
* **Talking counts as input; overhearing does not.** The screen locks
  after a while without input, and a conversation held hands-free gives
  none, so the agent tells the window system that you are there
  (`display::user_active`, which only the agent may call;
  `agent_core::presence`): in a conversation you asked for (a click,
  Super+Space, its name), whenever Deepgram hears you start to speak and
  gives your words, and while its voice answers. Nothing it hears while it
  only listens for its name counts, its name included, nor anything in a
  conversation that a reminder (or another notice) started, whoever speaks
  in it: the microphone hears whatever is said near the computer (a
  television, people in the room), and if that counted, an unattended
  computer would stay unlocked while anyone talked near it. A conversation
  you asked for is yours, as a film you play is: talk in it holds the lock
  off until it ends.
* **The Deepgram key** stays in the agent's private directory
  (`/home/.private/agent`, which the file system service lets only the
  agent open). It is never shown again (Settings shows its last characters)
  or offered to the language model, and goes only to Deepgram's own
  services, over TLS; a test endpoint (`agent.endpoint=…`) is sent the word
  `test` instead. The private store replaces a file in one step, through a
  temporary name, so a crash never loses one.

## Memory

The agent remembers what you tell it about yourself (your name, people in
your life, plans, how you like things done) when you say it plainly or ask
it to, never its own guesses. It also notes which applications you use at
what times of day, and keeps the last turns of recent conversations.

All of it lives in `/home/.private/agent/memory.json`, on your computer;
the prompt carries what is relevant. Settings shows it all and forgets any
item, or everything; you can also ask the agent. It forgets only the
memories your words name: "forget that" forgets nothing.

## Settings

| Settings → Agent | Sets | Default |
|------------------|------|---------|
| Deepgram | The API key, checked with Deepgram when saved | |
| Voice | Name (which it answers to), voice (Deepgram's Aura-2, with a spoken preview), speaking rate | Veda, Aura-2 Helena |
| Intelligence | Language model (Deepgram's catalogue with its price tier: Standard costs less per minute than Advanced), speech recognition model | OpenAI's gpt-4.1-mini (Standard), Flux |
| Listening | Answering to its name; how long a quiet conversation lasts | On, 40 seconds |
| Memory, Always allowed | What it remembers; the actions always allowed, to take back | |

The agent can change its own voice and speaking rate, its name and
language model only with your OK, and the key not at all.

## Making an application agent-compatible

Agent support is part of `ui`: an application describes its actions,
reports what it shows and performs actions (three methods of `ui::App`),
and `ui::run` registers it with the agent service (again after the
service restarts) and answers its requests (`proto::agent`'s `agentapp`
protocol) between frames.

```rust
use ui::agent::{self, Action, AppAgentInfo, Risk, Value, arg_path, arg_str, json, show_path};

impl ui::App for Notes {
    fn update(&mut self, ui: &mut ui::Ui) { /* ... */ }

    fn agent_info(&self) -> Option<AppAgentInfo> {
        Some(agent::info(
            "A notes app: one note per file in ~/Notes.",
            vec![
                Action::new("open_note", "Shows a note").param("name", "string", "Its title", true).build(),
                Action::new("add_line", "Adds a line at the end of the note shown")
                    .param("text", "string", "The line", true)
                    .build(),
                Action::new("delete_note", "Deletes a note for good")
                    .param("path", "string", "The note's file, such as ~/Notes/Ideas.txt", true)
                    .risk(Risk::Destructive)
                    .build(),
            ],
        ))
    }

    fn agent_state(&self) -> Value {
        json!({ "note": self.title.as_str(), "lines": self.lines.len() })
    }

    fn agent_invoke(&mut self, action: &str, args: &Value) -> Result<Value, String> {
        match action {
            "open_note" => { /* the same code as clicking it */ Ok(json!({ "showing": self.title.as_str() })) }
            "add_line" => { self.append(arg_str(args, "text")?); Ok(json!({ "lines": self.lines.len() })) }
            "delete_note" => {
                let path = arg_path(args, "path")?;
                self.delete(&path).map_err(|e| format!("{} could not be deleted: {e}", show_path(&path)))?;
                Ok(json!({ "deleted": show_path(&path) }))
            }
            other => Err(format!("Notes has no action called {other}")),
        }
    }
}
```

The built-in applications follow these conventions
(`implementation/applications/editor/source/agent.rs` is a compact example):

* **Actions**: snake_case verbs, with short imperative descriptions for a
  language model that give units, formats and defaults; `choice` for
  enumerations, `"array"` parameters (`arg_list`) for lists. Offer what
  people ask for, not every menu item.
* **Targets**: an approved action may run minutes later, so name its
  target in the arguments (a file path, a document's name, never "the
  selected one"), look it up again when it runs, refuse if it is gone, and
  match destructive targets exactly.
* **State**: compact JSON of what the window shows; lists capped (about 60
  items, with a total), long text cut with `clip`, no secrets.
* **Invoke**: the same code as the keyboard and mouse, so the window shows
  the result. Return a small JSON object saying what happened, or a
  sentence the agent can say ("there is no folder called Holidays in
  ~/Pictures").
* **Paths**: accept `~/…`, absolute and home-relative paths (`arg_path`);
  show them with `show_path`.

Programs with their own loop (a game such as Interceptor) keep a
`ui::agent::Registration`, call `serve` with their `AgentServer` once per
frame (it never waits), and add `wait_items` to their wait set if they
sleep between events.

Any program may register, so the agent bounds what one says about itself
(`agent_core::tools::sanitize`):

| Limit | Value |
|-------|-------|
| Actions | 64 at most, of 16 parameters at most; names of letters, digits, `_` and `-`, each used once |
| Descriptions | One line of at most 600 characters |
| Running an action | On the application's main thread, 30 seconds at most |
| Registrations | One per process (a new one replaces it); more than four a minute are refused for the rest of it |

## Testing

| Tests | What they check | Run by |
|-------|-----------------|--------|
| Host unit tests | `agent-core`, and the `audio` library's echo canceller and voice detector | `cargo automation test` |
| `verification/agent/` | The agent in Veda, with a stand-in for Deepgram: offline, free, deterministic | `cargo automation test --ui` |
| `verification/real/agent-*.vts` | The agent with the real Deepgram, which bills them | By hand |

The stand-in, `automation/source/agentsim.rs`, speaks the Voice Agent and
streaming recognition protocols: it answers `Settings`, records what the
agent sends, and sends function calls, voice and interruptions. Scripts
drive it with `agent-*` commands, and the agent's microphone (`testmic`)
with `mic-*` commands and `say` (Deepgram's text to speech, on the host),
all listed in `automation/source/script.rs`. Scripts with `mic-echo` (the
machine's output fed back into the microphone) run alone, after the others.

The real tests type the key from `$DEEPGRAM_API_KEY` into Settings with
`type-env`, so it appears in neither scripts nor logs:

```bash
export DEEPGRAM_API_KEY=...
cargo automation script verification/real/agent-deepgram.vts
```

## Limitations

* English only: Flux and the name detector's key term are English.
* Speech recognition can mishear. The agent asks when a word seems out of
  place, but a misheard name can wake it, or not.
* You must be louder at the microphone than the echo to talk over the
  agent or music, or to call it over music: with loud speakers right next
  to it, speak up or use headphones.
* The agent needs the Internet and a Deepgram account. Asleep, only the
  name listener's short recognitions are billed; every conversation minute
  is.
* Talk that reaches a conversation you left going (a television near the
  computer) keeps it going, and with it the screen unlocked: say goodbye,
  or lock the screen, as you leave.
