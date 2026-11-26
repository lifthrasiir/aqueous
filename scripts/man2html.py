#!/usr/bin/env python3
"""The manual pages (man/) as pages of the site, between its header and footer.

    scripts/man2html.py OUT
    scripts/man2html.py --single FILE

writes OUT/man/aqr.html, aqrsfx.html, aqrattributes.html and index.html (to aqr.html). Each is
the HTML of the page inside site/index.html's head, header and footer (their links made relative
to man/), styled as the site is:

- every option (`Fl`) links to where its page tells it (its `.It`), else to where aqr(1) does;
  `Nm aqr` and `Xr` to the pages here (git's to git-scm.com), the specification to ../spec/;
- a table of the pages and of the sections of this one beside it, where there is room.

--single writes all the pages into FILE alone (for Windows, which has no man(1)), linked by
fragments: FILE shows aqr(1), FILE#aqrsfx.5 aqrsfx(5), FILE#aqr.1:o what aqr.html#o is, every id
of a page taking its NAME.SECTION: before it. A page is shown by CSS (`:has(:target)`), with no
script; a browser without `:has` shows them all, one after another. The site's links are made
absolute, its icon a data: URL.

The pages are rendered here (Mdoc), not by mandoc, so that it runs wherever Python does (the
release's Windows job): only the part of mdoc(7) that the pages use, in the HTML that `mandoc -T
html` makes of it (its classes, ids and permalinks). Anything else in a page (a macro, an escape,
a kind of list or display) is an error naming its line: render it here first.

The spec workflow (.github/workflows/spec.yml) runs it, so that nothing generated is committed.
Only the standard library.
"""

import base64
import html
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The pages, in the order they are listed: (name, section).
PAGES = [("aqr", 1), ("aqrsfx", 5), ("aqrattributes", 5)]
OURS = {n for n, _ in PAGES}

# Where the site is, for the links of --single.
SITE = "https://lifthrasiir.github.io/aqueous/"

# Pages of other projects linked where they are named.
ELSEWHERE = {
    "git": "https://git-scm.com/docs/git",
    "gitattributes": "https://git-scm.com/docs/gitattributes",
    "gitignore": "https://git-scm.com/docs/gitignore",
}

CSS = """
/* manual pages (scripts/man2html.py) */
.man { display: grid; gap: 40px; padding-top: 24px; padding-bottom: 72px; }
@media (min-width: 960px) { .man { grid-template-columns: 200px minmax(0, 1fr); gap: 56px; } }
.man > * { min-width: 0; }
.toc { font-size: 14px; }
@media (min-width: 960px) {
  .toc { position: sticky; top: 24px; align-self: start; max-height: calc(100vh - 48px); overflow-y: auto; }
}
.toc h2 { font-size: 12px; font-weight: 600; text-transform: uppercase; letter-spacing: .05em; color: var(--ink-3); margin: 0 0 8px; }
.toc ul { list-style: none; margin: 0 0 24px; padding: 0; display: grid; gap: 2px; }
.toc ul ul { margin: 2px 0 4px 12px; }
.toc a { color: var(--ink-2); text-decoration: none; display: block; padding: 2px 0; }
.toc a:hover { color: var(--ink); }
.toc .pages a { font-family: var(--mono); }
.toc .pages a[aria-current] { color: var(--accent); font-weight: 650; }
@media (max-width: 959px) { .toc .sections { display: none; } .toc .pages ul { display: flex; flex-wrap: wrap; gap: 4px 20px; } }

.manual { max-width: 46em; }
.manual-head { margin-bottom: 40px; }
.manual-head .kick { margin: 0 0 6px; font-size: 13px; font-weight: 600; text-transform: uppercase; letter-spacing: .05em; color: var(--ink-3); }
.manual-head h1 { margin: 0 0 10px; font: 700 clamp(30px, 5vw, 44px)/1.1 var(--mono); letter-spacing: -.02em; }
.manual-head h1 small { font-size: .55em; color: var(--ink-3); font-weight: 500; }
.manual-head .nd { margin: 0; font-size: clamp(17px, 2.2vw, 19px); color: var(--ink-2); }

.manual section { padding: 0; }
.manual section.Sh { margin: 0 0 40px; }
.manual h2.Sh { font-size: 22px; margin: 0 0 16px; padding-top: 24px; border-top: 1px solid var(--line); letter-spacing: 0; }
.manual h3.Ss { font-size: 17px; margin: 28px 0 10px; }
.manual :target { scroll-margin-top: 16px; }
.manual .permalink { color: inherit; text-decoration: none; }
.manual h2 .permalink:hover::after, .manual h3 .permalink:hover::after { content: " #"; color: var(--ink-3); }
.manual p { margin: 0 0 14px; }
.manual code, .manual .Pa { font-family: var(--mono); font-size: .88em; }
.manual .Pa { color: var(--ink); }
.manual code.Fl, .manual code.Cm, .manual code.Ic, .manual code.Nm, .manual code.Ev { color: var(--ink); font-weight: 600; }
.manual var.Ar { font-style: italic; color: var(--ink-2); }
.manual a.opt code { color: inherit; }
.manual code.Fl, .manual code.Cm, .manual code.Ic { white-space: nowrap; }
.manual a.opt { color: var(--accent); text-decoration: none; }
.manual a.opt:hover { text-decoration: underline; }
.manual .Ql { font-weight: 400; }

.manual dl.Bl-tag { margin: 0 0 16px; }
.manual dl.Bl-tag > dt { margin-top: 14px; font-family: var(--mono); font-size: .92em; }
.manual dl.Bl-tag > dt code { font-size: 1em; }
.manual dl.Bl-tag > dd { margin: 4px 0 0 24px; color: var(--ink-2); }
.manual dl.Bl-tag > dd p { margin: 8px 0; }
.manual dl.Bl-tag > dt:target { background: var(--accent-soft); border-radius: 6px; margin-left: -8px; padding: 2px 8px; }
.manual dl.Bl-compact > dt { margin-top: 4px; }
.manual dl.Bl-compact > dd { margin-top: 0; }
.manual ol.Bl-enum, .manual ul.Bl-bullet { margin: 0 0 16px; padding-left: 24px; color: var(--ink-2); }
.manual ol.Bl-enum > li { margin-bottom: 8px; }
.manual .Bd-indent { margin: 0 0 16px; }
.manual .Bd-indent > dl { margin-left: 8px; }
.manual pre, .manual div.Bd > code.Li {
  display: block; margin: 0 0 16px; background: var(--code-bg); color: var(--code-ink); border-radius: 12px; padding: 16px 18px;
  font: 14px/1.65 var(--mono); overflow-x: auto; white-space: pre;
}
.manual div.Bd + div.Bd > code.Li { margin-top: -12px; }
.manual div.Bd > code.Li { border-radius: 12px; }

.manual table.Nm { width: auto; border-collapse: collapse; margin: 0 0 6px; font: 15px/1.6 var(--sans); font-variant-numeric: normal; }
.manual table.Nm td { border: 0; padding: 0 10px 0 0; vertical-align: top; }
.manual table.Nm td:first-child { white-space: nowrap; }
.manual table.Nm + br { display: none; }
"""

# (--single: a page shown as targeted, or one of its ids; aqr(1) when nothing is)
CSS_SINGLE = """
.mpage:not(:target):not(:has(:target)) { display: none; }
main:not(:has(:target)) .mpage.default { display: grid; }
"""


def text(fragment):
    """The text of an HTML fragment."""
    return html.unescape(re.sub(r"<[^>]+>", "", fragment)).strip()


def slug(s):
    return re.sub(r"[^A-Za-z0-9.-]+", "_", s).strip("_")


class MdocError(Exception):
    pass


# The mdoc(7) macros that may be called on a macro line: those not rendered here are errors, as
# mandoc would take them for macros too.
CALLABLE = set(
    "Ac Ad An Ao Ap Aq Ar At Bc Bo Bq Brc Bro Brq Bsx Bx Cd Cm Dc Do Dq Dv Dx Ec Em En Eo Er Es Ev "
    "Fa Fc Fl Fn Fo Fr Fx Ic In Lb Li Lk Ms Mt Nm No Ns Nx Oc Oo Op Ot Ox Pa Pc Pf Po Pq Qc Ql Qo Qq "
    "Rs Sc So Sq St Sx Sy Ta Tn Ux Va Vt Xc Xo Xr".split()
)

# Macros of words, each the element its words go into.
WORDS = {
    "Ar": ("var", "Ar"),
    "Cm": ("code", "Cm"),
    "Ev": ("code", "Ev"),
    "Fl": ("code", "Fl"),
    "Ic": ("code", "Ic"),
    "Li": ("code", "Li"),
    "Nm": ("code", "Nm"),
    "No": ("span", "No"),
    "Pa": ("span", "Pa"),
}

# Enclosures to the end of the line (before the delimiters that end it): their open and close.
ENCLOSE = {
    "Op": ("[", "]"),
    "Pq": ("(", ")"),
    "Qq": ('"', '"'),
    "Sq": ("&#x2018;", "&#x2019;"),
    "Ql": ('&#x2018;<code class="Li">', "</code>&#x2019;"),
}

OPENING, CLOSING, MIDDLE = {"(", "["}, {".", ",", ":", ";", ")", "]", "?", "!"}, {"|"}

# Enclosures opened and closed by macros of their own, over lines too.
PAIRS = {"Qo": ("Qc", '"', '"'), "Po": ("Pc", "(", ")")}
CLOSERS = {c: o for o, (c, _, _) in PAIRS.items()}

ESCAPES = {"&": "", "e": "\\", "(dq": '"'}


def permalink(id_, h):
    return f'<a class="permalink" href="#{id_}">{h}</a>'


class Mdoc:
    """The subset of mdoc(7) that man/'s pages use, rendered as HTML of the classes mandoc gives
    (`mandoc -T html`), which the site's style is written for. Anything else (a macro, an
    escape, a list or display of another kind) is an error, naming the file and the line."""

    def __init__(self, path, reserved=frozenset()):
        self.path = path
        self.reserved = reserved  # the ids of headings and tags as they are written
        self.out = []
        self.names, self.nd, self.date, self.default = [], "", "", None
        self.taken, self.refs, self.clean = set(), [], set()
        self.sh = None  # the section's title
        self.ss = False  # in a subsection
        self.blocks = []  # lists and their items: ["tag"|"enum", item open]
        self.para = "want"  # "want" (a paragraph opened by what comes), "open", or None (bare)
        self.space = False  # a space before the next word
        self.synopsis = False  # in a table of the synopsis' Nm
        self.literal = None  # the lines of a literal display
        self.pairs = []  # the enclosures of PAIRS open

    def fail(self, msg):
        raise MdocError(f"{self.path}:{self.line}: {msg}")

    # Words and escapes

    def unescape(self, s):
        def one(m):
            e = m.group(1)
            if e not in ESCAPES:
                self.fail(f"escape \\{e} not known")
            return ESCAPES[e]

        if re.search(r"\\(?!&|e|\(dq)", s):
            self.fail(f"escape not known in {s!r}")
        return html.escape(re.sub(r"\\(&|e|\(dq)", one, s), quote=False)

    def tokens(self, s):
        """A macro line's arguments: (raw, quoted)."""
        out, i = [], 0
        while i < len(s):
            if s[i] == " ":
                i += 1
            elif s[i] == '"':
                j, word = i + 1, ""
                while True:
                    if j >= len(s):
                        self.fail("a quoted argument not ended")
                    if s[j] == '"':
                        if s[j + 1 : j + 2] == '"':
                            word, j = word + '"', j + 2
                            continue
                        break
                    word, j = word + s[j], j + 1
                out.append((word, True))
                i = j + 1
            else:
                j = s.find(" ", i)
                j = len(s) if j < 0 else j
                out.append((s[i:j], False))
                i = j
        return out

    # Output

    def nl(self, h):
        """HTML of a block, on a line of its own."""
        self.out.append("\n" + h)

    def block(self, h):
        """HTML of a block, ending the paragraph."""
        self.end_para()
        self.nl(h)
        self.space = False

    def end_para(self):
        if self.para == "open":
            self.nl("</p>")
            self.para = "want"
        if self.synopsis:
            self.nl("</td>\n  </tr>\n</table>")
            self.synopsis = False

    def word(self, h, kind="word"):
        """HTML of the flow: a word, or a delimiter (opening, closing, middle)."""
        if self.sh is None:
            self.fail("text before the first section")
        if self.para == "want" and not self.synopsis:
            self.out.append('<p class="Pp">')
            self.para, self.space = "open", False
        if self.space and kind != "closing":
            self.out.append(" ")
        self.out.append(h)
        self.space = kind != "opening"

    # Lines

    def render(self):
        with open(self.path, encoding="utf-8") as f:
            lines = f.read().split("\n")
        if lines and lines[-1] == "":
            lines.pop()
        for self.line, s in enumerate(lines, 1):
            if self.literal is not None:
                if s == ".Ed":
                    self.block(f"<pre>{chr(10).join(self.literal)}</pre>\n</div>")
                    self.literal = None
                elif s.startswith("."):
                    self.fail("a macro in a literal display")
                else:
                    self.literal.append(self.unescape(s))
            elif s.startswith('.\\"'):
                continue
            elif s.startswith("."):
                self.macro_line(s[1:])
            elif s == "" or s[0] in " \t":
                self.fail("a blank line or one starting with a space")
            else:
                self.word(self.unescape(s))
        self.line = len(lines)
        if self.blocks or self.literal is not None or self.pairs:
            self.fail("a list, a display or an enclosure not ended")
        self.end_section(True)
        ids = self.taken
        for r in self.refs:
            if r not in ids:
                raise MdocError(f"{self.path}: Sx {r} names no section")
        return "".join(self.out).lstrip("\n")

    def heading(self, args):
        if any(not q and t in CALLABLE for t, q in args):
            self.fail("a macro in a section's title")
        title = " ".join(self.unescape(t) for t, _ in args)
        if not title:
            self.fail("a section without a title")
        return title

    def new_id(self, s):
        """The id of a heading or a tag: its text (`~2` and so on after the first), but where
        that is not a valid id as it is (`{cwd}`, `[run]`), its letters followed by `_`s so as
        not to take another's (`cwd_`)."""
        s = html.unescape(s)
        base = slug(s)
        if not base:
            self.fail(f"no id of {s!r}")
        id_, n = base, 1
        if base == s:
            self.clean.add(base)
            while id_ in self.taken:
                n += 1
                id_ = f"{base}~{n}"
        else:
            while id_ in self.taken or id_ in self.reserved:
                id_ += "_"
        self.taken.add(id_)
        return id_

    def end_section(self, sh):
        if self.blocks:
            self.fail("a section within a list")
        self.end_para()
        if self.ss:
            self.nl("</section>")
            self.ss = False
        if sh and self.sh not in (None, "NAME"):
            self.nl("</section>")

    def macro_line(self, s):
        args = self.tokens(s)
        if not args:
            self.fail("an empty macro line")
        (name, _), rest = args[0], args[1:]
        if name == "Dd":
            self.date = " ".join(t for t, _ in rest)
        elif name in ("Dt", "Os"):
            pass
        elif name == "Sh":
            title = self.heading(rest)
            self.end_section(True)
            self.sh, self.para = title, "want"
            if title != "NAME":
                id_ = self.new_id(title)
                self.nl(f'<section class="Sh">\n<h2 class="Sh" id="{id_}">{permalink(id_, title)}</h2>')
        elif name == "Ss":
            title = self.heading(rest)
            self.end_section(False)
            id_ = self.new_id(title)
            self.nl(f'<section class="Ss">\n<h3 class="Ss" id="{id_}">{permalink(id_, title)}</h3>')
            self.ss, self.para = True, "want"
        elif self.sh == "NAME":
            if name == "Nm" and rest:
                self.names.append(self.unescape(rest[0][0]))
                self.default = self.default or self.names[0]
            elif name == "Nd":
                self.nd = " ".join(self.unescape(t) for t, _ in rest)
            else:
                self.fail(f"{name} in NAME")
        elif name == "Pp":
            self.end_para()
            self.para, self.space = "want", False
        elif name == "Bl":
            self.list_begin(rest)
        elif name == "It":
            self.item(rest)
        elif name == "El":
            if not self.blocks:
                self.fail("El of no list")
            kind, item, indent = self.blocks.pop()
            self.end_para()
            if item:
                self.nl("</dd>" if kind == "tag" else "</li>")
            self.nl("</dl>" if kind == "tag" else "</ol>")
            if indent:
                self.nl("</div>")
            self.para = "want" if not self.blocks else None
        elif name == "Bd":
            if [t for t, _ in rest] != ["-literal", "-offset", "indent"]:
                self.fail("a display but -literal -offset indent")
            self.block('<div class="Bd Pp Bd-indent Li">')
            self.literal = []
        elif name == "Dl":
            self.end_para()
            saved, self.para = self.para, None
            self.nl('<div class="Bd Bd-indent"><code class="Li">')
            self.space = False
            self.inline(rest)
            self.nl("</code></div>")
            self.para, self.space = saved if saved != "open" else "want", False
        elif name == "Nm" and self.sh == "SYNOPSIS":
            self.end_para()
            nm = self.unescape(rest[0][0]) if rest else self.default
            self.nl(f'<table class="Nm">\n  <tr>\n    <td><code class="Nm">{nm}</code></td>\n    <td>')
            self.synopsis, self.space = True, False
            self.inline(rest[1:])
        elif name in CALLABLE:
            self.inline(args)
        else:
            self.fail(f"macro {name} not known")

    def list_begin(self, args):
        opts = [t for t, _ in args]
        kind = {"-tag": "tag", "-enum": "enum"}.get(opts[0] if opts else "")
        if kind is None:
            self.fail(f"a list of {opts[:1]} (only -tag and -enum)")
        classes, indent, i = [f"Bl-{kind}"], False, 1
        while i < len(opts):
            if opts[i] == "-width" and i + 1 < len(opts):
                i += 2
            elif opts[i] == "-offset" and opts[i + 1 : i + 2] == ["indent"]:
                indent, i = True, i + 2
            elif opts[i] == "-compact":
                classes.append("Bl-compact")
                i += 1
            else:
                self.fail(f"list option {opts[i]} not known")
        tag = "dl" if kind == "tag" else "ol"
        self.block(('<div class="Bd-indent">\n' if indent else "") + f'<{tag} class="{" ".join(classes)}">')
        self.blocks.append([kind, False, indent])

    def item(self, args):
        if not self.blocks:
            self.fail("It out of a list")
        b = self.blocks[-1]
        self.end_para()
        if b[1]:
            self.nl("</dd>" if b[0] == "tag" else "</li>")
        b[1] = True
        if b[0] == "enum":
            if args:
                self.fail("It of an -enum list with arguments")
            self.nl("  <li>")
            self.para, self.space = None, False
            return
        if not args:
            self.fail("It of a -tag list without a head")
        start = len(self.out)
        self.para, self.space = None, False
        self.inline(args)
        head = "".join(self.out[start:])
        del self.out[start:]
        first = args[0]
        # (as mandoc: of a macro of words but Ar and No, by its first)
        if not first[1] and first[0] in WORDS.keys() - {"Ar", "No"} and len(args) > 1:
            id_ = self.new_id(args[1][0].lstrip("-") if first[0] == "Fl" else self.unescape(args[1][0]))
            m = re.match(r"<(code|var|span) [^>]*>.*?</\1>", head)
            if m is None:
                self.fail("no element to link in the head")
            head = permalink(id_, m.group(0)) + head[m.end() :]
            self.nl(f'  <dt id="{id_}">{head}</dt>\n  <dd>')
        else:
            self.nl(f"  <dt>{head}</dt>\n  <dd>")
        self.space = False

    def inline(self, args):
        """A macro line of callable macros, words and delimiters."""
        args = list(args)

        def delim(a):
            t, q = a
            if q:
                return None
            return "opening" if t in OPENING else "closing" if t in CLOSING else "middle" if t in MIDDLE else None

        trail = []
        while args and delim(args[-1]) == "closing":
            trail.insert(0, args.pop())
        closes = []  # the enclosures open
        cur, words, had = None, [], False

        def flush():
            nonlocal words, had
            if cur in WORDS:
                tag, cls = WORDS[cur]
                if not words and not had:
                    if cur == "Nm" and self.default:
                        words = [self.default]
                    elif cur == "Fl":
                        words = [""]
                    else:
                        self.fail(f"{cur} without arguments")
                if cur == "Fl":
                    for w in words:
                        self.word(f'<code class="Fl">-{w}</code>')
                elif words:
                    self.word(f'<{tag} class="{cls}">{" ".join(words)}</{tag}>')
            elif cur == "Sx":
                if not words:
                    self.fail("Sx without a title")
                title = " ".join(words)
                id_ = slug(html.unescape(title))
                self.refs.append(id_)
                self.word(f'<a class="Sx" href="#{id_}">{title}</a>')
            else:
                for w in words:
                    self.word(w)
            had = had or bool(words)
            words = []

        i = 0
        while i < len(args):
            t, q = args[i]
            if not q and t in CALLABLE:
                flush()
                cur, had = None, False
                if t == "Ns":
                    self.space = False
                elif t == "Pf":
                    if i + 1 >= len(args):
                        self.fail("Pf without a prefix")
                    self.word(self.unescape(args[i + 1][0]), "opening")
                    i += 1
                elif t in ENCLOSE:
                    self.word(ENCLOSE[t][0], "opening")
                    closes.append(t)
                elif t in PAIRS:
                    self.word(PAIRS[t][1], "opening")
                    self.pairs.append(t)
                elif t in CLOSERS:
                    if not self.pairs or self.pairs[-1] != CLOSERS[t]:
                        self.fail(f"{t} of no {CLOSERS[t]}")
                    self.word(PAIRS[self.pairs.pop()][2], "closing")
                elif t == "Xr":
                    if len(args) - i < 3:
                        self.fail("Xr without a name and a section")
                    self.word(f'<a class="Xr">{self.unescape(args[i + 1][0])}({self.unescape(args[i + 2][0])})</a>')
                    i += 2
                elif t in WORDS or t == "Sx":
                    cur = t
                else:
                    self.fail(f"macro {t} not known")
            elif delim(args[i]):
                flush()
                self.word(t, delim(args[i]))
            else:
                words.append(self.unescape(t))
            i += 1
        flush()
        for c in reversed(closes):
            self.word(ENCLOSE[c][1], "closing")
        for t, _ in trail:
            self.word(t, "closing")


def options(body):
    """The options a page tells, each its id: the `Fl`s of each `.It` (`-c` and `--compress`)."""
    out = {}
    for m in re.finditer(r'<dt id="([^"]+)">(.*?)</dt>', body, re.S):
        for fl in re.findall(r'<code class="Fl">(.*?)</code>', m.group(2), re.S):
            out.setdefault(text(fl), m.group(1))
    return out


def link_options(body, own, aqr, ref):
    """Each `Fl` out of its own `.It`, linked to where it is told: of this page, else of aqr(1).
    REF(page, id) is the link to an id of a page (of this one if PAGE is None)."""
    parts = re.split(r"(<dt\b.*?</dt>|<a\b.*?</a>)", body, flags=re.S)

    def one(m):
        t = text(m.group(1))
        if t in own:
            href = ref(None, own[t])
        elif t in aqr:
            href = ref("aqr", aqr[t])
        else:
            return m.group(0)
        return f'<a class="opt" href="{href}">{m.group(0)}</a>'

    return "".join(
        p if i % 2 else re.sub(r'<code class="Fl">(.*?)</code>', one, p, flags=re.S) for i, p in enumerate(parts)
    )


def link_names(body, name, ref, spec):
    """`Nm aqr` of the other pages to aqr(1); `Xr` to the pages here or elsewhere, else no link.
    REF(page, None) is the link to a page, SPEC to the specification."""

    def xr(m):
        label = m.group(1)
        target = text(label).split("(")[0]
        if target in OURS:
            return f'<a class="Xr" href="{ref(target, None)}">{label}</a>'
        if target in ELSEWHERE:
            return f'<a class="Xr" href="{ELSEWHERE[target]}">{label}</a>'
        return f'<span class="Xr">{label}</span>'

    body = re.sub(r'<a class="Xr"[^>]*>(.*?)</a>', xr, body, flags=re.S)
    if name != "aqr":
        nm = '<code class="Nm">aqr</code>'
        body = body.replace(nm, f'<a class="opt" href="{ref("aqr", None)}">{nm}</a>')
    return body.replace("The Aqueous format specification.", f'The Aqueous <a href="{spec}">format specification</a>.')


def toc(body):
    """The sections and subsections of a page, as nested lists."""
    out, open_sub = [], False
    for m in re.finditer(r'<(h2|h3) class="(?:Sh|Ss)" id="([^"]+)">(.*?)</\1>', body, re.S):
        level, id_, label = m.group(1), m.group(2), html.escape(text(m.group(3)))
        if level == "h2":
            if open_sub:
                out.append("</ul></li>")
                open_sub = False
            elif out:
                out.append("</li>")
            out.append(f'<li><a href="#{id_}">{label}</a>')
        else:
            if not open_sub:
                out.append("<ul>")
                open_sub = True
            out.append(f'<li><a href="#{id_}">{label}</a></li>')
    out.append("</ul></li>" if open_sub else "</li>")
    return "<ul>" + "".join(out) + "</ul>"


def site_parts(up_link):
    """site/index.html's head (to its style's end), its sprite of icons with the droplet the
    header draws, its header and its footer, each relative link of the last two as UP_LINK makes
    it (of the site's root: `#`, `spec/`, `man/`)."""
    with open(os.path.join(ROOT, "site", "index.html"), encoding="utf-8") as f:
        page = f.read()
    head = page[: page.index("</style>")]
    sprite = re.search(r'<svg width="0" height="0".*?</svg>', page, re.S).group(0)
    drop = re.search(r'<svg id="drop-svg"[^>]*>(.*?)</svg>', page, re.S).group(1)
    # (its gradients' ids, of a letter, would be those of options: `#f`)
    for g in re.findall(r'<(?:radial|linear)Gradient id="([^"]+)"', drop):
        drop = drop.replace(f'id="{g}"', f'id="drop-{g}"').replace(f"url(#{g})", f"url(#drop-{g})")
    sprite = sprite.replace("</defs>", drop.strip() + "\n  </defs>", 1)
    header = re.search(r'<header class="wrap top">.*?</header>', page, re.S).group(0)
    footer = re.search(r"<footer>.*?</footer>", page, re.S).group(0)

    def up(part):
        # (of links alone: the droplet is `<use href="#drop-art">`)
        def one(m):
            h = m.group(2)
            if re.match(r"[a-z]+:", h):
                return m.group(0)
            return f'{m.group(1)}{up_link("" if h == "#" else h)}"'

        return re.sub(r'(<a\b[^>]*\bhref=")([^"]*)"', one, part)

    return head, sprite, up(header), up(footer)


def page_html(name, section):
    """The HTML of man/NAME.SECTION but its NAME section, its description, names and date."""
    path = os.path.join(ROOT, "man", f"{name}.{section}")
    # (twice: the ids of headings and tags as written known before those made of others)
    first = Mdoc(path)
    first.render()
    m = Mdoc(path, frozenset(first.clean))
    body = m.render()
    return body, html.unescape(m.nd), html.unescape(", ".join(m.names)), m.date


def prefixed(body, prefix):
    """BODY's ids and links within it, PREFIX before each."""
    body = re.sub(r' id="([^"]+)"', lambda m: f' id="{prefix}{m.group(1)}"', body)
    return re.sub(r' href="#([^"]+)"', lambda m: f' href="#{prefix}{m.group(1)}"', body)


def head_of(head, title, desc, nd):
    """The site's HEAD with the title and descriptions of a page."""
    h = re.sub(r"<title>.*?</title>", f"<title>{html.escape(title)} · Aqueous</title>", head)
    h = re.sub(r'(<meta name="description" content=")[^"]*', lambda m: m.group(1) + html.escape(desc), h)
    h = re.sub(r'(<meta property="og:title" content=")[^"]*', lambda m: m.group(1) + html.escape(title), h)
    return re.sub(r'(<meta property="og:description" content=")[^"]*', lambda m: m.group(1) + html.escape(nd), h)


def manual(rendered, name, section, ref, spec, single=None):
    """The pages' table and the page NAME, as the `.man` block of a document; SINGLE the classes
    of the block in a document of all the pages (its ids then prefixed)."""
    body, nd, names, date = rendered[name]
    aqr_options = options(rendered["aqr"][0])
    own = options(body)
    attrs = ""
    if single is not None:
        body = prefixed(body, f"{name}.{section}:")
        attrs = f' id="{name}.{section}" class="wrap man {single}"'
    body = link_names(link_options(body, own, aqr_options, ref), name, ref, spec)
    pages = "".join(
        f'<li><a href="{ref(n, None)}"{" aria-current=page" if n == name else ""}>{n}({s})</a></li>' for n, s in PAGES
    )
    return f"""<div{attrs or ' class="wrap man"'}>
  <aside class="toc">
    <nav class="pages" aria-label="Manual pages"><h2>Manual</h2><ul>{pages}</ul></nav>
    <nav class="sections" aria-label="On this page"><h2>On this page</h2>{toc(body)}</nav>
  </aside>
  <article class="manual">
    <header class="manual-head">
      <p class="kick">Section {section} · {html.escape(date)}</p>
      <h1>{html.escape(names)}<small>({section})</small></h1>
      <p class="nd">{html.escape(nd[:1].upper() + nd[1:])}.</p>
    </header>
{body.strip()}
  </article>
</div>"""


def document(head, css, sprite, header, mains, footer):
    return f"""{head}{css}</style>
</head>
<body>

{sprite}

{header}

<main>
{mains}
</main>

{footer}
</body>
</html>
"""


def write(out):
    rendered = {name: page_html(name, s) for name, s in PAGES}
    head, sprite, header, footer = site_parts(lambda h: f"../{h}")
    os.makedirs(os.path.join(out, "man"), exist_ok=True)
    for name, section in PAGES:

        def ref(page, id_):
            return (f"{page}.html" if page else "") + (f"#{id_}" if id_ else "")

        nd = rendered[name][1]
        title = f"{name}({section})"
        h = head_of(head, title, f"{title}: {nd}. A manual page of Aqueous.", nd)
        h = re.sub(r'(<link rel="canonical" href="[^"]*/)"', rf'\1man/{name}.html"', h)
        h = h.replace('href="droplet.svg"', 'href="../droplet.svg"')
        doc = document(h, CSS, sprite, header, manual(rendered, name, section, ref, "../spec/"), footer)
        with open(os.path.join(out, "man", f"{name}.html"), "w", encoding="utf-8") as f:
            f.write(doc)
    with open(os.path.join(out, "man", "index.html"), "w", encoding="utf-8") as f:
        f.write(
            '<!doctype html>\n<meta charset="utf-8">\n<title>aqr(1) · Aqueous</title>\n'
            '<link rel="canonical" href="aqr.html">\n<meta http-equiv="refresh" content="0; url=aqr.html">\n'
            "<script>location.replace('aqr.html' + location.hash)</script>\n"
            '<p><a href="aqr.html">aqr(1)</a></p>\n'
        )


def write_single(path):
    rendered = {name: page_html(name, s) for name, s in PAGES}
    sections = dict(PAGES)

    def up_link(h):
        # (the manual's own links to its fragments: `man/`, `man/aqr.html#o`)
        m = re.fullmatch(r"man/(?:([a-z]+)\.html)?(?:#(.*))?", h)
        if m:
            return ref(m.group(1) or "aqr", m.group(2), None)
        return SITE + h

    def ref(page, id_, name):
        page = page or name
        return f"#{page}.{sections[page]}" + (f":{id_}" if id_ else "")

    head, sprite, header, footer = site_parts(up_link)
    listed = ", ".join(f"{n}({s})" for n, s in PAGES)
    h = head_of(head, "Manual", f"The manual pages of Aqueous: {listed}.", "The manual pages.")
    h = re.sub(r'<link rel="canonical" href="[^"]*">', f'<link rel="canonical" href="{SITE}man/">', h)
    h = re.sub(r'<meta property="og:image" content="[^"]*">\n', "", h)
    with open(os.path.join(ROOT, "droplet.svg"), "rb") as f:
        icon = "data:image/svg+xml;base64," + base64.b64encode(f.read()).decode()
    h = h.replace('href="droplet.svg"', f'href="{icon}"')
    mains = "\n".join(
        manual(
            rendered,
            name,
            section,
            lambda page, id_, name=name: ref(page, id_, name),
            SITE + "spec/",
            "mpage default" if name == "aqr" else "mpage",
        )
        for name, section in PAGES
    )
    with open(path, "w", encoding="utf-8") as f:
        f.write(document(h, CSS + CSS_SINGLE, sprite, header, mains, footer))


if __name__ == "__main__":
    try:
        if len(sys.argv) == 2 and not sys.argv[1].startswith("-"):
            write(sys.argv[1])
        elif len(sys.argv) == 3 and sys.argv[1] == "--single":
            write_single(sys.argv[2])
        else:
            sys.exit("usage:\n" + "\n".join(__doc__.strip().splitlines()[2:4]))
    except MdocError as e:
        sys.exit(f"man2html.py: {e}")
