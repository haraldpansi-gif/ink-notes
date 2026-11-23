//! LaTeX formulas in text boxes (Issue #61).
//!
//! Text boxes in the normal font show `$…$` and `\(…\)` inside the text, and `$$…$$`, `\[…\]`
//! and `\begin{equation}`, `align`, `gather` … on a line of their own, as typeset formulas.
//! The box keeps the LaTeX code, which the editor shows, so it can be changed and copied back
//! (e.g. to Overleaf).
//!
//! Only a common subset is supported: sub- and superscripts, fractions, roots, big operators
//! with limits, Greek letters and the usual symbols, `\left…\right`, accents, `\text`,
//! matrices, `cases` and aligned equations. It is drawn with the fonts egui ships (Ubuntu
//! Light, Hack for symbols Ubuntu lacks), so it needs no extra font and looks the same on
//! screen, in PDFs and on every platform. Unknown commands stay visible as `\name`.

use std::cell::RefCell;
use std::collections::HashMap;

use ab_glyph::{Font, GlyphId};

use crate::text::{PlacedGlyph, Rule, TextFont};

// ---------------------------------------------------------------------------------------------
// Finding formulas in the text

/// A piece of a text box: plain text or a formula (its LaTeX code without the delimiters).
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    Text(String),
    /// Formula inside the text (`$…$`, `\(…\)`)
    Inline(String),
    /// Formula on a line of its own (`$$…$$`, `\[…\]`, `\begin{equation}…`)
    Display(String),
}

/// Environments that are formulas on their own line (with the `\begin…\end`, which the parser
/// reads).
const DISPLAY_ENVS: [&str; 14] = [
    "equation",
    "equation*",
    "align",
    "align*",
    "gather",
    "gather*",
    "multline",
    "multline*",
    "flalign",
    "flalign*",
    "eqnarray",
    "eqnarray*",
    "displaymath",
    "math",
];

/// Splits a text into plain text and formulas. `\$` is a dollar sign. A single `$` only opens
/// a formula when followed by a non-space and closes only after a non-space and not before a
/// digit (as in Pandoc), so "it costs $5 and $10" stays text.
pub fn split(content: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < content.len() {
        let rest = &content[i..];
        if rest.starts_with("\\$") {
            text.push('$');
            i += 2;
            continue;
        }
        if let Some((segment, len)) = math_at(rest) {
            if !text.is_empty() {
                segments.push(Segment::Text(std::mem::take(&mut text)));
            }
            segments.push(segment);
            i += len;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        text.push(c);
        i += c.len_utf8();
    }
    if !text.is_empty() {
        segments.push(Segment::Text(text));
    }
    segments
}

/// Formula starting at the beginning of `rest` and the length of its source.
fn math_at(rest: &str) -> Option<(Segment, usize)> {
    let delimited = |open: &str, close: &str| -> Option<(String, usize)> {
        let body = rest.strip_prefix(open)?;
        let end = body.find(close)?;
        Some((body[..end].to_string(), open.len() + end + close.len()))
    };
    if rest.starts_with("$$") {
        return delimited("$$", "$$")
            .filter(|(m, _)| !m.trim().is_empty())
            .map(|(m, n)| (Segment::Display(m), n));
    }
    if rest.starts_with("\\[") {
        return delimited("\\[", "\\]").map(|(m, n)| (Segment::Display(m), n));
    }
    if rest.starts_with("\\(") {
        return delimited("\\(", "\\)").map(|(m, n)| (Segment::Inline(m), n));
    }
    if let Some(after) = rest.strip_prefix("\\begin{") {
        let name = &after[..after.find('}')?];
        if !DISPLAY_ENVS.contains(&name) {
            return None;
        }
        let close = format!("\\end{{{name}}}");
        let end = rest.find(&close)? + close.len();
        return Some((Segment::Display(rest[..end].to_string()), end));
    }
    let body = rest.strip_prefix('$')?;
    let first = body.chars().next()?;
    if first.is_whitespace() || first == '$' {
        return None;
    }
    let mut previous = first;
    for (j, c) in body.char_indices().skip(1) {
        if c == '\n' {
            return None;
        }
        if c == '$' && !previous.is_whitespace() && previous != '\\' {
            if body[j + 1..].starts_with(|a: char| a.is_ascii_digit()) {
                return None;
            }
            return Some((Segment::Inline(body[..j].to_string()), j + 2));
        }
        previous = c;
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Parsing

/// Kind of an atom, for the spacing between atoms (as in TeX).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Ord,
    Op,
    Bin,
    Rel,
    Open,
    Close,
    Punct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Display,
    Text,
    Script,
    ScriptScript,
}

impl Style {
    fn scale(self) -> f32 {
        match self {
            Style::Display | Style::Text => 1.0,
            Style::Script => 0.72,
            Style::ScriptScript => 0.56,
        }
    }

    fn script(self) -> Self {
        match self {
            Style::Display | Style::Text => Style::Script,
            _ => Style::ScriptScript,
        }
    }

    fn fraction(self) -> Self {
        match self {
            Style::Display => Style::Text,
            Style::Text => Style::Script,
            _ => Style::ScriptScript,
        }
    }

    fn is_script(self) -> bool {
        matches!(self, Style::Script | Style::ScriptScript)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

/// When an operator takes its limits above and below instead of as scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Limits {
    /// In display formulas (sums, `\lim`)
    Display,
    Always,
    Never,
}

/// Mark above or below a formula.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mark {
    Overline,
    Underline,
    Glyph(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableKind {
    Matrix,
    Cases,
    /// Columns in pairs right/left (`align`, `aligned`)
    Aligned,
    /// One centred column (`gather`)
    Gather,
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Sym(char, Class),
    /// Upright text (`\text{…}`, `\sin`)
    Word(String, Class),
    Row(Vec<Node>),
    Op {
        body: Box<Node>,
        large: bool,
        limits: Limits,
    },
    Scripts {
        base: Box<Node>,
        sub: Option<Box<Node>>,
        sup: Option<Box<Node>>,
    },
    Frac {
        num: Box<Node>,
        den: Box<Node>,
        rule: bool,
        style: Option<Style>,
    },
    Sqrt {
        index: Option<Box<Node>>,
        body: Box<Node>,
    },
    /// Horizontal space in em
    Space(f32),
    Accent {
        body: Box<Node>,
        mark: Mark,
    },
    Delimited {
        left: Option<char>,
        body: Box<Node>,
        right: Option<char>,
    },
    /// `\big(` and friends: a bracket of the given height in em
    Big {
        ch: char,
        height: f32,
        class: Class,
    },
    Table {
        rows: Vec<Vec<Node>>,
        aligns: Vec<Align>,
        kind: TableKind,
    },
}

/// Where a row of atoms ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Eof,
    /// Before a `}` (not consumed)
    Brace,
    /// Before a `]` (not consumed), only when asked for
    Bracket,
    /// After `&`
    Amp,
    /// After `\\`
    Newline,
    /// Before `\right` (not consumed)
    Right,
    /// Before `\end` (not consumed)
    Environment,
}

struct Parser {
    s: Vec<char>,
    i: usize,
}

impl Parser {
    fn new(src: &str) -> Self {
        Self {
            s: src.chars().collect(),
            i: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn skip_spaces(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.i += 1;
        }
    }

    /// Reads a command at a `\`: letters, or a single other character.
    fn command(&mut self) -> String {
        self.i += 1;
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        if self.i == start && self.peek().is_some() {
            self.i += 1;
        }
        let mut name: String = self.s[start..self.i].iter().collect();
        if self.peek() == Some('*')
            && matches!(name.as_str(), "operatorname" | "tag" | "hspace" | "vspace")
        {
            self.i += 1;
            name.push('*');
        }
        name
    }

    fn peek_command(&mut self) -> Option<String> {
        if self.peek() != Some('\\') {
            return None;
        }
        let save = self.i;
        let name = self.command();
        self.i = save;
        Some(name)
    }

    /// Atoms up to the end of the formula, a `}`, `&`, `\\`, `\right` or `\end`.
    fn row(&mut self, until_bracket: bool) -> (Vec<Node>, End) {
        let mut nodes: Vec<Node> = Vec::new();
        loop {
            self.skip_spaces();
            let Some(c) = self.peek() else {
                return (nodes, End::Eof);
            };
            match c {
                '}' => return (nodes, End::Brace),
                ']' if until_bracket => return (nodes, End::Bracket),
                '&' => {
                    self.i += 1;
                    return (nodes, End::Amp);
                }
                // Scripts without a base (`^{14}C`, or after a `\limits`)
                '^' | '_' | '\'' => nodes.push(self.scripts(Node::Row(Vec::new()))),
                _ => {
                    match self.peek_command().as_deref() {
                        Some("\\") | Some("cr") | Some("newline") => {
                            self.command();
                            return (nodes, End::Newline);
                        }
                        Some("right") => return (nodes, End::Right),
                        Some("end") => return (nodes, End::Environment),
                        Some(name @ ("limits" | "nolimits")) => {
                            let always = name == "limits";
                            self.command();
                            if let Some(Node::Op { limits, .. }) = nodes.last_mut() {
                                *limits = if always {
                                    Limits::Always
                                } else {
                                    Limits::Never
                                };
                            }
                            continue;
                        }
                        _ => {}
                    }
                    if let Some(atom) = self.atom() {
                        nodes.push(self.scripts(atom));
                    }
                }
            }
        }
    }

    /// Contents of a `{…}` group, after the `{`.
    fn group(&mut self) -> Node {
        let mut all = Vec::new();
        loop {
            let (nodes, end) = self.row(false);
            all.extend(nodes);
            match end {
                End::Brace => {
                    self.i += 1;
                    break;
                }
                End::Eof => break,
                End::Amp | End::Newline | End::Bracket => {}
                End::Right => {
                    self.command();
                    self.delimiter();
                }
                End::Environment => {
                    self.command();
                    self.text_arg();
                }
            }
        }
        Node::Row(all)
    }

    /// Sub- and superscripts and primes after `base`.
    fn scripts(&mut self, mut base: Node) -> Node {
        let (mut sub, mut sup) = (None, None);
        loop {
            self.skip_spaces();
            match self.peek() {
                Some('^') if sup.is_none() => {
                    self.i += 1;
                    sup = Some(Box::new(self.arg()));
                }
                Some('_') if sub.is_none() => {
                    self.i += 1;
                    sub = Some(Box::new(self.arg()));
                }
                Some('\'') if sub.is_none() && sup.is_none() => {
                    self.i += 1;
                    base = Node::Row(vec![base, Node::Sym('′', Class::Ord)]);
                }
                Some('\\') => match self.peek_command().as_deref() {
                    Some(name @ ("limits" | "nolimits")) => {
                        let always = name == "limits";
                        self.command();
                        if let Node::Op { limits, .. } = &mut base {
                            *limits = if always {
                                Limits::Always
                            } else {
                                Limits::Never
                            };
                        }
                    }
                    _ => break,
                },
                _ => break,
            }
        }
        if sub.is_none() && sup.is_none() {
            base
        } else {
            Node::Scripts {
                base: Box::new(base),
                sub,
                sup,
            }
        }
    }

    /// Argument of a command: a group or a single atom.
    fn arg(&mut self) -> Node {
        self.skip_spaces();
        match self.peek() {
            Some('{') => {
                self.i += 1;
                self.group()
            }
            Some('}' | '^' | '_' | '&') | None => Node::Row(Vec::new()),
            Some(_) => self.atom().unwrap_or(Node::Row(Vec::new())),
        }
    }

    /// Optional argument in `[…]`.
    fn opt_arg(&mut self) -> Option<Node> {
        self.skip_spaces();
        if self.peek() != Some('[') {
            return None;
        }
        self.i += 1;
        let (nodes, end) = self.row(true);
        if end == End::Bracket {
            self.i += 1;
        }
        Some(Node::Row(nodes))
    }

    /// Raw text of a `{…}` argument (or a single character), e.g. for `\text`.
    fn text_arg(&mut self) -> String {
        self.skip_spaces();
        if self.peek() != Some('{') {
            return self
                .peek()
                .map(|c| {
                    self.i += 1;
                    c.to_string()
                })
                .unwrap_or_default();
        }
        self.i += 1;
        let mut depth = 0;
        let mut text = String::new();
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                '{' => depth += 1,
                '}' if depth == 0 => break,
                '}' => depth -= 1,
                '\\' => {
                    if let Some(next) = self.peek() {
                        self.i += 1;
                        if next.is_ascii_alphabetic() {
                            text.push('\\');
                        }
                        text.push(match next {
                            ' ' | ',' | ';' | ':' | '!' => ' ',
                            other => other,
                        });
                    }
                    continue;
                }
                '$' | '~' => {
                    if c == '~' {
                        text.push(' ');
                    }
                    continue;
                }
                _ => {}
            }
            if c != '{' && c != '}' {
                text.push(c);
            }
        }
        text
    }

    /// A bracket after `\left`, `\right` or `\big`; `None` for `.`.
    fn delimiter(&mut self) -> Option<char> {
        self.skip_spaces();
        let c = self.peek()?;
        if c == '\\' {
            let name = self.command();
            return match name.as_str() {
                "{" | "lbrace" => Some('{'),
                "}" | "rbrace" => Some('}'),
                "|" | "Vert" | "lVert" | "rVert" => Some('‖'),
                "vert" | "lvert" | "rvert" | "mid" => Some('|'),
                "langle" => Some('⟨'),
                "rangle" => Some('⟩'),
                "lfloor" => Some('⌊'),
                "rfloor" => Some('⌋'),
                "lceil" => Some('⌈'),
                "rceil" => Some('⌉'),
                "lbrack" => Some('['),
                "rbrack" => Some(']'),
                _ => None,
            };
        }
        self.i += 1;
        (c != '.').then_some(c)
    }

    /// One atom without scripts; `None` for commands that draw nothing.
    fn atom(&mut self) -> Option<Node> {
        self.skip_spaces();
        let c = self.peek()?;
        if c == '\\' {
            return self.command_atom();
        }
        self.i += 1;
        Some(match c {
            '{' => self.group(),
            '}' => return None,
            '~' => Node::Space(1.0 / 3.0),
            '-' => Node::Sym('−', Class::Bin),
            '*' => Node::Sym('∗', Class::Bin),
            _ => Node::Sym(c, char_class(c)),
        })
    }

    fn command_atom(&mut self) -> Option<Node> {
        let name = self.command();
        let n = name.as_str();
        if let Some((ch, class)) = symbol(n) {
            return Some(Node::Sym(ch, class));
        }
        if let Some((ch, limits)) = big_operator(n) {
            return Some(Node::Op {
                body: Box::new(Node::Sym(ch, Class::Op)),
                large: true,
                limits,
            });
        }
        if let Some(limits) = function(n) {
            return Some(Node::Op {
                body: Box::new(Node::Word(n.to_string(), Class::Op)),
                large: false,
                limits,
            });
        }
        if let Some(em) = space(n) {
            return Some(Node::Space(em));
        }
        Some(match n {
            "frac" | "dfrac" | "tfrac" | "cfrac" | "binom" | "dbinom" | "tbinom" => {
                let num = Box::new(self.arg());
                let den = Box::new(self.arg());
                let style = match n {
                    "dfrac" | "cfrac" | "dbinom" => Some(Style::Display),
                    "tfrac" | "tbinom" => Some(Style::Text),
                    _ => None,
                };
                let rule = n.ends_with("frac");
                let frac = Node::Frac {
                    num,
                    den,
                    rule,
                    style,
                };
                if rule {
                    frac
                } else {
                    Node::Delimited {
                        left: Some('('),
                        body: Box::new(frac),
                        right: Some(')'),
                    }
                }
            }
            "sqrt" => {
                let index = self.opt_arg().map(Box::new);
                let body = Box::new(self.arg());
                Node::Sqrt { index, body }
            }
            "operatorname" | "operatorname*" => Node::Op {
                body: Box::new(Node::Word(self.text_arg(), Class::Op)),
                large: false,
                limits: if n.ends_with('*') {
                    Limits::Display
                } else {
                    Limits::Never
                },
            },
            "text" | "textrm" | "textit" | "textbf" | "textsf" | "texttt" | "textnormal"
            | "textup" | "mbox" | "hbox" | "emph" => Node::Word(self.text_arg(), Class::Ord),
            "mathrm" | "mathit" | "mathbf" | "mathsf" | "mathtt" | "mathcal" | "mathfrak"
            | "mathscr" | "mathnormal" | "boldsymbol" | "bm" | "pmb" | "mathbb" => {
                let arg = self.arg();
                if n == "mathbb" {
                    double_struck(arg)
                } else {
                    arg
                }
            }
            "overline" | "bar" | "underline" | "hat" | "widehat" | "tilde" | "widetilde"
            | "vec" | "overrightarrow" | "dot" | "ddot" | "check" | "breve" | "acute" | "grave" => {
                let mark = match n {
                    "overline" | "bar" => Mark::Overline,
                    "underline" => Mark::Underline,
                    "hat" | "widehat" => Mark::Glyph('^'),
                    "tilde" | "widetilde" => Mark::Glyph('~'),
                    "vec" | "overrightarrow" => Mark::Glyph('→'),
                    "dot" => Mark::Glyph('˙'),
                    "ddot" => Mark::Glyph('¨'),
                    "check" => Mark::Glyph('ˇ'),
                    "breve" => Mark::Glyph('˘'),
                    "acute" => Mark::Glyph('´'),
                    _ => Mark::Glyph('`'),
                };
                Node::Accent {
                    body: Box::new(self.arg()),
                    mark,
                }
            }
            "left" => {
                let left = self.delimiter();
                let mut body = Vec::new();
                loop {
                    let (nodes, end) = self.row(false);
                    body.extend(nodes);
                    match end {
                        End::Right | End::Eof => break,
                        End::Brace => self.i += 1,
                        End::Environment => {
                            self.command();
                            self.text_arg();
                        }
                        _ => {}
                    }
                }
                let right = if self.peek_command().as_deref() == Some("right") {
                    self.command();
                    self.delimiter()
                } else {
                    None
                };
                Node::Delimited {
                    left,
                    body: Box::new(Node::Row(body)),
                    right,
                }
            }
            "big" | "Big" | "bigg" | "Bigg" | "bigl" | "Bigl" | "biggl" | "Biggl" | "bigr"
            | "Bigr" | "biggr" | "Biggr" | "bigm" | "Bigm" | "biggm" | "Biggm" => {
                let height = match n.trim_end_matches(['l', 'r', 'm']) {
                    "big" => 1.2,
                    "Big" => 1.8,
                    "bigg" => 2.4,
                    _ => 3.0,
                };
                let ch = self.delimiter()?;
                let class = match n.chars().last() {
                    Some('l') => Class::Open,
                    Some('r') => Class::Close,
                    Some('m') => Class::Rel,
                    _ => char_class(ch),
                };
                Node::Big { ch, height, class }
            }
            "begin" => {
                let env = self.text_arg();
                self.environment(&env)
            }
            "label" | "tag" | "tag*" | "color" | "hspace" | "hspace*" | "vspace" | "vspace*"
            | "eqref" | "ref" => {
                self.text_arg();
                if n.starts_with("hspace") {
                    return Some(Node::Space(0.5));
                }
                return None;
            }
            "textcolor" | "colorbox" => {
                self.text_arg();
                self.arg()
            }
            "nonumber" | "notag" | "displaystyle" | "textstyle" | "scriptstyle"
            | "scriptscriptstyle" | "limits" | "nolimits" | "right" | "end" | "relax" => {
                return None;
            }
            _ => Node::Word(format!("\\{n}"), Class::Ord),
        })
    }

    /// `\begin{env}` … `\end{env}` after the name.
    fn environment(&mut self, env: &str) -> Node {
        let base = env.trim_end_matches('*');
        let mut aligns_spec = None;
        if base == "array" || base == "alignat" || base == "alignedat" {
            let spec = self.text_arg();
            if base == "array" {
                aligns_spec = Some(
                    spec.chars()
                        .filter_map(|c| match c {
                            'l' => Some(Align::Left),
                            'c' => Some(Align::Center),
                            'r' => Some(Align::Right),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                );
            }
        }
        let mut rows: Vec<Vec<Node>> = vec![Vec::new()];
        let mut cell: Vec<Node> = Vec::new();
        loop {
            let (nodes, end) = self.row(false);
            cell.extend(nodes);
            match end {
                End::Amp => {
                    let done = std::mem::take(&mut cell);
                    rows.last_mut().expect("eine Zeile").push(Node::Row(done));
                }
                End::Newline => {
                    let done = std::mem::take(&mut cell);
                    rows.last_mut().expect("eine Zeile").push(Node::Row(done));
                    rows.push(Vec::new());
                }
                End::Environment | End::Eof => {
                    if end == End::Environment {
                        self.command();
                        self.text_arg();
                    }
                    let done = std::mem::take(&mut cell);
                    rows.last_mut().expect("eine Zeile").push(Node::Row(done));
                    break;
                }
                End::Brace => self.i += 1,
                End::Right => {
                    self.command();
                    self.delimiter();
                }
                End::Bracket => {}
            }
        }
        // A `\\` at the end does not start another row
        if rows.len() > 1
            && rows
                .last()
                .is_some_and(|r| r.iter().all(|c| matches!(c, Node::Row(n) if n.is_empty())))
        {
            rows.pop();
        }
        let (kind, delimiters) = match base {
            "matrix" | "smallmatrix" | "array" => (TableKind::Matrix, None),
            "pmatrix" => (TableKind::Matrix, Some((Some('('), Some(')')))),
            "bmatrix" => (TableKind::Matrix, Some((Some('['), Some(']')))),
            "Bmatrix" => (TableKind::Matrix, Some((Some('{'), Some('}')))),
            "vmatrix" => (TableKind::Matrix, Some((Some('|'), Some('|')))),
            "Vmatrix" => (TableKind::Matrix, Some((Some('‖'), Some('‖')))),
            "cases" | "dcases" => (TableKind::Cases, Some((Some('{'), None))),
            "rcases" => (TableKind::Cases, Some((None, Some('}')))),
            "align" | "aligned" | "alignat" | "alignedat" | "split" | "flalign" | "eqnarray" => {
                (TableKind::Aligned, None)
            }
            _ => (TableKind::Gather, None),
        };
        let columns = rows.iter().map(Vec::len).max().unwrap_or(1);
        if rows.len() == 1 && columns == 1 && delimiters.is_none() {
            return rows
                .pop()
                .and_then(|mut r| r.pop())
                .unwrap_or(Node::Row(Vec::new()));
        }
        let aligns = (0..columns)
            .map(|c| match kind {
                _ if aligns_spec.is_some() => aligns_spec
                    .as_ref()
                    .and_then(|a| a.get(c).copied())
                    .unwrap_or(Align::Center),
                TableKind::Matrix | TableKind::Gather => Align::Center,
                TableKind::Cases => Align::Left,
                TableKind::Aligned if base == "eqnarray" => {
                    [Align::Right, Align::Center, Align::Left][c.min(2)]
                }
                TableKind::Aligned if c % 2 == 0 => Align::Right,
                TableKind::Aligned => Align::Left,
            })
            .collect();
        let table = Node::Table { rows, aligns, kind };
        match delimiters {
            Some((left, right)) => Node::Delimited {
                left,
                body: Box::new(table),
                right,
            },
            None => table,
        }
    }
}

/// `\mathbb{R}` and friends as the double-struck letter (drawn as the plain letter where the
/// fonts lack it).
fn double_struck(node: Node) -> Node {
    match node {
        Node::Sym(c, class) => Node::Sym(
            match c {
                'R' => 'ℝ',
                'N' => 'ℕ',
                'Z' => 'ℤ',
                'Q' => 'ℚ',
                'C' => 'ℂ',
                other => other,
            },
            class,
        ),
        Node::Row(nodes) => Node::Row(nodes.into_iter().map(double_struck).collect()),
        other => other,
    }
}

/// Class of a character typed directly in a formula.
fn char_class(c: char) -> Class {
    match c {
        '+' | '−' | '±' | '∓' | '·' | '×' | '÷' | '∗' | '∘' | '∪' | '∩' | '⊕' | '⊗' => {
            Class::Bin
        }
        '=' | '<' | '>' | ':' | '≤' | '≥' | '≠' | '≈' | '≡' | '∼' | '≃' | '≅' | '∝' | '→' | '←'
        | '↔' | '⇒' | '⇐' | '⇔' | '↦' | '∈' | '∉' | '⊂' | '⊆' | '⊃' | '⊇' | '≪' | '≫' | '⊥'
        | '∥' | '≔' => Class::Rel,
        '(' | '[' | '⟨' | '⌊' | '⌈' => Class::Open,
        ')' | ']' | '⟩' | '⌋' | '⌉' | '!' => Class::Close,
        ',' | ';' => Class::Punct,
        _ => Class::Ord,
    }
}

/// Symbols and Greek letters.
fn symbol(name: &str) -> Option<(char, Class)> {
    let ord = |c| Some((c, Class::Ord));
    let c = match name {
        "alpha" => 'α',
        "beta" => 'β',
        "gamma" => 'γ',
        "delta" => 'δ',
        "epsilon" | "varepsilon" => 'ε',
        "zeta" => 'ζ',
        "eta" => 'η',
        "theta" | "vartheta" => 'θ',
        "iota" => 'ι',
        "kappa" => 'κ',
        "lambda" => 'λ',
        "mu" => 'μ',
        "nu" => 'ν',
        "xi" => 'ξ',
        "pi" => 'π',
        "varpi" => 'ϖ',
        "rho" | "varrho" => 'ρ',
        "sigma" => 'σ',
        "varsigma" => 'ς',
        "tau" => 'τ',
        "upsilon" => 'υ',
        "phi" | "varphi" => 'φ',
        "chi" => 'χ',
        "psi" => 'ψ',
        "omega" => 'ω',
        "Gamma" => 'Γ',
        "Delta" => 'Δ',
        "Theta" => 'Θ',
        "Lambda" => 'Λ',
        "Xi" => 'Ξ',
        "Pi" => 'Π',
        "Sigma" => 'Σ',
        "Upsilon" => 'Υ',
        "Phi" => 'Φ',
        "Psi" => 'Ψ',
        "Omega" => 'Ω',
        "infty" => '∞',
        "partial" => '∂',
        "nabla" => '∇',
        "forall" => '∀',
        "exists" => '∃',
        "emptyset" | "varnothing" => '∅',
        "angle" => '∠',
        "degree" => '°',
        "prime" => '′',
        "ldots" | "dots" | "dotsc" | "dotsb" | "cdots" | "dotsm" => '…',
        "vdots" => '⋮',
        "hbar" => 'ℏ',
        "ell" => 'ℓ',
        "Re" => 'ℜ',
        "Im" => 'ℑ',
        "aleph" => 'ℵ',
        "%" | "$" | "#" | "_" => name.chars().next()?,
        "&" => '&',
        "|" | "Vert" => '‖',
        "vert" => '|',
        "backslash" | "setminus" => '\\',
        "neg" | "lnot" => '¬',
        "checkmark" => '✓',
        "Omicron" => 'Ο',
        "square" | "Box" => '□',
        "triangle" => '△',
        "star" => '⋆',
        "dagger" => '†',
        _ => '\0',
    };
    if c != '\0' {
        return ord(c);
    }
    let (c, class) = match name {
        "pm" => ('±', Class::Bin),
        "mp" => ('∓', Class::Bin),
        "cdot" => ('·', Class::Bin),
        "times" => ('×', Class::Bin),
        "div" => ('÷', Class::Bin),
        "ast" => ('∗', Class::Bin),
        "circ" => ('∘', Class::Bin),
        "bullet" => ('•', Class::Bin),
        "cup" => ('∪', Class::Bin),
        "cap" => ('∩', Class::Bin),
        "oplus" => ('⊕', Class::Bin),
        "otimes" => ('⊗', Class::Bin),
        "wedge" | "land" => ('∧', Class::Bin),
        "vee" | "lor" => ('∨', Class::Bin),
        "le" | "leq" | "leqslant" => ('≤', Class::Rel),
        "ge" | "geq" | "geqslant" => ('≥', Class::Rel),
        "ne" | "neq" => ('≠', Class::Rel),
        "approx" => ('≈', Class::Rel),
        "equiv" => ('≡', Class::Rel),
        "sim" => ('∼', Class::Rel),
        "simeq" => ('≃', Class::Rel),
        "cong" => ('≅', Class::Rel),
        "propto" => ('∝', Class::Rel),
        "to" | "rightarrow" | "longrightarrow" => ('→', Class::Rel),
        "gets" | "leftarrow" | "longleftarrow" => ('←', Class::Rel),
        "leftrightarrow" | "longleftrightarrow" => ('↔', Class::Rel),
        "Rightarrow" | "implies" | "Longrightarrow" => ('⇒', Class::Rel),
        "Leftarrow" | "Longleftarrow" => ('⇐', Class::Rel),
        "Leftrightarrow" | "iff" | "Longleftrightarrow" => ('⇔', Class::Rel),
        "mapsto" => ('↦', Class::Rel),
        "uparrow" => ('↑', Class::Rel),
        "downarrow" => ('↓', Class::Rel),
        "in" => ('∈', Class::Rel),
        "notin" => ('∉', Class::Rel),
        "ni" => ('∋', Class::Rel),
        "subset" => ('⊂', Class::Rel),
        "subseteq" => ('⊆', Class::Rel),
        "supset" => ('⊃', Class::Rel),
        "supseteq" => ('⊇', Class::Rel),
        "ll" => ('≪', Class::Rel),
        "gg" => ('≫', Class::Rel),
        "perp" => ('⊥', Class::Rel),
        "parallel" => ('∥', Class::Rel),
        "mid" => ('|', Class::Rel),
        "coloneqq" => ('≔', Class::Rel),
        "{" | "lbrace" => ('{', Class::Open),
        "}" | "rbrace" => ('}', Class::Close),
        "langle" => ('⟨', Class::Open),
        "rangle" => ('⟩', Class::Close),
        "lfloor" => ('⌊', Class::Open),
        "rfloor" => ('⌋', Class::Close),
        "lceil" => ('⌈', Class::Open),
        "rceil" => ('⌉', Class::Close),
        "lvert" => ('|', Class::Open),
        "rvert" => ('|', Class::Close),
        "lVert" => ('‖', Class::Open),
        "rVert" => ('‖', Class::Close),
        "colon" => (':', Class::Punct),
        _ => return None,
    };
    Some((c, class))
}

/// Big operators and whether they take limits above and below.
fn big_operator(name: &str) -> Option<(char, Limits)> {
    Some(match name {
        "sum" => ('∑', Limits::Display),
        "prod" => ('∏', Limits::Display),
        "coprod" => ('∐', Limits::Display),
        "bigcup" => ('⋃', Limits::Display),
        "bigcap" => ('⋂', Limits::Display),
        "bigoplus" => ('⊕', Limits::Display),
        "bigotimes" => ('⊗', Limits::Display),
        "int" => ('∫', Limits::Never),
        "iint" => ('∬', Limits::Never),
        "iiint" => ('∭', Limits::Never),
        "oint" => ('∮', Limits::Never),
        _ => return None,
    })
}

/// Function names written upright, and whether they take limits (`\lim_{x\to 0}`).
fn function(name: &str) -> Option<Limits> {
    match name {
        "lim" | "limsup" | "liminf" | "max" | "min" | "sup" | "inf" | "det" | "gcd" | "Pr"
        | "argmax" | "argmin" => Some(Limits::Display),
        "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "arcsin" | "arccos" | "arctan" | "sinh"
        | "cosh" | "tanh" | "coth" | "log" | "ln" | "lg" | "exp" | "arg" | "deg" | "dim"
        | "ker" | "hom" | "mod" | "bmod" | "Re" | "Im" => Some(Limits::Never),
        _ => None,
    }
}

/// Spacing commands in em.
fn space(name: &str) -> Option<f32> {
    Some(match name {
        "quad" => 1.0,
        "qquad" => 2.0,
        "," | "thinspace" => 3.0 / 18.0,
        ":" | ">" | "medspace" => 4.0 / 18.0,
        ";" | "thickspace" => 5.0 / 18.0,
        "!" | "negthinspace" => -3.0 / 18.0,
        " " => 1.0 / 3.0,
        "enspace" => 0.5,
        _ => return None,
    })
}

/// Reads a formula.
fn parse(src: &str) -> Node {
    let mut parser = Parser::new(src);
    let mut rows: Vec<Vec<Node>> = vec![Vec::new()];
    loop {
        let (nodes, end) = parser.row(false);
        rows.last_mut().expect("eine Zeile").extend(nodes);
        match end {
            End::Eof => break,
            End::Newline => rows.push(Vec::new()),
            End::Amp | End::Bracket => {}
            End::Brace => parser.i += 1,
            End::Right => {
                parser.command();
                parser.delimiter();
            }
            End::Environment => {
                parser.command();
                parser.text_arg();
            }
        }
    }
    while rows.len() > 1 && rows.last().is_some_and(Vec::is_empty) {
        rows.pop();
    }
    if rows.len() == 1 {
        let mut row = rows.pop().unwrap_or_default();
        if row.len() == 1 {
            row.pop().unwrap_or(Node::Row(Vec::new()))
        } else {
            Node::Row(row)
        }
    } else {
        Node::Table {
            aligns: vec![Align::Center],
            rows: rows.into_iter().map(|r| vec![Node::Row(r)]).collect(),
            kind: TableKind::Gather,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Layout

/// A typeset formula: glyphs and rules relative to the start of its baseline (y down).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MathBox {
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    pub glyphs: Vec<PlacedGlyph>,
    pub rules: Vec<Rule>,
}

impl MathBox {
    fn space(width: f32) -> Self {
        Self {
            width,
            ..Self::default()
        }
    }

    /// Places `other` with its baseline start at `x`, `dy` below this baseline.
    fn place(&mut self, other: MathBox, x: f32, dy: f32) {
        if other.glyphs.is_empty() && other.rules.is_empty() {
            return;
        }
        self.ascent = self.ascent.max(other.ascent - dy);
        self.descent = self.descent.max(other.descent + dy);
        self.glyphs.extend(other.glyphs.into_iter().map(|mut g| {
            g.x += x;
            g.dy += dy;
            g
        }));
        self.rules.extend(other.rules.into_iter().map(|mut r| {
            r.x += x;
            r.dy += dy;
            r
        }));
    }

    /// Adds a rule `height` thick with its top edge `top` below the baseline.
    fn rule(&mut self, x: f32, top: f32, width: f32, height: f32) {
        if width <= 0.0 {
            return;
        }
        self.ascent = self.ascent.max(-top);
        self.descent = self.descent.max(top + height);
        self.rules.push(Rule {
            x,
            dy: top,
            width,
            height,
        });
    }

    /// The same box, `dy` lower.
    fn lowered(self, dy: f32) -> MathBox {
        let mut out = MathBox::space(self.width);
        out.ascent = f32::MIN;
        out.descent = f32::MIN;
        out.place(self, 0.0, dy);
        out.ascent = out.ascent.max(0.0);
        out.descent = out.descent.max(0.0);
        out
    }
}

/// Ink extent and advance of a glyph in em (y up).
#[derive(Debug, Clone, Copy, Default)]
struct Ink {
    advance: f32,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

impl Ink {
    fn center(&self) -> f32 {
        (self.top + self.bottom) / 2.0
    }

    fn height(&self) -> f32 {
        self.top - self.bottom
    }
}

fn ink(font: TextFont, id: GlyphId) -> Ink {
    thread_local! {
        static CACHE: RefCell<HashMap<(TextFont, u16), Ink>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|cache| {
        *cache.borrow_mut().entry((font, id.0)).or_insert_with(|| {
            let face = font.face();
            let per_em = 1.0 / face.units_per_em().unwrap_or(1000.0);
            let advance = face.h_advance_unscaled(id) * per_em;
            match face.outline(id) {
                // ab_glyph keeps the top in `min.y` and the bottom in `max.y`
                Some(outline) => Ink {
                    advance,
                    left: outline.bounds.min.x * per_em,
                    right: outline.bounds.max.x * per_em,
                    top: outline.bounds.min.y * per_em,
                    bottom: outline.bounds.max.y * per_em,
                },
                None => Ink {
                    advance,
                    ..Ink::default()
                },
            }
        })
    })
}

/// Stand-ins for symbols none of the fonts has.
fn substitute(c: char) -> Option<char> {
    Some(match c {
        'ϵ' => 'ε',
        'ϕ' => 'φ',
        'ϑ' => 'θ',
        'ϖ' => 'π',
        '∮' => '∫',
        '∬' => '∫',
        '∭' => '∫',
        'ℝ' => 'R',
        'ℕ' => 'N',
        'ℤ' => 'Z',
        'ℚ' => 'Q',
        'ℂ' => 'C',
        '∥' => '‖',
        '‖' => '|',
        '⋅' => '·',
        '∗' => '*',
        '′' => '\'',
        '˙' => '·',
        '−' => '-',
        '∐' => '∏',
        '⋮' => ':',
        '…' => '.',
        '≔' => '=',
        _ => return None,
    })
}

struct Typesetter {
    font: TextFont,
    other: TextFont,
    /// Em of the formula in page units
    size: f32,
    /// Height of the maths axis (middle of `+`) in em
    axis: f32,
    /// Thickness of fraction bars in em
    rule: f32,
}

impl Typesetter {
    fn new(font: TextFont, size: f32) -> Self {
        let other = match font {
            TextFont::Sans => TextFont::Mono,
            TextFont::Mono => TextFont::Sans,
        };
        let mut setter = Self {
            font,
            other,
            size,
            axis: 0.25,
            rule: 0.05,
        };
        if let Some((_, _, plus)) = setter.find('+') {
            setter.axis = plus.center();
        }
        if let Some((_, _, minus)) = setter.find('−') {
            setter.rule = minus.height().clamp(0.035, 0.08);
        }
        setter
    }

    fn find(&self, c: char) -> Option<(TextFont, GlyphId, Ink)> {
        let mut candidate = Some(c);
        while let Some(c) = candidate {
            for font in [self.font, self.other] {
                let id = font.face().glyph_id(c);
                if id.0 != 0 {
                    return Some((font, id, ink(font, id)));
                }
            }
            candidate = substitute(c);
        }
        None
    }

    fn em(&self, style: Style) -> f32 {
        self.size * style.scale()
    }

    /// A character `size` big, stretched `stretch` times vertically.
    fn glyph(&self, c: char, size: f32, stretch: f32) -> MathBox {
        let Some((font, id, ink)) = self.find(c) else {
            return MathBox::default();
        };
        MathBox {
            width: ink.advance * size,
            ascent: (ink.top * size * stretch).max(0.0),
            descent: (-ink.bottom * size * stretch).max(0.0),
            glyphs: vec![PlacedGlyph {
                id,
                font,
                x: 0.0,
                dy: 0.0,
                size,
                stretch,
            }],
            rules: Vec::new(),
        }
    }

    /// A character stretched to at least `height` (page units) and centred on the axis.
    fn centered(&self, c: char, size: f32, height: f32, axis: f32) -> MathBox {
        let Some((_, _, ink)) = self.find(c) else {
            return MathBox::default();
        };
        let natural = ink.height().max(1e-3) * size;
        let stretch = (height / natural).max(1.0);
        let glyph = self.glyph(c, size, stretch);
        glyph.lowered(ink.center() * size * stretch - axis)
    }

    fn layout(&self, node: &Node, style: Style) -> MathBox {
        let em = self.em(style);
        match node {
            Node::Sym(c, _) => self.glyph(*c, em, 1.0),
            Node::Word(text, _) => {
                let mut out = MathBox::default();
                let mut x = 0.0;
                for c in text.chars() {
                    let glyph = self.glyph(c, em, 1.0);
                    let width = glyph.width;
                    out.place(glyph, x, 0.0);
                    x += width;
                }
                out.width = x;
                out
            }
            Node::Row(nodes) => self.row(nodes, style, false),
            Node::Op { body, large, .. } => match (large, body.as_ref()) {
                (true, Node::Sym(c, _)) => self.operator(*c, style),
                _ => self.layout(body, style),
            },
            Node::Scripts { base, sub, sup } => {
                self.scripts(base, sub.as_deref(), sup.as_deref(), style)
            }
            Node::Frac {
                num,
                den,
                rule,
                style: forced,
            } => self.fraction(num, den, *rule, forced.unwrap_or(style)),
            Node::Sqrt { index, body } => self.sqrt(index.as_deref(), body, style),
            Node::Space(width) => MathBox::space(width * em),
            Node::Accent { body, mark } => self.accent(body, *mark, style),
            Node::Delimited { left, body, right } => {
                let body = self.layout(body, style);
                let axis = self.axis * em;
                let height = 2.0 * (body.ascent - axis).max(body.descent + axis) + 0.1 * em;
                let mut out = MathBox::default();
                let mut x = 0.0;
                for (c, b) in [(*left, None), (None, Some(body)), (*right, None)] {
                    let part = match (c, b) {
                        (Some(c), _) => self.centered(c, em, height, axis),
                        (None, Some(b)) => b,
                        _ => MathBox::space(0.12 * em),
                    };
                    let width = part.width;
                    out.place(part, x, 0.0);
                    x += width;
                }
                out.width = x;
                out
            }
            Node::Big { ch, height, .. } => {
                self.centered(*ch, em, height * em * 0.85, self.axis * em)
            }
            Node::Table { rows, aligns, kind } => self.table(rows, aligns, *kind, style),
        }
    }

    /// Atoms side by side with TeX's spacing. `lead_ord`: an empty atom before them (the
    /// right half of an `align` row starts with `=`).
    fn row(&self, nodes: &[Node], style: Style, lead_ord: bool) -> MathBox {
        let classes: Vec<Option<Class>> = nodes.iter().map(class_of).collect();
        let mut effective = classes.clone();
        let mut previous = lead_ord.then_some(Class::Ord);
        for i in 0..nodes.len() {
            let Some(class) = classes[i] else { continue };
            let mut class = class;
            if class == Class::Bin {
                let next = classes[i + 1..].iter().flatten().next().copied();
                if matches!(
                    previous,
                    None | Some(Class::Bin | Class::Op | Class::Rel | Class::Open | Class::Punct)
                ) || matches!(next, None | Some(Class::Rel | Class::Close | Class::Punct))
                {
                    class = Class::Ord;
                }
            }
            effective[i] = Some(class);
            previous = Some(class);
        }
        let em = self.em(style);
        let mut out = MathBox::default();
        let mut x = 0.0;
        let mut previous = lead_ord.then_some(Class::Ord);
        for (node, class) in nodes.iter().zip(effective) {
            if let Some(class) = class {
                x += spacing(previous, class, style) * em;
                previous = Some(class);
            }
            let part = self.layout(node, style);
            let width = part.width;
            out.place(part, x, 0.0);
            x += width;
        }
        out.width = x;
        out
    }

    fn operator(&self, c: char, style: Style) -> MathBox {
        let em = self.em(style);
        let axis = self.axis * em;
        let display = style == Style::Display;
        if matches!(c, '∫' | '∬' | '∭' | '∮') {
            let height = if display { 2.2 } else { 1.3 } * em;
            let size = if display { 1.1 * em } else { em };
            let mut out = self.centered(c, size, height, axis);
            out.width += 0.1 * em;
            out
        } else {
            let size = if display { 1.6 } else { 1.15 } * em;
            self.centered(c, size, 0.0, axis)
        }
    }

    fn scripts(
        &self,
        base: &Node,
        sub: Option<&Node>,
        sup: Option<&Node>,
        style: Style,
    ) -> MathBox {
        let em = self.em(style);
        let limits = match base {
            Node::Op { limits, .. } => match limits {
                Limits::Always => true,
                Limits::Display => style == Style::Display,
                Limits::Never => false,
            },
            _ => false,
        };
        let base_box = self.layout(base, style);
        let small = style.script();
        let sub = sub.map(|n| self.layout(n, small));
        let sup = sup.map(|n| self.layout(n, small));
        let mut out = MathBox::default();
        if limits {
            let gap = 0.12 * em;
            let width = [Some(&base_box), sub.as_ref(), sup.as_ref()]
                .into_iter()
                .flatten()
                .map(|b| b.width)
                .fold(0.0, f32::max);
            let (base_ascent, base_descent, base_width) =
                (base_box.ascent, base_box.descent, base_box.width);
            out.place(base_box, (width - base_width) / 2.0, 0.0);
            if let Some(sup) = sup {
                let dy = -(base_ascent + gap + sup.descent);
                let x = (width - sup.width) / 2.0;
                out.place(sup, x, dy);
            }
            if let Some(sub) = sub {
                let dy = base_descent + gap + sub.ascent;
                let x = (width - sub.width) / 2.0;
                out.place(sub, x, dy);
            }
            out.width = width;
            return out;
        }
        let mut up = (base_box.ascent - 0.35 * em).max(0.38 * em);
        let mut down = (base_box.descent - 0.1 * em).max(0.2 * em);
        if let Some(sup) = &sup {
            up = up.max(sup.descent + 0.22 * em);
        }
        if let Some(sub) = &sub {
            down = down.max(sub.ascent - 0.5 * em);
            if let Some(sup) = &sup {
                let gap = (up - sup.descent) - (sub.ascent - down);
                if gap < 0.15 * em {
                    down += 0.15 * em - gap;
                }
            }
        }
        let x = base_box.width;
        let scripts_width = sub
            .as_ref()
            .map_or(0.0, |b| b.width)
            .max(sup.as_ref().map_or(0.0, |b| b.width));
        out.place(base_box, 0.0, 0.0);
        if let Some(sup) = sup {
            out.place(sup, x, -up);
        }
        if let Some(sub) = sub {
            out.place(sub, x, down);
        }
        out.width = x + scripts_width + 0.04 * em;
        out
    }

    fn fraction(&self, num: &Node, den: &Node, rule: bool, style: Style) -> MathBox {
        let em = self.em(style);
        let inner = style.fraction();
        let num = self.layout(num, inner);
        let den = self.layout(den, inner);
        let axis = self.axis * em;
        let thickness = if rule { self.rule * em } else { 0.0 };
        let gap = if style == Style::Display { 0.16 } else { 0.1 } * em;
        let pad = 0.12 * em;
        let width = num.width.max(den.width) + 2.0 * pad;
        let mut out = MathBox::default();
        let num_dy = -(axis + thickness / 2.0 + gap + num.descent);
        let den_dy = -axis + thickness / 2.0 + gap + den.ascent;
        let (num_width, den_width) = (num.width, den.width);
        out.place(num, (width - num_width) / 2.0, num_dy);
        out.place(den, (width - den_width) / 2.0, den_dy);
        if rule {
            out.rule(
                0.06 * em,
                -(axis + thickness / 2.0),
                width - 0.12 * em,
                thickness,
            );
        }
        out.width = width;
        out
    }

    fn sqrt(&self, index: Option<&Node>, body: &Node, style: Style) -> MathBox {
        let em = self.em(style);
        let body = self.layout(body, style);
        let thickness = self.rule * em;
        let clearance = thickness + if style == Style::Display { 0.15 } else { 0.1 } * em;
        let top = body.ascent.max(0.45 * em) + clearance + thickness;
        let bottom = body.descent.max(0.05 * em);
        let Some((_, _, ink)) = self.find('√') else {
            return body;
        };
        let stretch = ((top + bottom) / (ink.height() * em)).max(1.0);
        // The sign's ink ends at the top of the bar
        let sign = self
            .glyph('√', em, stretch)
            .lowered(ink.top * em * stretch - top);
        let sign_right = ink.right * em;
        let index = index.map(|n| self.layout(n, Style::ScriptScript));
        let shift = index
            .as_ref()
            .map_or(0.0, |i| (i.width - 0.55 * sign_right).max(0.0));
        let mut out = MathBox::default();
        if let Some(index) = index {
            let dy = -(0.6 * (top + bottom) - bottom) - index.descent;
            out.place(index, 0.0, dy);
        }
        out.place(sign, shift, 0.0);
        let body_x = shift + sign_right + 0.06 * em;
        let body_width = body.width;
        out.place(body, body_x, 0.0);
        let bar_start = shift + sign_right - thickness;
        let bar_end = body_x + body_width + 0.06 * em;
        out.rule(bar_start, -top, bar_end - bar_start, thickness);
        out.width = bar_end + 0.04 * em;
        out
    }

    fn accent(&self, body: &Node, mark: Mark, style: Style) -> MathBox {
        let em = self.em(style);
        let body = self.layout(body, style);
        let thickness = self.rule * em;
        let (ascent, descent, width) = (body.ascent, body.descent, body.width);
        let mut out = MathBox::default();
        out.place(body, 0.0, 0.0);
        out.width = width;
        match mark {
            Mark::Overline => {
                let top = -(ascent.max(0.45 * em) + 0.12 * em + thickness);
                out.rule(0.02 * em, top, width - 0.04 * em, thickness);
            }
            Mark::Underline => {
                out.rule(0.02 * em, descent + 0.1 * em, width - 0.04 * em, thickness)
            }
            Mark::Glyph(c) => {
                let Some((_, _, ink)) = self.find(c) else {
                    return out;
                };
                let size = if c == '→' { 0.8 * em } else { em };
                let bottom = ascent.max(0.45 * em) + 0.06 * em;
                let dy = -(bottom - ink.bottom * size);
                let x = width / 2.0 - (ink.left + ink.right) / 2.0 * size;
                out.place(self.glyph(c, size, 1.0), x, dy);
            }
        }
        out
    }

    fn table(
        &self,
        rows: &[Vec<Node>],
        aligns: &[Align],
        kind: TableKind,
        style: Style,
    ) -> MathBox {
        let em = self.em(style);
        let cell_style = match kind {
            TableKind::Matrix | TableKind::Cases if style == Style::Display => Style::Text,
            _ => style,
        };
        let cells: Vec<Vec<MathBox>> = rows
            .iter()
            .map(|row| {
                row.iter()
                    .enumerate()
                    .map(|(c, cell)| {
                        let nodes = match cell {
                            Node::Row(nodes) => nodes.as_slice(),
                            other => std::slice::from_ref(other),
                        };
                        self.row(nodes, cell_style, kind == TableKind::Aligned && c % 2 == 1)
                    })
                    .collect()
            })
            .collect();
        let columns = cells.iter().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<f32> = (0..columns)
            .map(|c| {
                cells
                    .iter()
                    .filter_map(|r| r.get(c))
                    .map(|b| b.width)
                    .fold(0.0, f32::max)
            })
            .collect();
        let mut xs = Vec::with_capacity(columns);
        // Room between the brackets of a matrix and its entries
        let pad = match kind {
            TableKind::Matrix | TableKind::Cases => 0.15 * em,
            _ => 0.0,
        };
        let mut x = pad;
        for (c, width) in widths.iter().enumerate() {
            if c > 0 {
                x += match kind {
                    TableKind::Aligned if c % 2 == 1 => 0.0,
                    TableKind::Aligned => 1.5 * em,
                    TableKind::Cases => 1.0 * em,
                    _ => 0.9 * em,
                };
            }
            xs.push(x);
            x += width;
        }
        let total_width = x + pad;
        let row_gap = match kind {
            TableKind::Aligned | TableKind::Gather => 0.35 * em,
            _ => 0.2 * em,
        };
        let metrics: Vec<(f32, f32)> = cells
            .iter()
            .map(|r| {
                (
                    r.iter().map(|b| b.ascent).fold(0.7 * em, f32::max),
                    r.iter().map(|b| b.descent).fold(0.25 * em, f32::max),
                )
            })
            .collect();
        let height: f32 = metrics.iter().map(|(a, d)| a + d).sum::<f32>()
            + row_gap * metrics.len().saturating_sub(1) as f32;
        let mut top = -(self.axis * em + height / 2.0);
        let mut out = MathBox::default();
        for (row, (ascent, descent)) in cells.into_iter().zip(metrics) {
            let baseline = top + ascent;
            for (c, cell) in row.into_iter().enumerate() {
                let free = widths[c] - cell.width;
                let x = xs[c]
                    + match aligns.get(c).copied().unwrap_or(Align::Center) {
                        Align::Left => 0.0,
                        Align::Center => free / 2.0,
                        Align::Right => free,
                    };
                out.place(cell, x, baseline);
            }
            top = baseline + descent + row_gap;
        }
        out.ascent = out.ascent.max(self.axis * em + height / 2.0);
        out.descent = out.descent.max(height / 2.0 - self.axis * em);
        out.width = total_width;
        out
    }
}

fn class_of(node: &Node) -> Option<Class> {
    match node {
        Node::Sym(_, class) | Node::Word(_, class) | Node::Big { class, .. } => Some(*class),
        Node::Op { .. } => Some(Class::Op),
        Node::Scripts { base, .. } => class_of(base),
        Node::Space(_) => None,
        _ => Some(Class::Ord),
    }
}

/// Space between two atoms in em (TeX's table; thin = 3/18, medium = 4/18, thick = 5/18).
fn spacing(previous: Option<Class>, next: Class, style: Style) -> f32 {
    use Class::*;
    let Some(previous) = previous else {
        return 0.0;
    };
    const THIN: f32 = 3.0 / 18.0;
    const MEDIUM: f32 = 4.0 / 18.0;
    const THICK: f32 = 5.0 / 18.0;
    let script = style.is_script();
    let (space, in_scripts) = match (previous, next) {
        (Ord, Op) | (Op, Ord) | (Op, Op) | (Close, Op) => (THIN, true),
        (Bin, _) | (_, Bin) => (MEDIUM, false),
        (Rel, Rel) | (Rel, Close) | (Rel, Punct) | (Open, Rel) => (0.0, true),
        (Rel, _) | (_, Rel) => (THICK, false),
        (Punct, _) => (THIN, false),
        _ => (0.0, true),
    };
    if script && !in_scripts {
        0.0
    } else {
        space
    }
}

/// Typesets the LaTeX formula `src` in the font of a text box with em `size`; `display`
/// for a formula on its own line.
pub fn typeset(src: &str, display: bool, font: TextFont, size: f32) -> MathBox {
    let node = parse(src);
    let setter = Typesetter::new(font, size);
    let style = if display { Style::Display } else { Style::Text };
    let mut out = setter.layout(&node, style);
    out.ascent = out.ascent.max(0.0);
    out.descent = out.descent.max(0.0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(content: &str) -> Vec<Segment> {
        split(content)
    }

    #[test]
    fn splits_text_and_formulas() {
        use Segment::*;
        assert_eq!(
            seg("Here is a displayed equation:\n\\[\n\\int_0^1 x^2 \\, dx = \\frac{1}{3}\n\\]"),
            vec![
                Text("Here is a displayed equation:\n".into()),
                Display("\n\\int_0^1 x^2 \\, dx = \\frac{1}{3}\n".into()),
            ]
        );
        assert_eq!(
            seg("Strom $I_B<0$ und \\(U\\)."),
            vec![
                Text("Strom ".into()),
                Inline("I_B<0".into()),
                Text(" und ".into()),
                Inline("U".into()),
                Text(".".into()),
            ]
        );
        assert_eq!(seg("$$a$$"), vec![Display("a".into())]);
        assert_eq!(
            seg("x \\begin{align} a &= b \\end{align}"),
            vec![
                Text("x ".into()),
                Display("\\begin{align} a &= b \\end{align}".into())
            ]
        );
    }

    #[test]
    fn dollars_in_plain_text_stay_text() {
        use Segment::*;
        for text in [
            "it costs $5 and $10",
            "a $ b $ c",
            "price: \\$3",
            "$",
            "$$",
            "a$\nb$",
        ] {
            let segments = seg(text);
            assert!(
                segments.iter().all(|s| matches!(s, Text(_))),
                "{text}: {segments:?}"
            );
        }
        assert_eq!(seg("\\$3"), vec![Text("$3".into())]);
        // Unclosed delimiters stay text
        assert_eq!(seg("\\[ x"), vec![Text("\\[ x".into())]);
    }

    fn text_of(node: &Node) -> String {
        match node {
            Node::Sym(c, _) => c.to_string(),
            Node::Word(w, _) => w.clone(),
            Node::Row(nodes) => nodes.iter().map(text_of).collect(),
            Node::Op { body, .. } => text_of(body),
            Node::Scripts { base, sub, sup } => format!(
                "{}{}{}",
                text_of(base),
                sub.as_ref()
                    .map_or(String::new(), |s| format!("_[{}]", text_of(s))),
                sup.as_ref()
                    .map_or(String::new(), |s| format!("^[{}]", text_of(s)))
            ),
            Node::Frac { num, den, .. } => format!("({})/({})", text_of(num), text_of(den)),
            Node::Sqrt { index, body } => format!(
                "√{}({})",
                index.as_ref().map_or(String::new(), |i| text_of(i)),
                text_of(body)
            ),
            Node::Space(_) => " ".into(),
            Node::Accent { body, .. } => format!("acc({})", text_of(body)),
            Node::Delimited { left, body, right } => format!(
                "{}{}{}",
                left.unwrap_or('.'),
                text_of(body),
                right.unwrap_or('.')
            ),
            Node::Big { ch, .. } => ch.to_string(),
            Node::Table { rows, .. } => rows
                .iter()
                .map(|r| r.iter().map(text_of).collect::<Vec<_>>().join("&"))
                .collect::<Vec<_>>()
                .join(r"\\"),
        }
    }

    #[test]
    fn parses_common_constructs() {
        let cases = [
            (
                r"\int_0^1 x^2 \, dx = \frac{1}{3}",
                "∫_[0]^[1]x^[2] dx=(1)/(3)",
            ),
            ("I_B<0", "I_[B]<0"),
            (r"U_{CE} \leq 0{,}2\,\mathrm{V}", "U_[CE]≤0,2 V"),
            (r"\sqrt[3]{x+1}", "√3(x+1)"),
            (r"\sum_{k=1}^{n} k", "∑_[k=1]^[n]k"),
            (r"\left( \frac{a}{b} \right)^2", "((a)/(b))^[2]"),
            (r"f'(x)", "f′(x)"),
            (r"\alpha \cdot \beta", "α·β"),
            (r"\text{für } x > 0", "für x>0"),
            (r"\lim_{x \to 0} \sin x", "lim_[x→0]sinx"),
            (
                r"\begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}",
                r"(1&2\\3&4)",
            ),
            (r"\mathbb{R}", "ℝ"),
            (r"\foo", r"\foo"),
            (r"x^", "x^[]"),
            (r"\frac{1", "(1)/()"),
        ];
        for (src, expected) in cases {
            assert_eq!(text_of(&parse(src)), expected, "{src}");
        }
    }

    #[test]
    fn parses_environments() {
        let node = parse(r"\begin{align} a &= b + c \\ &= d \end{align}");
        let Node::Table { rows, aligns, kind } = node else {
            panic!("{node:?}");
        };
        assert_eq!(kind, TableKind::Aligned);
        assert_eq!(rows.len(), 2);
        assert_eq!(aligns, vec![Align::Right, Align::Left]);
        // Plain `equation` is just the formula
        assert_eq!(
            text_of(&parse(
                r"\begin{equation} E = mc^2 \label{eq:1} \end{equation}"
            )),
            "E=mc^[2]"
        );
        let cases = parse(r"f(x) = \begin{cases} 1 & x > 0 \\ 0 & \text{sonst} \end{cases}");
        assert_eq!(text_of(&cases), r"f(x)={1&x>0\\0&sonst.");
    }

    fn set(src: &str, display: bool) -> MathBox {
        typeset(src, display, TextFont::Sans, 100.0)
    }

    #[test]
    fn fonts_have_the_symbols() {
        let setter = Typesetter::new(TextFont::Sans, 1.0);
        for c in "αβγδεθλμπσφωΓΔΣΩ∫∑∏√∞≤≥≠≈±·×→⇒∂∇∈∀∃′…−⟨⟩".chars()
        {
            assert!(setter.find(c).is_some(), "{c}");
        }
        assert!(setter.axis > 0.15 && setter.axis < 0.4, "{}", setter.axis);
        assert!(setter.rule > 0.03 && setter.rule < 0.09, "{}", setter.rule);
    }

    #[test]
    fn scripts_are_smaller_and_shifted() {
        let plain = set("x", false);
        let squared = set("x^2", false);
        assert!(squared.width > plain.width);
        assert!(squared.ascent > plain.ascent);
        let two = &squared.glyphs[1];
        assert!(two.size < 100.0 && two.dy < 0.0, "{two:?}");
        let indexed = set("I_B", false);
        assert!(indexed.glyphs[1].dy > 0.0);
        assert!(indexed.descent > 0.0);
    }

    #[test]
    fn fractions_stack_around_a_bar() {
        let frac = set(r"\frac{1}{3}", true);
        assert_eq!(frac.rules.len(), 1);
        let bar = frac.rules[0];
        let [one, three] = [frac.glyphs[0], frac.glyphs[1]];
        assert!(
            one.dy < bar.dy && three.dy > bar.dy + bar.height,
            "{frac:?}"
        );
        // Numerator and denominator centred over each other
        assert!((one.x - three.x).abs() < 5.0);
        // Inline fractions are smaller than display ones
        let inline = set(r"\frac{1}{3}", false);
        assert!(inline.glyphs[0].size < one.size);
        assert!(inline.ascent + inline.descent < frac.ascent + frac.descent);
    }

    #[test]
    fn display_integrals_and_sums_are_large() {
        let inline = set(r"\int_0^1 x", false);
        let display = set(r"\int_0^1 x", true);
        assert!(display.ascent + display.descent > 1.3 * (inline.ascent + inline.descent));
        // Display sums take their limits above and below
        let sum = set(r"\sum_{k=1}^{n} k", true);
        let (sigma, n) = (
            sum.glyphs[0],
            sum.glyphs.iter().find(|g| g.size < 100.0).unwrap(),
        );
        assert!(n.x > sigma.x && n.x < sigma.x + 100.0, "{sum:?}");
        let inline = set(r"\sum_{k=1}^{n} k", false);
        assert!(inline.width > sum.width * 0.9);
    }

    #[test]
    fn roots_have_a_bar_over_the_radicand() {
        let root = set(r"\sqrt{x+1}", false);
        assert_eq!(root.rules.len(), 1);
        let bar = root.rules[0];
        let x = root.glyphs[1];
        assert!(bar.x < x.x && bar.x + bar.width > root.glyphs.last().unwrap().x);
        assert!(bar.dy < -root.ascent * 0.8);
        // A tall radicand stretches the sign
        let tall = set(r"\sqrt{\frac{a}{b}}", true);
        assert!(tall.glyphs[0].stretch > 1.2, "{:?}", tall.glyphs[0]);
    }

    #[test]
    fn brackets_grow_with_their_content() {
        let small = set(r"\left( x \right)", true);
        let big = set(r"\left( \frac{a}{b} \right)", true);
        assert!(small.glyphs[0].stretch < 1.01);
        assert!(big.glyphs[0].stretch > 1.2);
        let matrix = set(r"\begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}", true);
        assert_eq!(matrix.glyphs.len(), 6);
        let [_, one, two, three, ..] = [
            matrix.glyphs[0],
            matrix.glyphs[1],
            matrix.glyphs[2],
            matrix.glyphs[3],
            matrix.glyphs[4],
        ];
        assert!((one.dy - two.dy).abs() < 1e-3 && three.dy > one.dy);
        assert!(three.x < two.x);
    }

    #[test]
    fn relations_get_more_space_than_letters() {
        let tight = set("ab", false);
        let spaced = set("a=b", false);
        let equals = set("=", false);
        assert!(spaced.width > tight.width + equals.width + 0.4 * 100.0);
        // A leading minus is a sign, not an operator
        let negative = set("-a", false);
        let minus = set("-", false);
        let a = set("a", false);
        assert!((negative.width - minus.width - a.width).abs() < 1.0);
    }

    #[test]
    fn aligned_rows_line_up_at_the_equals_signs() {
        let aligned = set(
            r"\begin{aligned} a &= b + c \\ xyz &= d \end{aligned}",
            true,
        );
        let equals: Vec<_> = aligned
            .glyphs
            .iter()
            .filter(|g| g.id == Typesetter::new(TextFont::Sans, 1.0).find('=').unwrap().1)
            .collect();
        assert_eq!(equals.len(), 2);
        assert!((equals[0].x - equals[1].x).abs() < 1e-3);
        assert!(equals[1].dy > equals[0].dy);
    }

    #[test]
    fn pias_example_from_overleaf() {
        let formula = set(r"\int_0^1 x^2 \, dx = \frac{1}{3}", true);
        assert_eq!(formula.rules.len(), 1);
        // ∫ 0 1 x 2 d x = 1 3
        assert_eq!(formula.glyphs.len(), 10);
        assert!(formula.glyphs[0].stretch > 1.3);
    }
}
