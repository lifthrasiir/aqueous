# The `xml-recons` module

*Draft: no module of this version is built yet; it may change until one is.*

This document specifies the module `xml-recons` of aqr-formula entry formulas. The module
rebuilds XML (or any text of tags) from a *skeleton*, the text with each element's text and each
attribute's value cut out and its structure marked, and the values cut out, kept in *containers*
by the context they were in (after XMill: Liefke and Suciu, "XMill: an Efficient Compressor for
XML Data", 2000). The document is meant to be complete: an implementation of the module can be
written from it alone. Sections marked *(encoder)* describe the choices this implementation
makes when it builds formulas; the module does not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Streams](#3-streams)
4. [Contexts and containers](#4-contexts-and-containers)
5. [Rebuilding the text](#5-rebuilding-the-text)
6. [Encoding](#6-encoding-encoder)

## 1. Conventions

- Integers are unsigned. **varint**: unsigned LEB128, least significant group first, at most
  10 bytes, of at most 64 bits. A writer always uses the shortest form.
- `LF` is the byte 0x0A. **Space** is any of 0x20, 0x09, 0x0D and 0x0A. A **quote** is `"`
  (0x22) or `'` (0x27).
- The *special* bytes are 0x01 to 0x05: VALUE (0x01), OPEN (0x02), CLOSE (0x03), END (0x04)
  and ESC (0x05). XML 1.0 allows none of them in a document, so the skeleton marks its
  structure by them.
- A **name** is one or more bytes, none of them a space, `=`, `/`, `>`, `<`, a quote or a
  special byte.
- A **string** of the params is a varint `n` followed by `n` bytes.

## 2. The module

- **Module**: name `xml-recons`, version 1.
- **Params**, in order:
  - varint `containers`: the number of containers (§4);
  - varint `depth`: the most elements open at once while the text is rebuilt (the stack's
    bound, for the memory plan); more is an error;
  - varint `k`, then `k` strings: the names of the elements open where the text starts, the
    outermost first (§4; `k` at most `depth`).

  Nothing may follow.
- **Resources**: none.
- **Inputs**: four, in order: `skeleton`, `table`, `lens` and `values` (§3).
- **Output**: the text of the declared length (§5). Any inconsistency (a stream ending early or
  with bytes left, a container's part not read to its end, a length other than the declared
  one) is an error.

## 3. Streams

- **skeleton**: the text with the values cut out and its structure marked (§5).
- **table**: of each container, in the order containers are made (§4):

  ```text
  u8      flags    bit 0: its values' lengths are in `lens`; else each value ends at LF
  varint  bytes    its part of `values`
  ```

  Other flags are errors. The table has exactly `containers` entries.
- **lens**: of each value of a container whose flags have bit 0, in the order the values are
  taken (§5), a varint: its length.
- **values**: the parts of the containers one after the other, in the order of the table, of
  the lengths it gives; they add up to the stream's length. A container's part is its values one
  after the other, in the order they are taken: a value of a container of bit 0 is its length's
  bytes; any other value is its bytes up to LF, the LF taken from the part but not part of the
  value.

## 4. Contexts and containers

While the text is rebuilt, a *stack* of names is kept: the elements open, the innermost last. It
starts as the names of the params, and OPEN pushes a name and CLOSE and END pop one (§5).

Every value is taken in a *context*, a key of three parts:

- a text, taken by VALUE outside a tag: (0, p, e), where e is the innermost name of the stack
  and p the one before it (empty where there is none). VALUE with the stack empty is an error.
- an attribute's value, taken in a tag of OPEN: (1, e, a), where e is the tag's name and a the
  attribute's.

Each context has a container. Containers are numbered from 0 in the order they are *made*: the
first value taken in a context makes its container, the next of the table, whose flags it has;
making a container when all of the table's are made is an error. A value is taken from the next
bytes of its container's part (and, where its flags have bit 0, the next varint of `lens`).

At the end, every container of the table must be made, every part read to its end, and `lens`
too.

## 5. Rebuilding the text

The skeleton is read from its start, and at each step its next byte b determines what is output:

- b is not special: b.
- ESC: the byte after it, which must be special.
- VALUE: the next value of the context of a text (§4).
- OPEN: a start tag, of the bytes after it, which must be

  ```text
  tag  = "<" name *( space name [space] "=" [space] quote VALUE quote ) [space] ["/"] ">"
  ```

  (space here is one or more spaces; the two quotes of an attribute must be the same byte). The
  tag is output as it is, each VALUE replaced by the next value of the context (1, e, a), e the
  tag's name and a the attribute's name before it. Where the tag does not end with `/>`, its name
  is pushed onto the stack (an error where `depth` names are on it already). Bytes that do not
  match `tag`, a special byte other than its VALUEs among them, are an error.
- CLOSE: `</`, the innermost name of the stack, and `>`; the name is popped. CLOSE with the
  stack empty is an error.
- END: an end tag written otherwise (spaces before its `>`, another name): the bytes after it up
  to and including the next `>`, as they are, none of them special; the innermost name of the
  stack is popped. END with the stack empty is an error.

The text is what the steps output, one after the other; output past the declared length is an
error, and so is a skeleton that ends within a step.

## 6. Encoding *(encoder)*

- **Tokens.** The text is cut into tags and the text between them. Comments (`<!--` to `-->`),
  CDATA sections (`<![CDATA[` to `]]>`), processing instructions (`<?` to `?>`) and other
  declarations (`<!` to the next `>`, a DOCTYPE's internal subset between `[` and `]` too) are
  kept in the skeleton as they are. A start tag is coded by OPEN where it matches `tag` with its
  values cut out (each value the bytes between its quotes, whatever they are); an end tag by CLOSE where it
  is `</`, the innermost name and `>`, else by END where it is `</`, any bytes but `>` and
  specials, and `>`, with an element open. Anything else, a `<` of no tag among it, is text.
- **Values.** The text between two tags is cut out as a value, VALUE in its place, where an
  element is open and it holds a byte other than a space; else it is kept in the skeleton (the
  whitespace of the layout). A special byte kept in the skeleton is preceded by ESC.
- **Flags.** A container's values end at LF where none of them holds one; else their lengths
  are in `lens`.
- **Streams.** The skeleton, `table` and `lens` are the module's own. The part of each container
  is a stream of its own, of `Kind::Text`, a content: aqr-compress recompresses each in turn as
  text (aqr-xsv or aqr-log where it takes it, as with columns of numbers or times; else aqr-wrt,
  with one dictionary of all the parts' text), and `values` is their slices one after the other.
- **Segments.** A text is cut, after the end tags that leave one element or none open, into
  segments of 64 MiB at least, each an application of the module of its own, the names open at
  its start in its params. The formula is their concatenation.
- **Files taken.** Not settled: `docs/xml-design.md`, "Choosing".
