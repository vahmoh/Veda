// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! The command language of vsh, the Terminal's shell.
//!
//! A command line is split into words (quotes, backslash escapes, `$VAR`
//! and `~` expansion, `*`/`?` wildcards) and operators ([`tokenize`]),
//! which form lists (`;`, `&&`, `||`) of pipelines (`|`, `|&`) of commands
//! with redirections ([`parse`]): `< FILE`, `> FILE`, `>> FILE`, the same
//! for standard error (`2>`, `2>>`) or for both streams (`&>`, `&>>`), and
//! one stream sent where the other goes (`2>&1`, `>&2`). Only the three
//! standard streams can be redirected, and nothing runs in the background:
//! a lone `&` is an error, not part of a word.
//!
//! [`route`] works out where a command's streams go once its redirections
//! are applied in order, as POSIX shells do (`> log 2>&1` sends both to
//! the file; `2>&1 > log` sends errors where the output went before).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

/// A word of a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// Its text: quotes and escapes removed, `$VAR` and `~` expanded.
    pub text: String,
    /// It has unquoted wildcards (`*`, `?`) to match against file names.
    pub glob: bool,
    /// Part of it came from a variable (`$NAME`).
    pub expanded: bool,
    /// Where it is in the line (byte offsets).
    pub span: Range<usize>,
}

/// One of a command's two output streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Standard output (1).
    Out,
    /// Standard error (2).
    Err,
}

/// A redirection, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redirect {
    /// `< FILE`: standard input from a file.
    Input(Word),
    /// `> FILE`, `>> FILE` (`append`), `2> FILE`: a stream to a file.
    Output { stream: Stream, path: Word, append: bool },
    /// `2>&1`, `>&2`: `stream` goes where `to` goes at this point.
    Dup { stream: Stream, to: Stream },
}

/// A token of the command language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Word(Word),
    /// `|`
    Pipe,
    /// `|&`: a pipe that carries standard error too.
    PipeBoth,
    /// `;`
    Semi,
    /// `&&`
    And,
    /// `||`
    Or,
    /// `<` (or `0<`)
    Input,
    /// `>`, `>>`, `2>`, `2>>` (`stream`), or `&>`, `&>>` (both streams:
    /// `stream` is `None`).
    Output {
        stream: Option<Stream>,
        append: bool,
    },
    /// `2>&1`, `>&2`
    Dup {
        stream: Stream,
        to: Stream,
    },
    /// A lone `&`: running a command in the background.
    Background,
}

/// How a list item is connected to the item before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connector {
    /// After `;` (or the first item): always runs.
    Always,
    /// After `&&`: runs if the item before succeeded.
    And,
    /// After `||`: runs if the item before failed.
    Or,
}

/// One command of a pipeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Command {
    pub words: Vec<Word>,
    /// In the order written.
    pub redirects: Vec<Redirect>,
}

/// Commands connected by pipes.
pub type Pipeline = Vec<Command>;

/// Why a command line cannot run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxError {
    /// A quote (`'` or `"`) is not closed.
    UnterminatedQuote(char),
    /// A redirection without a file name after it.
    MissingFileName,
    /// A redirection without a command.
    MissingCommand,
    /// An operator where a command should be.
    Unexpected(&'static str),
    /// The line ends after an operator that needs more.
    UnexpectedEnd,
    /// `&`: vsh does not run commands in the background.
    Background,
    /// A redirection of a stream other than standard input, output and
    /// error, or the wrong one for its direction (`1<`, `0>`).
    BadStream(String),
    /// An operator vsh does not have (here-documents and the like).
    Unsupported(&'static str),
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyntaxError::UnterminatedQuote(q) => write!(f, "unterminated quote ({q})"),
            SyntaxError::MissingFileName => f.write_str("missing file name after redirection"),
            SyntaxError::MissingCommand => f.write_str("missing command before redirection"),
            SyntaxError::Unexpected(op) => write!(f, "syntax error near '{op}'"),
            SyntaxError::UnexpectedEnd => f.write_str("unexpected end of line"),
            SyntaxError::Background => f.write_str(
                "'&' would run a command in the background, which vsh does not do (quote it to use it in a word: 'a&b')",
            ),
            SyntaxError::BadStream(what) => {
                write!(f, "cannot redirect '{what}': only standard input (0), output (1) and errors (2) can be")
            }
            SyntaxError::Unsupported(what) => write!(f, "{what} is not supported"),
        }
    }
}

/// The values of variables (`$NAME`, `${NAME}`, `$?`).
pub type Vars<'a> = &'a dyn Fn(&str) -> Option<String>;

/// How [`tokenize`] reads a line.
pub struct Lexer<'a> {
    /// The values of variables; `None` keeps references as written.
    pub vars: Option<Vars<'a>>,
    /// What a leading `~` stands for.
    pub home: &'a str,
    /// Reads past what would be errors (an unterminated quote runs to the
    /// end of the line, unknown operators are skipped): for looking at a
    /// line, never for running it.
    pub lenient: bool,
}

/// A tokenised line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tokens {
    pub toks: Vec<Tok>,
    /// Where each item of the list after the first starts (a byte offset,
    /// after its separator), and how it is connected.
    pub breaks: Vec<(usize, Connector)>,
}

/// Splits a command line into tokens.
pub fn tokenize(line: &str, lx: &Lexer) -> Result<Tokens, SyntaxError> {
    let mut l = Lex { line, chars: line.char_indices().collect(), lx, out: Tokens::default(), word: None };
    l.run()?;
    Ok(l.out)
}

/// The word being read.
struct Partial {
    text: String,
    start: usize,
    glob: bool,
    expanded: bool,
    /// Only plain characters so far: no quotes, escapes or expansions (a
    /// word of plain digits before `>` or `<` names a stream).
    plain: bool,
}

struct Lex<'a> {
    line: &'a str,
    chars: Vec<(usize, char)>,
    lx: &'a Lexer<'a>,
    out: Tokens,
    word: Option<Partial>,
}

impl Lex<'_> {
    /// The byte offset of character `i` (the line's length past its end).
    fn pos(&self, i: usize) -> usize {
        self.chars.get(i).map_or(self.line.len(), |c| c.0)
    }

    fn ch(&self, i: usize) -> Option<char> {
        self.chars.get(i).map(|c| c.1)
    }

    /// The word being read, started at character `i` if there is none.
    fn word(&mut self, i: usize) -> &mut Partial {
        let start = self.pos(i);
        self.word.get_or_insert_with(|| Partial {
            text: String::new(),
            start,
            glob: false,
            expanded: false,
            plain: true,
        })
    }

    /// Ends the word being read before character `i`.
    fn end_word(&mut self, i: usize) {
        if let Some(w) = self.word.take() {
            let span = w.start..self.pos(i);
            self.out.toks.push(Tok::Word(Word { text: w.text, glob: w.glob, expanded: w.expanded, span }));
        }
    }

    /// A word of plain digits being read before an operator at `i`: it
    /// names the stream the operator redirects. Taken, it is no word.
    fn take_stream_number(&mut self) -> Option<String> {
        let w = self.word.as_ref()?;
        if !w.plain || w.text.is_empty() || !w.text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        self.word.take().map(|w| w.text)
    }

    /// An error, or (lenient) nothing.
    fn fail(&self, e: SyntaxError) -> Result<(), SyntaxError> {
        if self.lx.lenient { Ok(()) } else { Err(e) }
    }

    fn run(&mut self) -> Result<(), SyntaxError> {
        let mut i = 0;
        while let Some(c) = self.ch(i) {
            match c {
                ' ' | '\t' => self.end_word(i),
                // A comment (looking at a line, everything is read: a
                // password typed after `#` is still hidden).
                '#' if self.word.is_none() && !self.lx.lenient => break,
                '|' => {
                    self.end_word(i);
                    match self.ch(i + 1) {
                        Some('|') => {
                            i += 1;
                            self.out.toks.push(Tok::Or);
                            self.out.breaks.push((self.pos(i + 1), Connector::Or));
                        }
                        Some('&') => {
                            i += 1;
                            self.out.toks.push(Tok::PipeBoth);
                        }
                        _ => self.out.toks.push(Tok::Pipe),
                    }
                }
                '&' => {
                    self.end_word(i);
                    match self.ch(i + 1) {
                        Some('&') => {
                            i += 1;
                            self.out.toks.push(Tok::And);
                            self.out.breaks.push((self.pos(i + 1), Connector::And));
                        }
                        Some('>') => {
                            i += 1;
                            let append = self.ch(i + 1) == Some('>');
                            if append {
                                i += 1;
                            }
                            self.out.toks.push(Tok::Output { stream: None, append });
                        }
                        _ => {
                            self.fail(SyntaxError::Background)?;
                            self.out.toks.push(Tok::Background);
                        }
                    }
                }
                ';' => {
                    self.end_word(i);
                    self.out.toks.push(Tok::Semi);
                    self.out.breaks.push((self.pos(i + 1), Connector::Always));
                }
                '>' => i = self.output(i)?,
                '<' => i = self.input(i)?,
                '\'' => {
                    let w = self.word(i);
                    w.plain = false;
                    let mut j = i + 1;
                    let mut text = String::new();
                    while let Some(c) = self.ch(j).filter(|&c| c != '\'') {
                        text.push(c);
                        j += 1;
                    }
                    self.word(i).text.push_str(&text);
                    if self.ch(j).is_none() {
                        self.fail(SyntaxError::UnterminatedQuote('\''))?;
                    }
                    i = j;
                }
                '"' => {
                    self.word(i).plain = false;
                    let mut j = i + 1;
                    loop {
                        let Some(c) = self.ch(j) else {
                            self.fail(SyntaxError::UnterminatedQuote('"'))?;
                            break;
                        };
                        match c {
                            '"' => break,
                            '\\' if matches!(self.ch(j + 1), Some('"' | '\\' | '$' | '`')) => {
                                let n = self.ch(j + 1).unwrap_or('\\');
                                self.word(i).text.push(n);
                                j += 2;
                            }
                            '$' => j = self.variable(i, j),
                            c => {
                                self.word(i).text.push(c);
                                j += 1;
                            }
                        }
                    }
                    i = j;
                }
                '\\' => {
                    let w = self.word(i);
                    w.plain = false;
                    if let Some(n) = self.ch(i + 1) {
                        self.word(i).text.push(n);
                        i += 1;
                    }
                }
                '$' => {
                    i = self.variable(i, i);
                    continue;
                }
                '~' if self.word.is_none()
                    && matches!(self.ch(i + 1), None | Some('/' | ' ' | '\t' | ';' | '|' | '&' | '<' | '>')) =>
                {
                    let home = self.lx.home;
                    let w = self.word(i);
                    w.plain = false;
                    w.text.push_str(home);
                }
                c => {
                    let w = self.word(i);
                    if c == '*' || c == '?' {
                        w.glob = true;
                    }
                    w.text.push(c);
                }
            }
            i += 1;
        }
        let end = self.chars.len();
        self.end_word(end);
        Ok(())
    }

    /// `>` at character `i` (with a stream number before it): returns the
    /// index of its last character.
    fn output(&mut self, mut i: usize) -> Result<usize, SyntaxError> {
        let number = self.take_stream_number();
        self.end_word(i);
        let stream = match number.as_deref() {
            None | Some("1") => Some(Stream::Out),
            Some("2") => Some(Stream::Err),
            Some(n) => {
                self.fail(SyntaxError::BadStream(alloc::format!("{n}>")))?;
                None
            }
        };
        match self.ch(i + 1) {
            Some('>') => {
                i += 1;
                if let Some(stream) = stream {
                    self.out.toks.push(Tok::Output { stream: Some(stream), append: true });
                }
            }
            Some('&') => {
                i += 1;
                let to = match self.ch(i + 1) {
                    Some('1') => Some(Stream::Out),
                    Some('2') => Some(Stream::Err),
                    _ => None,
                };
                match (stream, to) {
                    (Some(stream), Some(to)) => {
                        i += 1;
                        self.out.toks.push(Tok::Dup { stream, to });
                    }
                    _ => self.fail(SyntaxError::Unsupported("'>&' other than '2>&1' or '>&2'"))?,
                }
            }
            next => {
                // `>|` overrides a shell option vsh does not have.
                if next == Some('|') {
                    i += 1;
                }
                if let Some(stream) = stream {
                    self.out.toks.push(Tok::Output { stream: Some(stream), append: false });
                }
            }
        }
        Ok(i)
    }

    /// `<` at character `i`: returns the index of its last character.
    fn input(&mut self, i: usize) -> Result<usize, SyntaxError> {
        let number = self.take_stream_number();
        self.end_word(i);
        if let Some(n) = number.filter(|n| n != "0") {
            self.fail(SyntaxError::BadStream(alloc::format!("{n}<")))?;
        }
        match self.ch(i + 1) {
            Some('<') => {
                self.fail(SyntaxError::Unsupported("Reading input from the line itself (<<)"))?;
                Ok(i + 1)
            }
            Some('&') => {
                self.fail(SyntaxError::Unsupported("'<&'"))?;
                Ok(i + 1)
            }
            Some('>') => {
                self.fail(SyntaxError::Unsupported("'<>'"))?;
                Ok(i + 1)
            }
            _ => {
                self.out.toks.push(Tok::Input);
                Ok(i)
            }
        }
    }

    /// Expands `$NAME`, `${NAME}` or `$?` at character `at` into the word
    /// started at `start`; returns the index after it.
    fn variable(&mut self, start: usize, at: usize) -> usize {
        let mut j = at + 1;
        let name: String = if self.ch(j) == Some('{') {
            let from = j + 1;
            let mut k = from;
            while self.ch(k).is_some_and(|c| c != '}') {
                k += 1;
            }
            j = (k + 1).min(self.chars.len());
            self.chars[from.min(self.chars.len())..k.min(self.chars.len())].iter().map(|c| c.1).collect()
        } else if self.ch(j) == Some('?') {
            j += 1;
            "?".into()
        } else {
            let from = j;
            while self.ch(j).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                j += 1;
            }
            self.chars[from..j].iter().map(|c| c.1).collect()
        };
        let written = &self.line[self.pos(at)..self.pos(j)];
        let value = if name.is_empty() {
            Some(String::from("$"))
        } else {
            match self.lx.vars {
                Some(vars) => Some(vars(&name).unwrap_or_default()),
                None => Some(written.to_string()),
            }
        };
        let w = self.word(start);
        w.plain = false;
        if !name.is_empty() {
            w.expanded = true;
        }
        w.text.push_str(&value.unwrap_or_default());
        j
    }
}

/// Splits tokens into connected pipelines of commands.
pub fn parse(toks: Vec<Tok>) -> Result<Vec<(Connector, Pipeline)>, SyntaxError> {
    let mut list = Vec::new();
    let mut conn = Connector::Always;
    let mut pipeline: Pipeline = Vec::new();
    let mut cmd = Command::default();
    let mut it = toks.into_iter().peekable();
    fn finish(cmd: &mut Command, pipeline: &mut Pipeline, sep: &'static str) -> Result<(), SyntaxError> {
        if cmd.words.is_empty() {
            return Err(if cmd.redirects.is_empty() {
                SyntaxError::Unexpected(sep)
            } else {
                SyntaxError::MissingCommand
            });
        }
        pipeline.push(core::mem::take(cmd));
        Ok(())
    }
    while let Some(t) = it.next() {
        match t {
            Tok::Word(w) => cmd.words.push(w),
            Tok::Input | Tok::Output { .. } => {
                let Some(Tok::Word(path)) = it.next_if(|t| matches!(t, Tok::Word(_))) else {
                    return Err(SyntaxError::MissingFileName);
                };
                match t {
                    Tok::Input => cmd.redirects.push(Redirect::Input(path)),
                    Tok::Output { stream: Some(stream), append } => {
                        cmd.redirects.push(Redirect::Output { stream, path, append })
                    }
                    Tok::Output { stream: None, append } => {
                        cmd.redirects.push(Redirect::Output { stream: Stream::Out, path, append });
                        cmd.redirects.push(Redirect::Dup { stream: Stream::Err, to: Stream::Out });
                    }
                    _ => {}
                }
            }
            Tok::Dup { stream, to } => cmd.redirects.push(Redirect::Dup { stream, to }),
            Tok::Pipe => finish(&mut cmd, &mut pipeline, "|")?,
            Tok::PipeBoth => {
                cmd.redirects.push(Redirect::Dup { stream: Stream::Err, to: Stream::Out });
                finish(&mut cmd, &mut pipeline, "|&")?
            }
            Tok::Background => return Err(SyntaxError::Background),
            Tok::Semi | Tok::And | Tok::Or => {
                let sep = match t {
                    Tok::Semi => ";",
                    Tok::And => "&&",
                    _ => "||",
                };
                if cmd.words.is_empty() && cmd.redirects.is_empty() && pipeline.is_empty() && t == Tok::Semi {
                    continue; // an empty statement
                }
                finish(&mut cmd, &mut pipeline, sep)?;
                list.push((conn, core::mem::take(&mut pipeline)));
                conn = match t {
                    Tok::And => Connector::And,
                    Tok::Or => Connector::Or,
                    _ => Connector::Always,
                };
            }
        }
    }
    if !cmd.words.is_empty() || !cmd.redirects.is_empty() {
        finish(&mut cmd, &mut pipeline, "end of line")?;
    } else if !pipeline.is_empty() || conn != Connector::Always {
        return Err(SyntaxError::UnexpectedEnd);
    }
    if !pipeline.is_empty() {
        list.push((conn, pipeline));
    }
    Ok(list)
}

/// The items of a command line's list, checked as a whole first: how each
/// is connected to the one before and its text (with its separator). Each
/// is expanded when its turn comes, so that it sees what the items before
/// it did (`cd /tmp; ls`, `prog; echo $?`).
pub fn split_list<'l>(line: &'l str, home: &str) -> Result<Vec<(Connector, &'l str)>, SyntaxError> {
    let toks = tokenize(line, &Lexer { vars: None, home, lenient: false })?;
    let breaks = toks.breaks.clone();
    parse(toks.toks)?;
    let mut items = Vec::with_capacity(breaks.len() + 1);
    let mut start = (0, Connector::Always);
    for b in breaks {
        items.push((start.1, &line[start.0..b.0]));
        start = b;
    }
    items.push((start.1, &line[start.0..]));
    Ok(items)
}

/// The pipeline of one item of [`split_list`] (its separator left out),
/// expanded now: `None` for an empty item.
pub fn parse_item(text: &str, lx: &Lexer) -> Result<Option<Pipeline>, SyntaxError> {
    let mut toks = tokenize(text, lx)?.toks;
    toks.retain(|t| !matches!(t, Tok::Semi | Tok::And | Tok::Or));
    Ok(parse(toks)?.into_iter().next().map(|(_, p)| p))
}

/// Where one of a command's streams goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    /// The terminal.
    Terminal,
    /// The next command of the pipeline.
    Pipe,
    /// The file at this index of [`Routes::files`].
    File(usize),
}

/// Where a command's input comes from and its output goes, its
/// redirections applied in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routes {
    /// `< FILE` (the last one).
    pub input: Option<Word>,
    pub out: Sink,
    pub err: Sink,
    /// The files to open for writing, in order, and whether to append:
    /// every one is opened (and emptied, unless appended to), as in POSIX
    /// shells, even if no stream ends up there.
    pub files: Vec<(Word, bool)>,
}

/// Where `cmd`'s streams go: by default its output to the next command
/// (or to the terminal for the `last` one), its errors to the terminal.
pub fn route(cmd: &Command, last: bool) -> Routes {
    let mut r = Routes {
        input: None,
        out: if last { Sink::Terminal } else { Sink::Pipe },
        err: Sink::Terminal,
        files: Vec::new(),
    };
    for redirect in &cmd.redirects {
        match redirect {
            Redirect::Input(path) => r.input = Some(path.clone()),
            Redirect::Output { stream, path, append } => {
                r.files.push((path.clone(), *append));
                let sink = Sink::File(r.files.len() - 1);
                match stream {
                    Stream::Out => r.out = sink,
                    Stream::Err => r.err = sink,
                }
            }
            Redirect::Dup { stream: Stream::Err, to: Stream::Out } => r.err = r.out,
            Redirect::Dup { stream: Stream::Out, to: Stream::Err } => r.out = r.err,
            Redirect::Dup { .. } => {}
        }
    }
    r
}

/// Escapes the characters that the tokenizer treats specially.
pub fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if matches!(c, ' ' | '\t' | '\'' | '"' | '\\' | '|' | ';' | '&' | '<' | '>' | '(' | ')' | '$' | '*' | '?' | '#')
        {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The commands of a line as written, without expanding anything and
/// whatever its errors: each command's words (redirection targets left
/// out). For looking at a line, never for running it.
pub fn commands_as_written(line: &str) -> Vec<Vec<Word>> {
    let lx = Lexer { vars: None, home: "~", lenient: true };
    let toks = tokenize(line, &lx).map(|t| t.toks).unwrap_or_default();
    let mut out = Vec::new();
    let mut words = Vec::new();
    let mut target = false;
    for t in toks {
        match t {
            Tok::Word(_) if target => target = false,
            Tok::Word(w) => words.push(w),
            Tok::Input | Tok::Output { .. } => target = true,
            Tok::Dup { .. } => {}
            Tok::Pipe | Tok::PipeBoth | Tok::Semi | Tok::And | Tok::Or | Tok::Background => {
                target = false;
                out.push(core::mem::take(&mut words));
            }
        }
    }
    out.push(words);
    out.retain(|w| !w.is_empty());
    out
}

/// What [`redact`] puts in place of a password.
pub const HIDDEN: &str = "********";

/// A command line as it may be kept (in the history) or read by the voice
/// agent: the password of every `wifi connect SSID PASSWORD` in it, in
/// whichever command of the line it is, is replaced by [`HIDDEN`]. Read as
/// written and whatever its errors (a password whose closing quote is
/// missing is hidden all the same).
pub fn redact(line: &str) -> String {
    let mut spans: Vec<Range<usize>> = Vec::new();
    for words in commands_as_written(line) {
        if words.len() > 3 && words[0].text == "wifi" && words[1].text == "connect" {
            // The password, and anything after it.
            spans.extend(words[3..].iter().map(|w| w.span.clone()));
        }
    }
    spans.sort_by_key(|s| s.start);
    let mut out = String::from(line);
    for span in spans.into_iter().rev() {
        out.replace_range(span, HIDDEN);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn vars(name: &str) -> Option<String> {
        match name {
            "HOME" => Some("/home/user".into()),
            "N" => Some("2".into()),
            "?" => Some("0".into()),
            _ => None,
        }
    }

    fn lexer() -> Lexer<'static> {
        Lexer { vars: Some(&vars), home: "/home/user", lenient: false }
    }

    fn list(line: &str) -> Result<Vec<(Connector, Pipeline)>, SyntaxError> {
        parse(tokenize(line, &lexer())?.toks)
    }

    /// The one command of `line`.
    fn command(line: &str) -> Command {
        let l = list(line).unwrap();
        assert_eq!(l.len(), 1, "{line:?}");
        assert_eq!(l[0].1.len(), 1, "{line:?}");
        l[0].1[0].clone()
    }

    fn words(c: &Command) -> Vec<&str> {
        c.words.iter().map(|w| w.text.as_str()).collect()
    }

    fn path(r: &Redirect) -> &str {
        match r {
            Redirect::Input(p) | Redirect::Output { path: p, .. } => &p.text,
            Redirect::Dup { .. } => "",
        }
    }

    #[test]
    fn words_quotes_and_expansions() {
        let c = command(r#"echo 'a  b' "c $HOME" d\ e ~/x $N${N} $? *.txt "*.md" $"#);
        assert_eq!(words(&c), ["echo", "a  b", "c /home/user", "d e", "/home/user/x", "22", "0", "*.txt", "*.md", "$"]);
        let flags: Vec<(bool, bool)> = c.words.iter().map(|w| (w.glob, w.expanded)).collect();
        assert_eq!(flags[2], (false, true));
        assert_eq!(flags[7], (true, false));
        assert_eq!(flags[8], (false, false));
        // A word's span is where it is written, quotes included.
        let line = r#"cat "my file" x"#;
        let c = command(line);
        assert_eq!(&line[c.words[1].span.clone()], "\"my file\"");
        assert_eq!(&line[c.words[2].span.clone()], "x");
        // A comment at a word's start; `#` inside a word is a character.
        assert_eq!(words(&command("echo a#b # c")), ["echo", "a#b"]);
    }

    #[test]
    fn stream_redirections() {
        // `2>&1` sends errors where the output goes; it is no argument and
        // no file called `&1`.
        let c = command("ls ~/Docs 2>&1");
        assert_eq!(words(&c), ["ls", "/home/user/Docs"]);
        assert_eq!(c.redirects, vec![Redirect::Dup { stream: Stream::Err, to: Stream::Out }]);
        // `2>FILE` redirects the errors, not the output (and `2` is no
        // argument).
        let c = command("ls x 2>/dev/null");
        assert_eq!(words(&c), ["ls", "x"]);
        assert!(matches!(&c.redirects[..], [Redirect::Output { stream: Stream::Err, append: false, .. }]));
        assert_eq!(path(&c.redirects[0]), "/dev/null");
        let c = command("make 2>> log >&2");
        assert!(matches!(c.redirects[0], Redirect::Output { stream: Stream::Err, append: true, .. }));
        assert_eq!(c.redirects[1], Redirect::Dup { stream: Stream::Out, to: Stream::Err });
        // Both streams.
        let c = command("build &> all.txt");
        assert_eq!(words(&c), ["build"]);
        assert!(matches!(c.redirects[0], Redirect::Output { stream: Stream::Out, append: false, .. }));
        assert_eq!(c.redirects[1], Redirect::Dup { stream: Stream::Err, to: Stream::Out });
        let c = command("build &>> all.txt");
        assert!(matches!(c.redirects[0], Redirect::Output { stream: Stream::Out, append: true, .. }));
        // Digits are a stream only on their own and unquoted.
        let c = command("echo a2>f '2'>g");
        assert_eq!(words(&c), ["echo", "a2", "2"]);
        assert_eq!(c.redirects.len(), 2);
        assert!(matches!(c.redirects[0], Redirect::Output { stream: Stream::Out, .. }));
        // Input, and `0<`.
        let c = command("wc < a.txt 0<b.txt");
        assert_eq!((path(&c.redirects[0]), path(&c.redirects[1])), ("a.txt", "b.txt"));
        // `|&` pipes the errors too.
        let l = list("make |& grep error").unwrap();
        assert_eq!(l[0].1[0].redirects, vec![Redirect::Dup { stream: Stream::Err, to: Stream::Out }]);
    }

    #[test]
    fn what_vsh_does_not_do() {
        assert_eq!(list("sleep 5 &"), Err(SyntaxError::Background));
        assert_eq!(list("a & b"), Err(SyntaxError::Background));
        assert_eq!(list("curl http://x/?a=1&b=2"), Err(SyntaxError::Background));
        assert_eq!(words(&command("echo 'a&b' a\\&b")), ["echo", "a&b", "a&b"]);
        assert_eq!(list("ls 3>f"), Err(SyntaxError::BadStream("3>".into())));
        assert_eq!(list("ls 1<f"), Err(SyntaxError::BadStream("1<".into())));
        assert!(matches!(list("cat << EOF"), Err(SyntaxError::Unsupported(_))));
        assert!(matches!(list("ls >&3"), Err(SyntaxError::Unsupported(_))));
        assert_eq!(list("echo 'abc"), Err(SyntaxError::UnterminatedQuote('\'')));
        assert_eq!(list("echo \"abc"), Err(SyntaxError::UnterminatedQuote('"')));
    }

    #[test]
    fn lists_and_errors() {
        let l = list("a | b && c || d; e").unwrap();
        let shape: Vec<(Connector, usize)> = l.iter().map(|(c, p)| (*c, p.len())).collect();
        assert_eq!(shape, [(Connector::Always, 2), (Connector::And, 1), (Connector::Or, 1), (Connector::Always, 1)]);
        assert!(list("; ls ;").is_ok());
        assert_eq!(list("ls |"), Err(SyntaxError::UnexpectedEnd));
        assert_eq!(list("ls &&"), Err(SyntaxError::UnexpectedEnd));
        assert_eq!(list("| ls"), Err(SyntaxError::Unexpected("|")));
        assert_eq!(list("ls >"), Err(SyntaxError::MissingFileName));
        assert_eq!(list("ls > | x"), Err(SyntaxError::MissingFileName));
        // A redirection without a command is an error, not the next
        // command's redirection.
        assert_eq!(list("> f; ls"), Err(SyntaxError::MissingCommand));
        assert_eq!(list("> f"), Err(SyntaxError::MissingCommand));
    }

    #[test]
    fn list_items() {
        let items = split_list("cd /tmp; ls $HOME && echo 'a;b' || x", "/home/user").unwrap();
        // Each item with how it follows the one before, and its separator.
        assert_eq!(
            items,
            [
                (Connector::Always, "cd /tmp;"),
                (Connector::Always, " ls $HOME &&"),
                (Connector::And, " echo 'a;b' ||"),
                (Connector::Or, " x"),
            ]
        );
        assert!(split_list("ls &", "/").is_err());
        // Each item parses without its separator, its variables expanded.
        let words_of = |item: &str| -> Vec<String> {
            let p = parse_item(item, &lexer()).unwrap().unwrap();
            p[0].words.iter().map(|w| w.text.clone()).collect()
        };
        assert_eq!(words_of(" ls $HOME &&"), ["ls", "/home/user"]);
        assert_eq!(words_of("cd /tmp;"), ["cd", "/tmp"]);
        assert_eq!(words_of(" echo 'a;b' ||"), ["echo", "a;b"]);
        assert_eq!(parse_item(" ;", &lexer()), Ok(None));
    }

    #[test]
    fn routes() {
        let r = route(&command("cmd > log 2>&1"), true);
        assert_eq!((r.out, r.err), (Sink::File(0), Sink::File(0)));
        let r = route(&command("cmd 2>&1 > log"), true);
        assert_eq!((r.out, r.err), (Sink::File(0), Sink::Terminal));
        let r = route(&command("cmd > a > b"), true);
        assert_eq!(r.out, Sink::File(1));
        assert_eq!(r.files.iter().map(|(w, _)| w.text.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        let r = route(&command("cmd 2>&1"), false);
        assert_eq!((r.out, r.err), (Sink::Pipe, Sink::Pipe));
        let r = route(&command("cmd < in.txt"), true);
        assert_eq!((r.input.map(|w| w.text), r.out, r.err), (Some("in.txt".into()), Sink::Terminal, Sink::Terminal));
    }

    #[test]
    fn passwords_hidden_wherever_they_are() {
        assert_eq!(redact("wifi connect Home pw"), "wifi connect Home ********");
        assert_eq!(redact("wifi connect Home pw; ls"), "wifi connect Home ********; ls");
        assert_eq!(redact("cd ~ && wifi connect \"My Net\" 'my pass'"), "cd ~ && wifi connect \"My Net\" ********");
        assert_eq!(redact("wifi connect Net pw | cat"), "wifi connect Net ******** | cat");
        assert_eq!(redact("wifi connect Net pw > out"), "wifi connect Net ******** > out");
        // A line that does not parse is read all the same.
        assert_eq!(redact("wifi connect Net \"pass word"), "wifi connect Net ********");
        assert_eq!(redact("wifi connect Net pw &"), "wifi connect Net ******** &");
        assert_eq!(redact("wifi connect Net #secret"), "wifi connect Net ********");
        // Not a password.
        assert_eq!(redact("wifi connect Open"), "wifi connect Open");
        assert_eq!(redact("echo wifi connect a b"), "echo wifi connect a b");
        assert_eq!(redact("ls -l"), "ls -l");
    }
}
