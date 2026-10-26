# The `wrt-recons` module

This document specifies the module `wrt-recons` of aqr-formula entry formulas. The module
rebuilds a text from a coded text, in which words and markup of a dictionary are replaced by
codes of one to three bytes. A word code also stands for the space before the word, and flags
before it tell its case. This is a word replacing transform after XWRT (Skibiński). The
document is meant to be complete: an implementation of the module can be written from it
alone. Sections marked *(encoder)* describe the choices this implementation makes when it
builds formulas; the module does not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [The dictionary](#3-the-dictionary)
4. [Rebuilding the text](#4-rebuilding-the-text)
5. [Encoding](#5-encoding-encoder)

## 1. Conventions

- Integers are unsigned. **varint**: unsigned LEB128, least significant group first, at most
  10 bytes. A writer always uses the shortest form.
- `LF` is the byte 0x0A and the space is 0x20. A *letter* is an ASCII letter (`A`–`Z`,
  `a`–`z`). The upper case of a lower-case letter is the letter with bit 0x20 cleared.

## 2. The module

- **Module**: name `wrt-recons`, version 1.
- **Params**: none (empty).
- **Resources**: one, the dictionary (§3).
- **Inputs**: one, `text`, the coded text (§4).
- **Output**: the text of the declared length (§4). Any inconsistency is an error. That
  includes a malformed dictionary, coded text that ends inside a code, and an output of a
  length other than the declared one.

## 3. The dictionary

The resource is the layout of the codes followed by the entries.

```text
u8      ESC, CAP, UP, NOSP    the flags
varint  n1                    the codes of one byte
u8      one[n1]
varint  n2                    the first bytes of the codes of two bytes
u8      two[n2]
varint  n3                    the first bytes of the codes of three bytes
u8      three[n3]
entries                       each followed by LF, to the end of the resource
```

- The 4 + n1 + n2 + n3 bytes of the flags and the lists must be distinct, and none of them may
  be a letter. A byte of these is a *special* byte.
- An entry is not empty and does not contain LF.
  - An entry made only of lower-case letters is a *word*.
  - Any other entry is *markup*, and its first byte must not be a letter.
- There are at most n1 + 256 · n2 + 65536 · n3 entries. Entry k is numbered from 0 in order, and
  its code is:

  ```text
  k < n1:                one[k]
  k < n1 + 256 · n2:     two[j >> 8], j & 255                       with j = k − n1
  otherwise:             three[j >> 16], (j >> 8) & 255, j & 255    with j = k − n1 − 256 · n2
  ```

## 4. Rebuilding the text

The coded text is read from its start. At each step, the next byte b determines what comes
next:

- b is not special: b is output.
- b is ESC: the byte after it is output. That byte must be special.
- Otherwise the step is, in order:
  1. NOSP, optionally;
  2. CAP or UP, optionally;
  3. a code, either one of `one`, or one of `two` and the byte after it, or one of `three` and
     the two bytes after it.

  The code names entry k, which must exist. What is output depends on the entry:

  - Markup: the entry is output as it is. NOSP, CAP or UP before it is an error.
  - A word: a space is output first, unless NOSP was given. Then the word is output, in one of
    three ways:
    - as it is, without a flag;
    - its first letter in upper case, with CAP;
    - all its letters in upper case, with UP.

The output ends where the coded text does, and its length must be the declared one.

## 5. Encoding *(encoder)*

- **Tokens.** A text is cut into tokens from its start, and each token is found where the
  last one ends.
  - A *word* is a run of letters as long as it goes.
  - *Markup* is one of the following:
    - `<` and a name;
    - `</`, a name and `>`;
    - `&`, letters and `;`;
    - `&#`, digits and `;`.

    A name is a letter followed by letters, digits and `:`, `_`, `.`, `-`, as many as follow.
  - The bytes between tokens are not tokens. No token contains LF.
- **Entries of tokens.**
  - A word of 2 to 48 letters has an entry, its lower case, where it is one of the following:
    - all in lower case;
    - all in upper case (UP);
    - in lower case but for its first letter (CAP).

    Words of mixed case have no entry.
  - Markup of at most 48 bytes is its own entry.
- **Dictionary.** The entries that the texts it is made of have at least 4 times, most common
  first (ties by their bytes), as many as the layout has codes for.
- **Layout.**
  - The flags and code bytes are the bytes that are not letters, the rarest in the texts first
    (ties by value). The first 4 are the flags, and the next ones are `one`, `two` and `three`.
  - Their number K (8 to 160) and the split (n3 ∈ {0, 1, 2, 4, 8}, n2 ≥ 1, n1 ≥ 1) minimize the
    following estimate:

    ```text
    2 · (the bytes of the texts escaped) + Σ (each entry's count · the length of its code)
    ```

    The bytes escaped count twice, as an escape also breaks the matches through it. Among the
    layouts with room for all entries, the one with the smallest estimate is taken; where none
    has room, the one with the most room is taken.
- **Coding.**
  - A token whose entry is in the dictionary is replaced by its code. The bytes since the last
    one replaced are written before it, a special byte among them preceded by ESC.
  - For a word, a space that ends those bytes is dropped. If there is no such space, NOSP is
    written. After that comes CAP or UP for its case.
  - A text longer than 16 MiB is coded in pieces of about 16 MiB, each ending with LF, on
    threads of their own. As no token holds LF, the pieces code to the same bytes as the whole.
- **What is taken.**
  - Text of at least 256 KiB in which at most 1 in 1000 bytes are not text bytes. Text bytes
    are the printable ones, tab, CR, LF, form feed, and every byte from 0x80 on.
  - aqr-compress looks for such text in two places:
    - a file all of whose bytes are text bytes;
    - data whose name tells it is text (`.txt`, `.html`, `.xml`, …).
  - It is taken where coding pays, as told by the text and the dictionary alone. Eight windows
    of 128 KiB are taken, evenly spread (or the whole text where it is shorter than these).
    zstd -3 must compress them coded at least 1/32 smaller than as they are, and by more bytes
    over the whole text than its own dictionary takes compressed, where it has one.
  - The archive does not weigh it against the text kept as it is, so the same text is taken
    alike wherever it is.
- **Shared dictionary.**
  - The texts of a batch of files share one dictionary. It is made of the batch's files that
    are text, where these come to at least 256 KiB.
  - With a fixed dictionary, a text that holds another codes to bytes that hold the other's
    code, except where the other's edges cut a token. So the archive still stores once what
    the two share, as with a file and the same file in an archive, or a file and its start.
    For the latter, aqr-formula's `share_slices` takes the start of a slice too.
  - The dictionary's resource counts as nothing for any one file when the archiver sizes it.
    So does coded text whose text the files archived together hold, as text coded alike
    holds its code.
- **Own dictionary.** Text of a single-file archive, or of a batch with less text, has its own
  dictionary, made of it alone.
