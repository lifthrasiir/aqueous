#!/usr/bin/env python3
"""The manual pages (man/) as pages of the site, between its header and footer.

    scripts/man2html.py OUT
    scripts/man2html.py --single FILE

writes OUT/man/aqr.html, aqrsfx.html, aqrattributes.html and index.html (to aqr.html). Each is
mandoc's HTML of the page (`mandoc -T html -O fragment`) inside site/index.html's head, header
and footer (their links made relative to man/), styled as the site is:

- every option (`Fl`) links to where its page tells it (its `.It`), else to where aqr(1) does;
  `Nm aqr` and `Xr` to the pages here (git's to git-scm.com), the specification to ../spec/;
- a table of the pages and of the sections of this one beside it, where there is room.

--single writes all the pages into FILE alone (for Windows, which has no man(1)), linked by
fragments: FILE shows aqr(1), FILE#aqrsfx.5 aqrsfx(5), FILE#aqr.1:o what aqr.html#o is, every id
of a page taking its NAME.SECTION: before it. A page is shown by CSS (`:has(:target)`), with no
script; a browser without `:has` shows them all, one after another. The site's links are made
absolute, its icon a data: URL.

The spec workflow (.github/workflows/spec.yml) runs it, so that nothing generated is committed.
Needs mandoc; only the standard library.
"""

import base64
import html
import os
import re
import subprocess
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


def mandoc(page):
    """mandoc's HTML of man/PAGE (its fragment)."""
    return subprocess.run(
        ["mandoc", "-T", "html", "-O", "fragment", "-I", "os=Aqueous", os.path.join(ROOT, "man", page)],
        check=True,
        capture_output=True,
        text=True,
    ).stdout


def text(fragment):
    """The text of an HTML fragment."""
    return html.unescape(re.sub(r"<[^>]+>", "", fragment)).strip()


def slug(s):
    return re.sub(r"[^A-Za-z0-9.-]+", "_", s).strip("_")


def fix_ids(body):
    """Ids of the text of their element where mandoc's lost it (`[extract]`: `_extract`)."""
    taken = set(re.findall(r' id="([^"]+)"', body))

    def one(m):
        tag, old, inner = m.group(1), m.group(2), m.group(3)
        if not (old.startswith("_") or old.endswith("_")):
            return m.group(0)
        new = slug(text(inner).split(",")[0]) or old
        while new != old and new in taken:
            new += "_"
        taken.add(new)
        inner = inner.replace(f'href="#{old}"', f'href="#{new}"', 1)
        return f'<{tag} id="{new}"{inner}'

    return re.sub(r'<(dt|h2 class="Sh"|h3 class="Ss") id="([^"]+)"(.*?</(?:dt|h2|h3)>)', one, body, flags=re.S)


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
    return body.replace(
        "The Aqueous format specification.", f'The Aqueous <a href="{spec}">format specification</a>.'
    )


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
    body = mandoc(f"{name}.{section}")
    nd = text(re.search(r'<span class="Nd"[^>]*>(.*?)</span>', body, re.S).group(1))
    names = ", ".join(text(n) for n in re.findall(r'<code class="Nm">(.*?)</code>', body.split("</section>")[0]))
    # (mandoc 1.14.6, Ubuntu's: the footer a table and the text a div; later ones: a div and main)
    date = text(re.search(r'<(span|td) class="foot-date">(.*?)</\1>', body, re.S).group(2))
    body = re.search(r'<(main|div) class="manual-text">(.*)</\1>', body, re.S).group(2)
    body = re.sub(r'<section class="Sh">\s*<h2 class="Sh" id="NAME">.*?</section>\s*', "", body, count=1, flags=re.S)
    body = fix_ids(body)
    return body, nd, names, date


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
        f'<li><a href="{ref(n, None)}"{" aria-current=page" if n == name else ""}>{n}({s})</a></li>'
        for n, s in PAGES
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
    if len(sys.argv) == 2 and not sys.argv[1].startswith("-"):
        write(sys.argv[1])
    elif len(sys.argv) == 3 and sys.argv[1] == "--single":
        write_single(sys.argv[2])
    else:
        sys.exit("usage:\n" + "\n".join(__doc__.strip().splitlines()[2:4]))
