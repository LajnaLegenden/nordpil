//! Vim keys for the code editor: modes, counts, motions, operators and text
//! objects, as a state machine over the editor's text and cursor. It knows
//! nothing of egui, so the tests drive it with plain strings.
//!
//! In insert mode the editor types as usual; only Escape and undo reach here.

use std::ops::Range;

/// Counts above this are clamped, so `99999999x` can't stall a frame.
const MAX_COUNT: usize = 9999;
const MAX_UNDO: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Visual => "VISUAL",
            Self::VisualLine => "V-LINE",
        }
    }

    pub fn is_visual(self) -> bool {
        matches!(self, Self::Visual | Self::VisualLine)
    }
}

/// A key as vim sees it. The editor turns egui events into these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Escape,
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    /// Ctrl+Z.
    Undo,
    /// Ctrl+R, Ctrl+Y or Ctrl+Shift+Z.
    Redo,
}

/// What y, d, c and x store and p puts back. One for the whole app.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Register {
    pub text: String,
    /// Whole lines, which p puts below the cursor's line.
    pub linewise: bool,
}

/// What a key works on: the editor's text, its cursor as a char index, and
/// the register every editor shares.
pub struct Edit<'a> {
    pub text: &'a mut String,
    pub cursor: usize,
    pub register: &'a mut Register,
}

type Snapshot = (String, usize);

#[derive(Debug, Clone, Default)]
pub struct Vim {
    mode: Mode,
    multiline: bool,
    /// Keys of a command that isn't complete yet, like `d2` or `f`.
    pending: Vec<Key>,
    /// The other end of the visual selection.
    anchor: usize,
    /// The column j and k aim for; `usize::MAX` after `$`.
    column: Option<usize>,
    last_find: Option<Find>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The text before the change in progress: an insert session, or a `c`
    /// and what is typed after it.
    change_start: Option<Snapshot>,
}

impl Vim {
    pub fn new(multiline: bool) -> Self {
        Self {
            multiline,
            ..Self::default()
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Goes to insert mode without a key, e.g. when the filter bar gains focus.
    pub fn start_insert(&mut self, text: &str, cursor: usize) {
        self.pending.clear();
        self.mode = Mode::Insert;
        self.change_start
            .get_or_insert_with(|| (text.to_owned(), cursor));
    }

    /// Visual mode from `anchor`, e.g. after a mouse drag. The editor keeps
    /// the cursor at the other end.
    pub fn select(&mut self, anchor: usize) {
        self.pending.clear();
        self.mode = Mode::Visual;
        self.anchor = anchor;
    }

    /// Makes a change made outside vim, like a paste, one undo step.
    pub fn changed_outside(&mut self, before: &str, cursor: usize) {
        self.push_undo((before.to_owned(), cursor));
    }

    /// Back to normal mode, e.g. after a click ends a visual selection.
    pub fn normal(&mut self) {
        self.pending.clear();
        if self.mode != Mode::Insert {
            self.mode = Mode::Normal;
        }
    }

    /// The cursor kept on a character, as normal and visual mode want it.
    pub fn clamp(&self, text: &str, cursor: usize) -> usize {
        let chars: Vec<char> = text.chars().collect();
        match self.mode {
            Mode::Insert => cursor.min(chars.len()),
            _ => clamp_normal(&chars, cursor),
        }
    }

    /// The visual selection as chars, end exclusive.
    pub fn selection(&self, text: &str, cursor: usize) -> Option<Range<usize>> {
        if !self.mode.is_visual() {
            return None;
        }
        let chars: Vec<char> = text.chars().collect();
        let span = self.visual_span(&chars, cursor);
        let range = span.range;
        let end = if span.linewise {
            (range.end + 1).min(chars.len())
        } else {
            range.end
        };
        Some(range.start..end)
    }

    pub fn key(&mut self, key: Key, edit: &mut Edit<'_>) {
        if self.mode != Mode::Insert {
            edit.cursor = self.clamp(edit.text, edit.cursor);
        }
        match (self.mode, key) {
            (Mode::Insert, Key::Escape) => self.leave_insert(edit),
            (Mode::Insert, Key::Undo | Key::Redo) => {
                self.commit(edit.text);
                self.history(key == Key::Redo, 1, edit);
                self.change_start = Some((edit.text.clone(), edit.cursor));
            }
            (Mode::Insert, _) => {}
            (Mode::Visual | Mode::VisualLine, Key::Escape) => {
                self.pending.clear();
                self.mode = Mode::Normal;
            }
            (Mode::Normal, Key::Escape) => self.pending.clear(),
            _ => {
                self.pending.push(key);
                self.run_pending(edit);
            }
        }
    }

    fn leave_insert(&mut self, edit: &mut Edit<'_>) {
        self.commit(edit.text);
        self.mode = Mode::Normal;
        self.column = None;
        let chars: Vec<char> = edit.text.chars().collect();
        let cursor = edit.cursor.min(chars.len());
        if cursor > line_start(&chars, cursor) {
            edit.cursor = cursor - 1;
        }
        edit.cursor = clamp_normal(&chars, edit.cursor);
    }

    /// Ends the change in progress, as one undo step if it changed anything.
    fn commit(&mut self, text: &str) {
        if let Some(start) = self.change_start.take()
            && start.0 != text
        {
            self.push_undo(start);
        }
    }

    fn push_undo(&mut self, snapshot: Snapshot) {
        self.undo.push(snapshot);
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn history(&mut self, redo: bool, count: usize, edit: &mut Edit<'_>) {
        let (from, to) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        for _ in 0..count {
            let Some((text, cursor)) = from.pop() else {
                break;
            };
            to.push((std::mem::replace(edit.text, text), edit.cursor));
            edit.cursor = cursor;
        }
        edit.cursor = self.clamp(edit.text, edit.cursor);
    }

    fn run_pending(&mut self, edit: &mut Edit<'_>) {
        let parsed = match parse(&self.pending, self.mode.is_visual()) {
            Ok(parsed) => parsed,
            Err(Need::More) => return,
            Err(Need::Invalid) => {
                self.pending.clear();
                return;
            }
        };
        self.pending.clear();
        let count = parsed.count.unwrap_or(1);
        match parsed.command {
            Command::Undo => return self.history(false, count, edit),
            Command::Redo => return self.history(true, count, edit),
            _ => {}
        }
        let before = (edit.text.clone(), edit.cursor);
        let chars: Vec<char> = edit.text.chars().collect();
        let mut work = Work {
            cursor: clamp_normal(&chars, edit.cursor),
            chars,
            register: &mut *edit.register,
        };
        self.run(parsed, &mut work);
        let text: String = work.chars.iter().collect();
        edit.cursor = match self.mode {
            Mode::Insert => work.cursor.min(work.chars.len()),
            _ => clamp_normal(&work.chars, work.cursor),
        };
        self.anchor = self.anchor.min(work.chars.len());
        if self.mode == Mode::Insert {
            self.change_start = Some(before);
        } else if text != before.0 {
            self.push_undo(before);
        }
        *edit.text = text;
    }

    fn run(&mut self, parsed: Parsed, work: &mut Work<'_>) {
        let Parsed { command, count } = parsed;
        let n = count.unwrap_or(1);
        match command {
            Command::Move(motion) => self.move_cursor(Movement { motion, count }, work),
            Command::Operate(operator, target) => {
                if let Some(span) = self.span(operator, target, count, work) {
                    self.apply(operator, span, work);
                }
            }
            Command::Insert(at) => self.insert(at, work),
            Command::Put { before } => self.put(before, n, work),
            Command::Replace(c) => replace_chars(work, n, c),
            Command::Join => self.join(n, work),
            Command::ToggleCase => toggle_case(work, n),
            Command::Visual(mode) => self.toggle_visual(mode, work.cursor),
            Command::SwapEnds => std::mem::swap(&mut self.anchor, &mut work.cursor),
            Command::SelectObject(object) => self.select_object(object, n, work),
            Command::VisualOperate(operator) => {
                let span = self.visual_span(&work.chars, work.cursor);
                self.mode = Mode::Normal;
                self.apply(operator, span, work);
            }
            Command::VisualPut { swap } => self.visual_put(swap, work),
            Command::Undo | Command::Redo => {}
        }
    }

    fn move_cursor(&mut self, movement: Movement, work: &mut Work<'_>) {
        let Some(jump) = self.jump(movement, work) else {
            return;
        };
        let column = self.column(work);
        work.cursor = jump.to;
        self.column = match movement.motion {
            Motion::Up | Motion::Down => Some(column),
            Motion::LineEnd => Some(usize::MAX),
            _ => None,
        };
    }

    fn column(&self, work: &Work<'_>) -> usize {
        self.column
            .unwrap_or_else(|| work.cursor - line_start(&work.chars, work.cursor))
    }

    /// Where a motion goes. `None` when it can't, e.g. `f` without a match.
    fn jump(&mut self, movement: Movement, work: &Work<'_>) -> Option<Jump> {
        let Movement { motion, count } = movement;
        let chars = &work.chars;
        let at = work.cursor;
        let n = count.unwrap_or(1);
        let last_line = line_count(chars) - 1;
        let jump = match motion {
            Motion::Left => Jump::exclusive(at.saturating_sub(n).max(line_start(chars, at))),
            Motion::Right => Jump::exclusive((at + n).min(line_end(chars, at))),
            Motion::Up | Motion::Down => {
                let line = line_index(chars, at);
                let target = if motion == Motion::Up {
                    line.saturating_sub(n)
                } else {
                    (line + n).min(last_line)
                };
                if target == line {
                    return None;
                }
                Jump::linewise(at_column(chars, target, self.column(work)))
            }
            Motion::NextLine => {
                let line = line_index(chars, at) + n;
                if line > last_line {
                    return None;
                }
                Jump::linewise(first_non_blank(chars, nth_line_start(chars, line)))
            }
            Motion::WordStart(big) => Jump::exclusive(repeat(n, at, |i| word_start(chars, i, big))),
            Motion::WordBack(big) => Jump::exclusive(repeat(n, at, |i| word_back(chars, i, big))),
            Motion::WordEnd(big) => Jump::inclusive(repeat(n, at, |i| word_end(chars, i, big))),
            Motion::LineStart => Jump::exclusive(line_start(chars, at)),
            Motion::FirstNonBlank => Jump::exclusive(first_non_blank(chars, at)),
            Motion::LineEnd => {
                let line = (line_index(chars, at) + n - 1).min(last_line);
                Jump::exclusive(line_end(chars, nth_line_start(chars, line)))
            }
            Motion::FirstLine | Motion::LastLine => {
                let default = if motion == Motion::FirstLine {
                    1
                } else {
                    last_line + 1
                };
                let line = (count.unwrap_or(default) - 1).min(last_line);
                Jump::linewise(first_non_blank(chars, nth_line_start(chars, line)))
            }
            Motion::Find(find) => {
                self.last_find = Some(find);
                find_in_line(chars, at, find, n, false)?
            }
            Motion::RepeatFind { reverse } => {
                let find = self.last_find?;
                let find = if reverse { find.reversed() } else { find };
                find_in_line(chars, at, find, n, true)?
            }
            Motion::MatchPair => Jump::inclusive(match_pair(chars, at)?),
        };
        Some(jump)
    }

    /// The text an operator covers.
    fn span(
        &mut self,
        operator: Operator,
        target: Target,
        count: Option<usize>,
        work: &Work<'_>,
    ) -> Option<Span> {
        let chars = &work.chars;
        let at = work.cursor;
        let n = count.unwrap_or(1);
        match target {
            Target::Lines => {
                let last = (line_index(chars, at) + n - 1).min(line_count(chars) - 1);
                Some(Span::lines(
                    line_start(chars, at),
                    line_end(chars, nth_line_start(chars, last)),
                ))
            }
            Target::Object(object) => text_object(chars, at, object, n),
            // cw changes to the word's end, not up to the next word.
            Target::Motion(Motion::WordStart(big))
                if operator == Operator::Change
                    && chars.get(at).is_some_and(|c| !c.is_whitespace()) =>
            {
                Some(Span::chars(at..change_word_end(chars, at, n, big) + 1))
            }
            Target::Motion(motion) => {
                let jump = self.jump(Movement { motion, count }, work)?;
                let (from, to) = (at.min(jump.to), at.max(jump.to));
                let span = match jump.kind {
                    Kind::Exclusive if matches!(motion, Motion::WordStart(_)) => {
                        Span::chars(from..word_operator_end(chars, from, to))
                    }
                    Kind::Exclusive => Span::chars(from..to),
                    Kind::Inclusive => Span::chars(from..(to + 1).min(chars.len())),
                    Kind::Linewise => Span::lines(line_start(chars, from), line_end(chars, to)),
                };
                Some(span)
            }
        }
    }

    fn apply(&mut self, operator: Operator, span: Span, work: &mut Work<'_>) {
        match operator {
            Operator::Yank => {
                work.yank(&span);
                let start = span.range.start;
                if !span.linewise {
                    work.cursor = work.cursor.min(start);
                } else if start < line_start(&work.chars, work.cursor) {
                    work.cursor = start;
                }
            }
            Operator::Delete => work.delete(span),
            Operator::Change => {
                work.change(span);
                self.mode = Mode::Insert;
            }
        }
    }

    fn insert(&mut self, at: InsertAt, work: &mut Work<'_>) {
        let chars = &work.chars;
        let cursor = work.cursor;
        let start = line_start(chars, cursor);
        let end = line_end(chars, cursor);
        let indent = indent(chars, start);
        match at {
            InsertAt::Before => {}
            InsertAt::After => work.cursor = (cursor + 1).min(end),
            InsertAt::LineStart => work.cursor = first_non_blank(chars, cursor),
            InsertAt::LineEnd => work.cursor = end,
            InsertAt::LineBelow | InsertAt::LineAbove if !self.multiline => return,
            InsertAt::LineBelow => {
                work.splice(end..end, &format!("\n{indent}"));
                work.cursor = end + 1 + indent.chars().count();
            }
            InsertAt::LineAbove => {
                work.splice(start..start, &format!("{indent}\n"));
                work.cursor = start + indent.chars().count();
            }
        }
        self.mode = Mode::Insert;
    }

    fn put(&self, before: bool, count: usize, work: &mut Work<'_>) {
        let Register { text, linewise } = work.register.clone();
        if text.is_empty() {
            return;
        }
        if !self.multiline {
            return put_chars(work, &one_line(&text).repeat(count), before);
        }
        if linewise {
            put_lines(work, &text.repeat(count), before);
        } else {
            put_chars(work, &text.repeat(count), before);
        }
    }

    fn join(&self, count: usize, work: &mut Work<'_>) {
        if !self.multiline {
            return;
        }
        for _ in 1..count.max(2) {
            let chars = &work.chars;
            let start = line_start(chars, work.cursor);
            let end = line_end(chars, work.cursor);
            if end >= chars.len() {
                return;
            }
            let next = (end + 1..chars.len())
                .find(|&i| !is_blank(chars[i]))
                .unwrap_or(chars.len());
            let next_empty = chars.get(next).is_none_or(|&c| c == '\n' || c == ')');
            let ends_blank = end > start && is_blank(chars[end - 1]);
            let space = !(next_empty || ends_blank || end == start);
            work.splice(end..next, if space { " " } else { "" });
            work.cursor = end;
        }
    }

    fn toggle_visual(&mut self, mode: Mode, cursor: usize) {
        if self.mode == mode {
            self.mode = Mode::Normal;
            return;
        }
        if !self.mode.is_visual() {
            self.anchor = cursor;
        }
        self.mode = mode;
    }

    fn select_object(&mut self, object: TextObject, count: usize, work: &mut Work<'_>) {
        let Some(span) = text_object(&work.chars, work.cursor, object, count) else {
            return;
        };
        let Range { start, end } = span.range;
        if start >= end {
            return;
        }
        self.anchor = start;
        work.cursor = end - 1;
    }

    /// The selection, as an operator covers it.
    fn visual_span(&self, chars: &[char], cursor: usize) -> Span {
        let (from, to) = (self.anchor.min(cursor), self.anchor.max(cursor));
        if self.mode == Mode::VisualLine {
            Span::lines(line_start(chars, from), line_end(chars, to))
        } else {
            Span::chars(from.min(chars.len())..(to + 1).min(chars.len()))
        }
    }

    /// Replaces the selection with the register. `p` puts what it replaced
    /// in the register; `P` keeps the register.
    fn visual_put(&mut self, swap: bool, work: &mut Work<'_>) {
        let register = work.register.clone();
        let span = self.visual_span(&work.chars, work.cursor);
        self.mode = Mode::Normal;
        if register.text.is_empty() {
            return;
        }
        let replaced = work.slice(span.range.clone());
        let text = match (span.linewise, register.linewise) {
            _ if !self.multiline => one_line(&register.text),
            (false, true) => format!("\n{}", register.text),
            (true, true) => register.text.trim_end_matches('\n').to_owned(),
            (_, false) => register.text.clone(),
        };
        let start = span.range.start;
        work.splice(span.range, &text);
        work.cursor = match (span.linewise, text.starts_with('\n')) {
            (true, _) => first_non_blank(&work.chars, start),
            (false, true) => first_non_blank(&work.chars, start + 1),
            _ if text.contains('\n') => start,
            _ => start + text.chars().count().saturating_sub(1),
        };
        if swap {
            let linewise = span.linewise;
            let text = if linewise { replaced + "\n" } else { replaced };
            *work.register = Register { text, linewise };
        }
    }
}

/// The editor text while a command runs, as chars.
struct Work<'a> {
    chars: Vec<char>,
    cursor: usize,
    register: &'a mut Register,
}

impl Work<'_> {
    fn slice(&self, range: Range<usize>) -> String {
        self.chars[range].iter().collect()
    }

    fn splice(&mut self, range: Range<usize>, with: &str) {
        self.chars.splice(range, with.chars());
    }

    fn yank(&mut self, span: &Span) {
        let mut text = self.slice(span.range.clone());
        if span.linewise {
            text.push('\n');
        }
        *self.register = Register {
            text,
            linewise: span.linewise,
        };
    }

    fn delete(&mut self, span: Span) {
        self.yank(&span);
        let Range { start, end } = span.range;
        if !span.linewise {
            self.chars.drain(start..end);
            self.cursor = start;
            return;
        }
        // Takes the line's newline, or the one before the last line.
        let (from, to) = if end < self.chars.len() {
            (start, end + 1)
        } else {
            (start.saturating_sub(1), end)
        };
        self.chars.drain(from..to);
        self.cursor = first_non_blank(&self.chars, from);
    }

    /// Deletes the span for typing in its place. Lines keep their indent.
    fn change(&mut self, span: Span) {
        self.yank(&span);
        let Range { start, end } = span.range;
        let indent = if span.linewise {
            indent(&self.chars, start)
        } else {
            String::new()
        };
        self.splice(start..end, &indent);
        self.cursor = start + indent.chars().count();
    }
}

fn put_chars(work: &mut Work<'_>, text: &str, before: bool) {
    if text.is_empty() {
        return;
    }
    let on_char = work.chars.get(work.cursor).is_some_and(|&c| c != '\n');
    let at = if before || !on_char {
        work.cursor
    } else {
        work.cursor + 1
    };
    work.splice(at..at, text);
    work.cursor = if text.contains('\n') {
        at
    } else {
        at + text.chars().count() - 1
    };
}

/// Puts whole lines, `text` ending in a newline, below or above the cursor's line.
fn put_lines(work: &mut Work<'_>, text: &str, before: bool) {
    let at = if before {
        line_start(&work.chars, work.cursor)
    } else {
        line_end(&work.chars, work.cursor)
    };
    let start = if before {
        work.splice(at..at, text);
        at
    } else if at < work.chars.len() {
        work.splice(at + 1..at + 1, text);
        at + 1
    } else {
        let text = format!("\n{}", text.trim_end_matches('\n'));
        work.splice(at..at, &text);
        at + 1
    };
    work.cursor = first_non_blank(&work.chars, start);
}

fn replace_chars(work: &mut Work<'_>, count: usize, c: char) {
    let end = line_end(&work.chars, work.cursor);
    if work.cursor + count > end {
        return;
    }
    for at in work.cursor..work.cursor + count {
        work.chars[at] = c;
    }
    work.cursor += count - 1;
}

fn toggle_case(work: &mut Work<'_>, count: usize) {
    let end = (work.cursor + count).min(line_end(&work.chars, work.cursor));
    for at in work.cursor..end {
        work.chars[at] = toggled(work.chars[at]);
    }
    work.cursor = end;
}

/// The other case, unless it takes more than one char, like `ß`.
fn toggled(c: char) -> char {
    let other: Vec<char> = if c.is_lowercase() {
        c.to_uppercase().collect()
    } else {
        c.to_lowercase().collect()
    };
    match other[..] {
        [one] => one,
        _ => c,
    }
}

/// Register text for a single-line editor: lines joined by spaces.
fn one_line(text: &str) -> String {
    text.trim_end_matches('\n').replace('\n', " ")
}

fn repeat(count: usize, start: usize, step: impl Fn(usize) -> usize) -> usize {
    (0..count).fold(start, |at, _| step(at))
}

// Parsing ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operator {
    Delete,
    Change,
    Yank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FindKind {
    /// `f`
    To,
    /// `t`
    Till,
    /// `F`
    BackTo,
    /// `T`
    BackTill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Find {
    kind: FindKind,
    c: char,
}

impl Find {
    fn reversed(self) -> Self {
        let kind = match self.kind {
            FindKind::To => FindKind::BackTo,
            FindKind::Till => FindKind::BackTill,
            FindKind::BackTo => FindKind::To,
            FindKind::BackTill => FindKind::Till,
        };
        Self { kind, ..self }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// Enter: the first non-blank of the next line.
    NextLine,
    /// `w`, or `W` when big.
    WordStart(bool),
    WordBack(bool),
    WordEnd(bool),
    LineStart,
    FirstNonBlank,
    LineEnd,
    /// `gg`, or the line its count names.
    FirstLine,
    /// `G`, or the line its count names.
    LastLine,
    Find(Find),
    RepeatFind {
        reverse: bool,
    },
    MatchPair,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Movement {
    motion: Motion,
    count: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Object {
    Word { big: bool },
    Quote(char),
    Bracket(char, char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextObject {
    object: Object,
    around: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Motion(Motion),
    Object(TextObject),
    /// `dd`, `cc`, `yy`.
    Lines,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InsertAt {
    Before,
    After,
    LineStart,
    LineEnd,
    LineBelow,
    LineAbove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Move(Motion),
    Operate(Operator, Target),
    Insert(InsertAt),
    Put {
        before: bool,
    },
    Replace(char),
    Join,
    ToggleCase,
    Undo,
    Redo,
    /// `v` or `V`: start, switch or leave visual mode.
    Visual(Mode),
    SwapEnds,
    SelectObject(TextObject),
    VisualOperate(Operator),
    VisualPut {
        swap: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Parsed {
    command: Command,
    count: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
enum Need {
    More,
    Invalid,
}

struct Keys<'a> {
    keys: &'a [Key],
    at: usize,
}

impl Keys<'_> {
    fn next(&mut self) -> Result<Key, Need> {
        let key = self.keys.get(self.at).copied().ok_or(Need::More)?;
        self.at += 1;
        Ok(key)
    }

    fn char(&mut self) -> Result<char, Need> {
        match self.next()? {
            Key::Char(c) => Ok(c),
            _ => Err(Need::Invalid),
        }
    }

    /// A count, unless the next key is a `0` motion.
    fn count(&mut self) -> Option<usize> {
        let mut count: Option<usize> = None;
        while let Some(&Key::Char(c)) = self.keys.get(self.at) {
            let Some(digit) = c.to_digit(10) else {
                break;
            };
            if count.is_none() && digit == 0 {
                break;
            }
            count = Some((count.unwrap_or(0) * 10 + digit as usize).min(MAX_COUNT));
            self.at += 1;
        }
        count
    }
}

fn parse(keys: &[Key], visual: bool) -> Result<Parsed, Need> {
    let mut keys = Keys { keys, at: 0 };
    if visual {
        parse_visual(&mut keys)
    } else {
        parse_normal(&mut keys)
    }
}

fn parse_normal(keys: &mut Keys<'_>) -> Result<Parsed, Need> {
    let count = keys.count();
    let key = keys.next()?;
    let operate = |operator, motion| Command::Operate(operator, Target::Motion(motion));
    let command = match key {
        Key::Char(c @ ('d' | 'c' | 'y')) => {
            let inner = keys.count();
            let target = match keys.next()? {
                Key::Char(same) if same == c => Target::Lines,
                Key::Char(around @ ('i' | 'a')) => {
                    Target::Object(text_object_key(around == 'a', keys.char()?)?)
                }
                key => Target::Motion(motion(key, keys)?),
            };
            let count = match (count, inner) {
                (None, None) => None,
                (a, b) => Some((a.unwrap_or(1) * b.unwrap_or(1)).min(MAX_COUNT)),
            };
            let command = Command::Operate(operator(c), target);
            return Ok(Parsed { command, count });
        }
        Key::Char('x') | Key::Delete => operate(Operator::Delete, Motion::Right),
        Key::Char('X') => operate(Operator::Delete, Motion::Left),
        Key::Char('s') => operate(Operator::Change, Motion::Right),
        Key::Char('S') => Command::Operate(Operator::Change, Target::Lines),
        Key::Char('D') => operate(Operator::Delete, Motion::LineEnd),
        Key::Char('C') => operate(Operator::Change, Motion::LineEnd),
        Key::Char('Y') => operate(Operator::Yank, Motion::LineEnd),
        Key::Char('i') => Command::Insert(InsertAt::Before),
        Key::Char('a') => Command::Insert(InsertAt::After),
        Key::Char('I') => Command::Insert(InsertAt::LineStart),
        Key::Char('A') => Command::Insert(InsertAt::LineEnd),
        Key::Char('o') => Command::Insert(InsertAt::LineBelow),
        Key::Char('O') => Command::Insert(InsertAt::LineAbove),
        Key::Char('p') => Command::Put { before: false },
        Key::Char('P') => Command::Put { before: true },
        Key::Char('r') => Command::Replace(keys.char()?),
        Key::Char('J') => Command::Join,
        Key::Char('~') => Command::ToggleCase,
        Key::Char('u') | Key::Undo => Command::Undo,
        Key::Redo => Command::Redo,
        Key::Char('v') => Command::Visual(Mode::Visual),
        Key::Char('V') => Command::Visual(Mode::VisualLine),
        key => Command::Move(motion(key, keys)?),
    };
    Ok(Parsed { command, count })
}

fn parse_visual(keys: &mut Keys<'_>) -> Result<Parsed, Need> {
    let count = keys.count();
    let command = match keys.next()? {
        Key::Char('o') => Command::SwapEnds,
        Key::Char('d' | 'x') | Key::Delete => Command::VisualOperate(Operator::Delete),
        Key::Char('c' | 's') => Command::VisualOperate(Operator::Change),
        Key::Char('y') => Command::VisualOperate(Operator::Yank),
        Key::Char('p') => Command::VisualPut { swap: true },
        Key::Char('P') => Command::VisualPut { swap: false },
        Key::Char('v') => Command::Visual(Mode::Visual),
        Key::Char('V') => Command::Visual(Mode::VisualLine),
        Key::Char(around @ ('i' | 'a')) => {
            Command::SelectObject(text_object_key(around == 'a', keys.char()?)?)
        }
        key => Command::Move(motion(key, keys)?),
    };
    Ok(Parsed { command, count })
}

fn operator(c: char) -> Operator {
    match c {
        'd' => Operator::Delete,
        'c' => Operator::Change,
        _ => Operator::Yank,
    }
}

fn motion(key: Key, keys: &mut Keys<'_>) -> Result<Motion, Need> {
    let find = |kind, keys: &mut Keys<'_>| -> Result<Motion, Need> {
        Ok(Motion::Find(Find {
            kind,
            c: keys.char()?,
        }))
    };
    let motion = match key {
        Key::Char('h') | Key::Left | Key::Backspace => Motion::Left,
        Key::Char('l') | Key::Right => Motion::Right,
        Key::Char('k') | Key::Up => Motion::Up,
        Key::Char('j') | Key::Down => Motion::Down,
        Key::Enter => Motion::NextLine,
        Key::Char('w') => Motion::WordStart(false),
        Key::Char('W') => Motion::WordStart(true),
        Key::Char('b') => Motion::WordBack(false),
        Key::Char('B') => Motion::WordBack(true),
        Key::Char('e') => Motion::WordEnd(false),
        Key::Char('E') => Motion::WordEnd(true),
        Key::Char('0') | Key::Home => Motion::LineStart,
        Key::Char('^') => Motion::FirstNonBlank,
        Key::Char('$') | Key::End => Motion::LineEnd,
        Key::Char('g') => match keys.next()? {
            Key::Char('g') => Motion::FirstLine,
            _ => return Err(Need::Invalid),
        },
        Key::Char('G') => Motion::LastLine,
        Key::Char('f') => find(FindKind::To, keys)?,
        Key::Char('t') => find(FindKind::Till, keys)?,
        Key::Char('F') => find(FindKind::BackTo, keys)?,
        Key::Char('T') => find(FindKind::BackTill, keys)?,
        Key::Char(';') => Motion::RepeatFind { reverse: false },
        Key::Char(',') => Motion::RepeatFind { reverse: true },
        Key::Char('%') => Motion::MatchPair,
        _ => return Err(Need::Invalid),
    };
    Ok(motion)
}

fn text_object_key(around: bool, c: char) -> Result<TextObject, Need> {
    let object = match c {
        'w' => Object::Word { big: false },
        'W' => Object::Word { big: true },
        '"' | '\'' | '`' => Object::Quote(c),
        '(' | ')' | 'b' => Object::Bracket('(', ')'),
        '[' | ']' => Object::Bracket('[', ']'),
        '{' | '}' | 'B' => Object::Bracket('{', '}'),
        _ => return Err(Need::Invalid),
    };
    Ok(TextObject { object, around })
}

// Motions and objects ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Exclusive,
    Inclusive,
    Linewise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Jump {
    to: usize,
    kind: Kind,
}

impl Jump {
    fn exclusive(to: usize) -> Self {
        Self {
            to,
            kind: Kind::Exclusive,
        }
    }

    fn inclusive(to: usize) -> Self {
        Self {
            to,
            kind: Kind::Inclusive,
        }
    }

    fn linewise(to: usize) -> Self {
        Self {
            to,
            kind: Kind::Linewise,
        }
    }
}

/// Text an operator covers. Lines run from the first line's start to the
/// last line's end, without its newline.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    range: Range<usize>,
    linewise: bool,
}

impl Span {
    fn chars(range: Range<usize>) -> Self {
        Self {
            range,
            linewise: false,
        }
    }

    fn lines(start: usize, end: usize) -> Self {
        Self {
            range: start..end,
            linewise: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Blank,
    Punctuation,
    Word,
}

fn class(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punctuation
    }
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn line_start(chars: &[char], at: usize) -> usize {
    chars[..at.min(chars.len())]
        .iter()
        .rposition(|&c| c == '\n')
        .map_or(0, |i| i + 1)
}

/// The index of the line's newline, or the text's end.
fn line_end(chars: &[char], at: usize) -> usize {
    let at = at.min(chars.len());
    chars[at..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(chars.len(), |i| at + i)
}

fn line_index(chars: &[char], at: usize) -> usize {
    chars[..at.min(chars.len())]
        .iter()
        .filter(|&&c| c == '\n')
        .count()
}

fn line_count(chars: &[char]) -> usize {
    chars.iter().filter(|&&c| c == '\n').count() + 1
}

fn nth_line_start(chars: &[char], line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    chars
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == '\n')
        .nth(line - 1)
        .map_or(chars.len(), |(i, _)| i + 1)
}

fn first_non_blank(chars: &[char], at: usize) -> usize {
    let start = line_start(chars, at);
    let end = line_end(chars, start);
    (start..end).find(|&i| !is_blank(chars[i])).unwrap_or(end)
}

fn indent(chars: &[char], at: usize) -> String {
    let start = line_start(chars, at);
    chars[start..first_non_blank(chars, start)].iter().collect()
}

/// `column` on `line`, or its last char when the line is shorter.
fn at_column(chars: &[char], line: usize, column: usize) -> usize {
    let start = nth_line_start(chars, line);
    let length = line_end(chars, start) - start;
    start + column.min(length.saturating_sub(1))
}

/// Normal mode keeps the cursor on a char: never on a newline, unless the
/// line is empty, and never past the end.
fn clamp_normal(chars: &[char], cursor: usize) -> usize {
    let cursor = cursor.min(chars.len());
    let start = line_start(chars, cursor);
    let end = line_end(chars, cursor);
    if cursor >= end && end > start {
        end - 1
    } else {
        cursor
    }
}

fn word_start(chars: &[char], at: usize, big: bool) -> usize {
    let len = chars.len();
    if at >= len {
        return len;
    }
    let mut at = at;
    let here = class(chars[at], big);
    if here != Class::Blank {
        while at < len && class(chars[at], big) == here {
            at += 1;
        }
    }
    while at < len && chars[at].is_whitespace() {
        at += 1;
        // An empty line counts as a word.
        if chars[at - 1] == '\n' && chars.get(at).is_none_or(|&c| c == '\n') {
            return at;
        }
    }
    at
}

fn word_end(chars: &[char], at: usize, big: bool) -> usize {
    let len = chars.len();
    let mut at = at + 1;
    while at < len && chars[at].is_whitespace() {
        at += 1;
    }
    if at >= len {
        return len.saturating_sub(1);
    }
    let here = class(chars[at], big);
    while at + 1 < len && class(chars[at + 1], big) == here {
        at += 1;
    }
    at
}

fn word_back(chars: &[char], at: usize, big: bool) -> usize {
    if at == 0 {
        return 0;
    }
    let mut at = at.min(chars.len()) - 1;
    while at > 0 && chars[at].is_whitespace() {
        if chars[at] == '\n' && chars[at - 1] == '\n' {
            return at;
        }
        at -= 1;
    }
    let here = class(chars[at], big);
    if here == Class::Blank {
        return at;
    }
    while at > 0 && class(chars[at - 1], big) == here {
        at -= 1;
    }
    at
}

/// Where `cw` stops: the end of the word under the cursor, then of the
/// words after it.
fn change_word_end(chars: &[char], at: usize, count: usize, big: bool) -> usize {
    let here = class(chars[at], big);
    let mut end = at;
    while end + 1 < chars.len() && class(chars[end + 1], big) == here {
        end += 1;
    }
    repeat(count - 1, end, |i| word_end(chars, i, big))
}

/// `dw` on a line's last word stops at the line's end rather than taking
/// the next line's indent with it.
fn word_operator_end(chars: &[char], from: usize, to: usize) -> usize {
    let mut end = to;
    while end > from && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    if !chars[end..to].contains(&'\n') {
        return to;
    }
    if end > from {
        end
    } else {
        line_end(chars, from)
    }
}

fn find_in_line(chars: &[char], at: usize, find: Find, count: usize, again: bool) -> Option<Jump> {
    let Find { kind, c } = find;
    let start = line_start(chars, at);
    let end = line_end(chars, at);
    // Repeating `t` skips the match right next to the cursor.
    let skip = usize::from(again);
    let nth = |positions: &mut dyn Iterator<Item = usize>| {
        positions.filter(|&i| chars[i] == c).nth(count - 1)
    };
    let jump = match kind {
        FindKind::To => Jump::inclusive(nth(&mut (at + 1..end))?),
        FindKind::Till => Jump::inclusive(nth(&mut (at + 1 + skip..end))? - 1),
        FindKind::BackTo => Jump::exclusive(nth(&mut (start..at).rev())?),
        FindKind::BackTill => {
            Jump::exclusive(nth(&mut (start..at.saturating_sub(skip)).rev())? + 1)
        }
    };
    Some(jump)
}

const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// `%`: the bracket matching the first one at or after the cursor on its line.
fn match_pair(chars: &[char], at: usize) -> Option<usize> {
    let end = line_end(chars, at);
    let from = (at..end).find(|&i| PAIRS.iter().any(|&(o, c)| chars[i] == o || chars[i] == c))?;
    let &(open, close) = PAIRS
        .iter()
        .find(|&&(o, c)| chars[from] == o || chars[from] == c)?;
    if chars[from] == open {
        find_close(chars, from, (open, close))
    } else {
        find_open(chars, from, (open, close))
    }
}

/// The open bracket before `at` that isn't closed before it.
fn find_open(chars: &[char], at: usize, (open, close): (char, char)) -> Option<usize> {
    let mut depth = 0;
    for i in (0..at.min(chars.len())).rev() {
        if chars[i] == close {
            depth += 1;
        } else if chars[i] == open {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
    }
    None
}

/// The bracket that closes the one at `open_at`.
fn find_close(chars: &[char], open_at: usize, (open, close): (char, char)) -> Option<usize> {
    let mut depth = 0;
    for (i, &c) in chars.iter().enumerate().skip(open_at + 1) {
        if c == open {
            depth += 1;
        } else if c == close {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
    }
    None
}

fn text_object(chars: &[char], at: usize, object: TextObject, count: usize) -> Option<Span> {
    let TextObject { object, around } = object;
    match object {
        Object::Word { big } => word_object(chars, at, big, around, count).map(Span::chars),
        Object::Quote(quote) => quote_object(chars, at, quote, around).map(Span::chars),
        Object::Bracket(open, close) => bracket_object(chars, at, (open, close), around, count),
    }
}

/// The run of same-class chars around `at`, within its line.
fn run_at(chars: &[char], at: usize, big: bool) -> Range<usize> {
    let here = class(chars[at], big);
    let same = |i: usize| chars[i] != '\n' && class(chars[i], big) == here;
    let mut start = at;
    while start > 0 && same(start - 1) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < chars.len() && same(end) {
        end += 1;
    }
    start..end
}

fn word_object(
    chars: &[char],
    at: usize,
    big: bool,
    around: bool,
    count: usize,
) -> Option<Range<usize>> {
    if chars.get(at).is_none_or(|&c| c == '\n') {
        return None;
    }
    let on_line = |i: usize| chars.get(i).is_some_and(|&c| c != '\n');
    let blank_at = |i: usize| chars.get(i).is_some_and(|&c| is_blank(c));
    let mut range = run_at(chars, at, big);
    if !around {
        for _ in 1..count {
            if !on_line(range.end) {
                break;
            }
            range.end = run_at(chars, range.end, big).end;
        }
        return Some(range);
    }
    let on_blank = is_blank(chars[at]);
    let words = if on_blank { count } else { count - 1 };
    for _ in 0..words {
        if blank_at(range.end) {
            range.end = run_at(chars, range.end, big).end;
        }
        if on_line(range.end) {
            range.end = run_at(chars, range.end, big).end;
        }
    }
    if on_blank {
        return Some(range);
    }
    if blank_at(range.end) {
        range.end = run_at(chars, range.end, big).end;
    } else if range.start > 0 && blank_at(range.start - 1) {
        range.start = run_at(chars, range.start - 1, big).start;
    }
    Some(range)
}

/// A quoted string on the cursor's line: the one around the cursor, or the
/// next one after it.
fn quote_object(chars: &[char], at: usize, quote: char, around: bool) -> Option<Range<usize>> {
    let start = line_start(chars, at);
    let end = line_end(chars, at);
    let quotes: Vec<usize> = (start..end)
        .filter(|&i| chars[i] == quote && (i == start || chars[i - 1] != '\\'))
        .collect();
    let (open, close) = quotes
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        .find(|&(_, close)| at <= close)?;
    if !around {
        return Some(open + 1..close);
    }
    let mut range = open..close + 1;
    let trailing = (range.end..end).take_while(|&i| is_blank(chars[i])).count();
    if trailing > 0 {
        range.end += trailing;
    } else {
        range.start -= (start..range.start)
            .rev()
            .take_while(|&i| is_blank(chars[i]))
            .count();
    }
    Some(range)
}

/// The brackets around the cursor, `count` levels out. Inside a block that
/// spans lines, `i` takes whole lines: not the newline after the opener, nor
/// the indent before the closer.
fn bracket_object(
    chars: &[char],
    at: usize,
    pair: (char, char),
    around: bool,
    count: usize,
) -> Option<Span> {
    let mut left = match chars.get(at) {
        Some(&c) if c == pair.0 => at,
        _ => find_open(chars, at, pair)?,
    };
    for _ in 1..count {
        left = find_open(chars, left, pair)?;
    }
    let right = find_close(chars, left, pair)?;
    if around {
        return Some(Span::chars(left..right + 1));
    }
    let mut start = left + 1;
    if chars.get(start) == Some(&'\n') {
        start += 1;
    }
    let close_line = line_start(chars, right);
    let end = if close_line > left && chars[close_line..right].iter().all(|&c| is_blank(c)) {
        close_line
    } else {
        right
    };
    let start = start.min(end);
    let whole_lines = start < end && start == line_start(chars, start) && chars[end - 1] == '\n';
    if whole_lines {
        return Some(Span::lines(start, end - 1));
    }
    Some(Span::chars(start..end))
}

#[cfg(test)]
mod tests;
