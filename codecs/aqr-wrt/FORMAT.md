# The `wrt-recons` and `wrt-recons-hangul` modules

This document specifies the modules `wrt-recons` and `wrt-recons-hangul` of aqr-formula entry
formulas. `wrt-recons` rebuilds a text from a coded text, in which words and markup of a
dictionary are replaced by codes of one to three bytes. A word code also stands for the space
before the word, and flags before it tell its case. This is a word replacing transform after
XWRT (Skibiński). `wrt-recons-hangul` does the same for text of Hangul (Korean): the coded text
has Hangul syllables in two bytes rather than three, and words of Hangul, with the space before
them, replaced by codes (§6). The document is meant to be complete: an implementation of the
modules can be written from it alone. Sections marked *(encoder)* describe the choices this
implementation makes when it builds formulas; the modules do not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [The dictionary](#3-the-dictionary)
4. [Rebuilding the text](#4-rebuilding-the-text)
5. [Encoding](#5-encoding-encoder)
6. [The module `wrt-recons-hangul`](#6-the-module-wrt-recons-hangul)
7. [Encoding text of Hangul](#7-encoding-text-of-hangul-encoder)

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
  - Text of Hangul (§7) is coded by `wrt-recons-hangul` instead.
  - It is taken where coding pays, as told by the text and the dictionary alone. Eight windows
    of 128 KiB are taken, evenly spread (or the whole text where it is shorter than these).
    zstd -3 must compress them coded at least 1/32 smaller than as they are, and by more bytes
    over the whole text than its own dictionary takes compressed, where it has one.
  - The archive does not weigh it against the text kept as it is, so the same text is taken
    alike wherever it is.
- **Shared dictionary.**
  - The texts of a batch of files share one dictionary. It is made of the batch's files that
    are text but not of Hangul (§7), where the batch's text comes to at least 256 KiB.
  - With a fixed dictionary, a text that holds another codes to bytes that hold the other's
    code, except where the other's edges cut a token. So the archive still stores once what
    the two share, as with a file and the same file in an archive, or a file and its start.
    For the latter, aqr-formula's `share_slices` takes the start of a slice too.
  - The dictionary's resource counts as nothing for any one file when the archiver sizes it.
    So does coded text whose text the files archived together hold, as text coded alike
    holds its code.
- **Own dictionary.** Text of a single-file archive, or of a batch with less text, has its own
  dictionary, made of it alone.

## 6. The module `wrt-recons-hangul`

- **Module**: name `wrt-recons-hangul`, version 1.
- **Params**: the pieces of the coded text, one or more, each a pair of varints: the length of
  its coded text, then the length of its text. The coded texts of the pieces follow each other
  in the input, and their lengths must add up to the input's length. The lengths of their texts
  must add up to the declared length.
- **Resources**: one, the dictionary.
- **Inputs**: one, `text`, the coded text.
- **Output**: the texts of the pieces, in order. Any inconsistency is an error, as in §2.

A *syllable* is one of the 11172 Hangul syllables U+AC00 to U+D7A3; syllable j is U+AC00 + j.
*Continuation* bytes are 0x80 to 0xBF. The 32 *leads* are 0xAC to 0xC1 and 0xF5 to 0xFE, in that
order; none of them begins a character in UTF-8.

### 6.1 The dictionary

```text
varint  n1                    the codes of one byte
u8      one[n1]
varint  n2                    how many of the leads begin codes of two bytes
entries                       each followed by LF, to the end of the resource
```

- The bytes of `one` must be distinct, below 0x80, and none of them LF. n2 is at most 32.
- An entry is not empty and does not contain LF. It may be any other bytes.
- There are at most n1 + 256 · n2 + 65536 · (32 − n2) entries. Entry k is numbered from 0 in
  order. Its code is as follows, where L is the list of the leads:

  ```text
  k < n1:                one[k]
  k < n1 + 256 · n2:     L[j >> 8], j & 255                           with j = k − n1
  otherwise:             L[n2 + (j >> 16)], (j >> 8) & 255, j & 255    with j = k − n1 − 256 · n2
  ```

### 6.2 Rebuilding a piece

Each piece's coded text is read from its start, and must give exactly the length of its text.
At each step, the next byte b determines what comes next:

- b is one of `one`: entry k for b = one[k] is output.
- Any other b below 0x80 is output.
- b from 0x80 to 0xAB: with the byte t after it, j = (b − 0x80) · 256 + t must be below 11172.
  Syllable j is output in UTF-8 (three bytes).
- b is a lead: a code of two or three bytes as §6.1 numbers them. The entry it names must exist,
  and is output as it is.
- b is 0xFF: the byte after it is output.
- b from 0xC2 to 0xF4: b and the next 1 (b up to 0xDF), 2 (up to 0xEF) or 3 bytes are output.
  These must all be continuation bytes.

A step must not be cut by the end of the piece.

## 7. Encoding text of Hangul *(encoder)*

- **What is taken.** Text, as §5 tells, whose syllables make up at least a quarter of the bytes
  of the eight windows that §5 samples. Whether coding pays is told as in §5.
- **Tokens.** A text is scanned from its start. A *token* is a run of syllables as long as it
  goes, with the space before it if there is one. The bytes between tokens are not tokens. No
  token contains LF.
- **Dictionary.** It is made of the tokens of at most 48 bytes that the texts have at least 4
  times. Tokens are counted by a 64-bit hash, so two tokens of the same hash count as one (the
  first found).
  - `one` is the bytes below 0x80 that the texts do not have, but LF. They code the tokens that
    save the most bytes by them (count · (length − 1)), as many as there are of these bytes.
  - n2 is the most for which the rest have room. The tokens left that save the most by codes of
    two bytes (count · (length − 2), of those longer than 2) have them, and then those of three
    in the same way.
  - Ties are broken by the bytes of the tokens.
- **Coding.**
  - A token with an entry is replaced by its code. Otherwise, a token with a space before it is
    coded as a space and the rest of it, by its entry where it has one. Otherwise, each of its
    syllables j is coded as the bytes 0x80 + (j >> 8) and j & 255.
  - The bytes between tokens are written as they are. The exceptions are the bytes of `one`,
    and bytes at or above 0x80 that do not begin a character of UTF-8 complete with its
    continuation bytes. Each of these is preceded by 0xFF.
  - Pieces are of about 16 MiB each, coded and decoded on threads of their own. Each but the
    last ends where the first ASCII byte other than a space after its first 16 MiB begins. No
    token holds such a byte, nor is it within a character of UTF-8, so the pieces code to the
    same bytes as the whole. A text that holds another codes to bytes that hold the other's code,
    except where the other's edges cut a token.
- **Shared dictionary.** The texts of Hangul of a batch share a dictionary of their own, as §5
  tells of the others.
