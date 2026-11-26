# The `xml-recons` module

*Version 1. Implemented by `src/module.rs` (the module) and `src/formula.rs` (the encoder).*

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
    bound, for the memory plan); more is an error. At most 1024;
  - varint `k`, then `k` strings: the names of the elements open where the text starts, the
    outermost first (§4; `k` at most `depth`).

  `containers` is at most 65536, and at most half the length of `table` (each entry of it has
  two bytes at least).

  Nothing may follow.
- **Resources**: none.
- **Inputs**: four, in order: `skeleton`, `table`, `lens` and `values` (§3).
- **Memory**: the module's working memory is laid out as a stack in this order, each block
  rounded up to 16 bytes and held to the end (the plan, `module::plan`): after the copies of
  the params and inputs, the stack of names (`depth` entries of 12 bytes on WebAssembly), the
  containers (`containers` entries of 12), the table of contexts (§4: `max(2, 2 * containers
  rounded up to a power of two)` entries of 24) and the text (its declared length).
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

Each context has a container. (A decoder keeps them in an open-addressing table of a size fixed
by `containers`, names as slices of the skeleton and the params.) Containers are numbered from 0 in the order they are *made*: the
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
  to and including the next `>`, which are the whole tag, `</` and all, output as they are. They
  must start with `</` and hold no special byte, and there must be a `>`; the innermost name of
  the stack is popped. END with the stack empty is an error.

The text is what the steps output, one after the other; output past the declared length is an
error, and so is a skeleton that ends within a step.

## 6. Encoding *(encoder)*

- **Tokens.** The text is cut into tags and the text between them. Comments (`<!--` to `-->`),
  CDATA sections (`<![CDATA[` to `]]>`), processing instructions (`<?` to `?>`) and other
  declarations (`<!` to the next `>`; where a `[` comes first, past the next `]` and the `>`
  after it: a DOCTYPE's internal subset) are kept in the skeleton as they are, and so is any
  that has no end. A start tag is coded by OPEN where it matches `tag` (each value the bytes
  between its quotes, whatever they are, at most 1 GiB), the element's name pushed unless it
  ends with `/>`. An end tag, `</`, bytes none of them `<` or special, and `>`, with an element
  open, is coded by CLOSE where it is `</`, the innermost name and `>`, else by END. Anything
  else, a `<` of no token among it (an unterminated tag, a start tag of attributes with no
  value, an end tag with nothing open), is text.
- **Values.** The text between two tokens (a `<` of no token in it is text) is cut out as a
  value, VALUE in its place, where an element is open and it holds a byte other than a space;
  else it is kept in the skeleton (the whitespace of the layout). A special byte kept in the
  skeleton is preceded by ESC.
- **Bounds.** A segment makes at most 4096 containers (`CONTAINERS`) and has at most 256
  elements open (`DEPTH`); past them, a start tag that would pass one (a tag with a value in a
  context that has no container yet, or an element that would be the 257th) is kept in the
  skeleton as text, escaped, and is an element the module does not know (a *phantom*): the
  encoder keeps it on its own stack, its end tag kept as text too and its text, and the module's
  stack and its contexts are those of the elements around it. The values of a text context that
  has no container yet stay in the skeleton.
- **Flags.** A container's values end at LF where none of them holds one; else their lengths
  are in `lens`.
- **Streams.** The skeleton (`xml/skeleton`, `Kind::Text`), `table` and `lens` are the module's
  own. The part of each container is a stream of its own, of `Kind::Text`, a content
  (`xml/values`; of the kind `Kind::Words` where the values are prose, 40 bytes on average
  and a space in 16, `Kind::Column` for the other parts of 16 KiB or more, which aqr-compress takes
  as text with fewer looks, else `Kind::Text`): aqr-compress recompresses each in turn as text (aqr-xsv or aqr-log where it
  takes it, as with columns of numbers or times; else aqr-wrt), and `values` is their slices one
  after the other. The parts of fewer than 16384 bytes share one stream, a slice of it for each.
  Streams of one class and kind are put in one when the formula is archived
  (`Formula::merged`), contents that were recompressed or not alike.
- **Segments.** A text is cut after the end tags and the empty-element tags (`<a/>`) that leave
  at most four elements open, all of them known to the module, once the segment is 48 MiB
  (`xml:segment`) long at least. Each is an application of the module of its own, the names open
  at its start in its params, its contexts and containers its own. The formula is their
  concatenation.
- **Taken.** A text shorter than 64 KiB (`MIN_LEN`) is not taken, nor one that has no value cut
  out; `starts` tells a text that begins as XML does. Which texts to take where it pays is for
  aqr-compress (`docs/xml-design.md`, "Choosing").

## Design notes

- **Prototype** (Python, the pieces archived by `aqr` as files of a directory): elements of text
  (DBLP, PubMed, JMdict, SwissProt) gain the most, 16-24%; rows of numbers as attributes gain
  nothing, as aqr-log takes the lines whole; small files lose to the cost of each container. The
  numbers' own coding mattered little, so the module codes none.
- **Cost of compressing.** The first version took 1.79 times the corpus's instructions (enwik9
  363 -> 665 G): the looks that every content makes on each of 31 segments' text container, aqr-log
  on pieces, the comparison of the formula with the file, and the shared dictionary. Now 1.29
  (enwik9 419 G); the encoder itself is 4.8 GB/s. What was done:
  - The encoder tells aqr-compress what a container is by its stream's kind: `Kind::Words` for
    prose (values of 40 bytes on average, a space for 16) is made as aqr-wrt's text and nothing
    else; `Kind::Column` for the other containers of 16 KiB or more is made as any text with a
    sample of a sixteenth of the size (`table:column-window`, 16 KiB windows). 665 -> 435 G; the
    archives vary within 1.6% either way (PubMed -1.0%, lineitem +1.2%).
  - A file of 4 MiB or more is taken without the comparison of its formula with the file
    (`xml:uncompared`): the sample chose it; a small file's formula is compared still, as the
    module's block alone is 4 KB.
  - Segments of 48 MiB, not 32: a third fewer files to look at (435 -> 425 G); 64 MiB passes the
    application bound (256 MiB), which 48 leaves 13 MiB to spare for.
  - A file under 1 MiB is taken only at 92% (`xml:small-percent`): the fixed costs of the format
    are most of what it would gain (`play3.xml`, 0.93 of zstd, was 5.3% larger).
