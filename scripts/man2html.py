#!/usr/bin/env python3
"""The manual pages (man/) as pages of the site, between its header and footer.

    scripts/man2html.py OUT

writes OUT/man/aqr.html, aqrsfx.html, aqrattributes.html and index.html (to aqr.html). Each is
mandoc's HTML of the page (`mandoc -T html -O fragment`) inside site/index.html's head, header
and footer (their links made relative to man/), styled as the site is:

- every option (`Fl`) links to where its page tells it (its `.It`), else to where aqr(1) does;
  `Nm aqr` and `Xr` to the pages here (git's to git-scm.com), the specification to ../spec/;
- a table of the pages and of the sections of this one beside it, where there is room.

The spec workflow (.github/workflows/spec.yml) runs it, so that nothing generated is committed.
Needs mandoc; only the standard library.
"""

import html
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The pages, in the order they are listed: (name, section).
PAGES = [("aqr", 1), ("aqrsfx", 5), ("aqrattributes", 5)]
OURS = {n for n, _ in PAGES}

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


def link_options(body, own, aqr):
    """Each `Fl` out of its own `.It`, linked to where it is told: of this page, else of aqr(1)."""
    parts = re.split(r"(<dt\b.*?</dt>|<a\b.*?</a>)", body, flags=re.S)

    def one(m):
        t = text(m.group(1))
        if t in own:
            href = f"#{own[t]}"
        elif t in aqr:
            href = f"aqr.html#{aqr[t]}"
        else:
            return m.group(0)
        return f'<a class="opt" href="{href}">{m.group(0)}</a>'

    return "".join(
        p if i % 2 else re.sub(r'<code class="Fl">(.*?)</code>', one, p, flags=re.S) for i, p in enumerate(parts)
    )


def link_names(body, name):
    """`Nm aqr` of the other pages to aqr(1); `Xr` to the pages here or elsewhere, else no link."""

    def xr(m):
        label = m.group(1)
        target = text(label).split("(")[0]
        if target in OURS:
            return f'<a class="Xr" href="{target}.html">{label}</a>'
        if target in ELSEWHERE:
            return f'<a class="Xr" href="{ELSEWHERE[target]}">{label}</a>'
        return f'<span class="Xr">{label}</span>'

    body = re.sub(r'<a class="Xr"[^>]*>(.*?)</a>', xr, body, flags=re.S)
    if name != "aqr":
        nm = '<code class="Nm">aqr</code>'
        body = body.replace(nm, f'<a class="opt" href="aqr.html">{nm}</a>')
    return body.replace(
        "The Aqueous format specification.", 'The Aqueous <a href="../spec/">format specification</a>.'
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


def site_parts():
    """site/index.html's head (to its style's end), its sprite of icons with the droplet the
    header draws, its header and its footer, their links made relative to man/."""
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
            return f'{m.group(1)}../{"" if h == "#" else h}"'

        return re.sub(r'(<a\b[^>]*\bhref=")([^"]*)"', one, part)

    return head, sprite, up(header), up(footer)


def page_html(name, section):
    body = mandoc(f"{name}.{section}")
    nd = text(re.search(r'<span class="Nd"[^>]*>(.*?)</span>', body, re.S).group(1))
    names = ", ".join(text(n) for n in re.findall(r'<code class="Nm">(.*?)</code>', body.split("</section>")[0]))
    date = text(re.search(r'<span class="foot-date">(.*?)</span>', body, re.S).group(1))
    body = re.search(r'<main class="manual-text">(.*)</main>', body, re.S).group(1)
    body = re.sub(r'<section class="Sh">\s*<h2 class="Sh" id="NAME">.*?</section>\s*', "", body, count=1, flags=re.S)
    body = fix_ids(body)
    return body, nd, names, date


def write(out):
    rendered = {name: page_html(name, s) for name, s in PAGES}
    aqr_options = options(rendered["aqr"][0])
    head, sprite, header, footer = site_parts()
    os.makedirs(os.path.join(out, "man"), exist_ok=True)
    for name, section in PAGES:
        body, nd, names, date = rendered[name]
        body = link_names(link_options(body, options(body), aqr_options), name)
        title = f"{name}({section})"
        desc = f"{title}: {nd}. A manual page of Aqueous."
        h = re.sub(r"<title>.*?</title>", f"<title>{html.escape(title)} · Aqueous</title>", head)
        h = re.sub(r'(<meta name="description" content=")[^"]*', lambda m: m.group(1) + html.escape(desc), h)
        h = re.sub(r'(<meta property="og:title" content=")[^"]*', lambda m: m.group(1) + html.escape(title), h)
        h = re.sub(r'(<meta property="og:description" content=")[^"]*', lambda m: m.group(1) + html.escape(nd), h)
        h = re.sub(r'(<link rel="canonical" href="[^"]*/)"', rf'\1man/{name}.html"', h)
        h = h.replace('href="droplet.svg"', 'href="../droplet.svg"')
        pages = "".join(
            f'<li><a href="{n}.html"{" aria-current=page" if n == name else ""}>{n}({s})</a></li>' for n, s in PAGES
        )
        doc = f"""{h}{CSS}</style>
</head>
<body>

{sprite}

{header}

<main>
<div class="wrap man">
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
</div>
</main>

{footer}
</body>
</html>
"""
        with open(os.path.join(out, "man", f"{name}.html"), "w", encoding="utf-8") as f:
            f.write(doc)
    with open(os.path.join(out, "man", "index.html"), "w", encoding="utf-8") as f:
        f.write(
            '<!doctype html>\n<meta charset="utf-8">\n<title>aqr(1) · Aqueous</title>\n'
            '<link rel="canonical" href="aqr.html">\n<meta http-equiv="refresh" content="0; url=aqr.html">\n'
            "<script>location.replace('aqr.html' + location.hash)</script>\n"
            '<p><a href="aqr.html">aqr(1)</a></p>\n'
        )


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__.strip().splitlines()[2].strip())
    write(sys.argv[1])
