//! Regions of records in a file, their forms, and the choice of those to take.

use crate::chain::{self, Rule};
use crate::fields::{self, Cost, Field};
use crate::{fixed, probe, zcost};

use crate::MIN_RECORDS;
use crate::module::How;

/// Candidates are judged on themselves and this much around them.
const LOCAL_CONTEXT: usize = 1 << 16;
/// Candidates are judged on their first records, up to this many bytes.
const JUDGE: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Records of k bytes.
    Fixed { k: usize },
    /// Entries by a rule, each with a head of `head` bytes; `starts` are where they start and
    /// where the last ends (for the encoder: the decoder follows the lengths).
    Entries { rule: Rule, head: usize, starts: Vec<usize> },
}

/// A region of records and how its fields are coded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub start: usize,
    pub end: usize,
    pub count: usize,
    pub shape: Shape,
    pub fields: Vec<Field>,
}

/// How a region is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// The records' (heads') bytes transposed: column by column.
    Bytes,
    /// The fields' planes, field by field.
    Fields,
}

impl Region {
    pub fn fixed(data: &[u8], start: usize, k: usize, count: usize) -> Region {
        let m = &data[start..start + k * count];
        let (fields, _, _) = fields::split(m, k, Cost::Kt, true);
        Region { start, end: start + k * count, count, shape: Shape::Fixed { k }, fields }
    }

    /// Entries starting at `starts` (the last where the last ends), with a head of `head`
    /// bytes or one chosen.
    pub fn entries(data: &[u8], starts: Vec<usize>, rule: Rule, head: Option<usize>) -> Region {
        let lmin = starts.windows(2).map(|w| w[1] - w[0]).min().unwrap();
        let head =
            head.map_or_else(|| chain::choose_head(data, &starts, &rule), |h| h.min(lmin).max(rule.d + rule.width()));
        let heads = Self::heads(data, &starts, head);
        let (fields, _, _) = fields::split(&heads, head, Cost::Kt, true);
        let (start, end) = (starts[0], *starts.last().unwrap());
        Region { start, end, count: starts.len() - 1, shape: Shape::Entries { rule, head, starts }, fields }
    }

    fn heads(data: &[u8], starts: &[usize], head: usize) -> Vec<u8> {
        starts[..starts.len() - 1].iter().flat_map(|&s| &data[s..s + head]).copied().collect()
    }

    /// The region's first records, as many as fit in `bytes` (MIN_RECORDS at least).
    pub fn prefix(&self, bytes: usize) -> Region {
        if self.end - self.start <= bytes {
            return self.clone();
        }
        let (count, shape) = match &self.shape {
            Shape::Fixed { k } => ((bytes / k).max(MIN_RECORDS).min(self.count), self.shape.clone()),
            Shape::Entries { rule, head, starts } => {
                let n = starts.partition_point(|&s| s <= self.start + bytes).max(MIN_RECORDS + 1).min(starts.len());
                (n - 1, Shape::Entries { rule: *rule, head: *head, starts: starts[..n].to_vec() })
            }
        };
        let end = match &shape {
            Shape::Fixed { k } => self.start + k * count,
            Shape::Entries { starts, .. } => *starts.last().unwrap(),
        };
        Region { start: self.start, end, count, shape, fields: self.fields.clone() }
    }

    /// The record size, or the entries' head size.
    pub fn width(&self) -> usize {
        match &self.shape {
            Shape::Fixed { k } => *k,
            Shape::Entries { head, .. } => *head,
        }
    }

    pub fn label(&self) -> String {
        match &self.shape {
            Shape::Fixed { k } => format!("{k}×{}", self.count),
            Shape::Entries { .. } => format!("entries ×{}", self.count),
        }
    }

    pub fn describe(&self) -> String {
        match &self.shape {
            Shape::Fixed { .. } => fields::describe(&self.fields),
            Shape::Entries { rule, head, .. } => {
                format!("head {head} B, {}: {}", rule.describe(), fields::describe(&self.fields))
            }
        }
    }

    /// The fields a form writes: the region's, or for its bytes transposed a raw byte each.
    pub fn fields_of(&self, form: Form) -> Vec<Field> {
        match form {
            Form::Fields => self.fields.clone(),
            Form::Bytes => (0..self.width()).map(|off| Field { off, width: 1, big: false, how: How::Raw }).collect(),
        }
    }

    /// The records (the entries' heads), and the entries' tails one after another.
    pub fn records(&self, data: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
        match &self.shape {
            Shape::Fixed { .. } => (data[self.start..self.end].to_vec(), None),
            Shape::Entries { head, starts, .. } => {
                let tails = starts.windows(2).flat_map(|w| &data[w[0] + head..w[1]]).copied().collect();
                (Self::heads(data, starts, *head), Some(tails))
            }
        }
    }

    /// The region of data in a form.
    pub fn render(&self, data: &[u8], form: Form) -> Vec<u8> {
        let k = self.width();
        let (m, tails): (std::borrow::Cow<[u8]>, Option<&[usize]>) = match &self.shape {
            Shape::Fixed { .. } => (data[self.start..self.end].into(), None),
            Shape::Entries { head, starts, .. } => (Self::heads(data, starts, *head).into(), Some(starts)),
        };
        let mut out = match form {
            Form::Bytes => {
                let mut t = Vec::with_capacity(self.end - self.start);
                for j in 0..k {
                    t.extend(m.chunks_exact(k).map(|r| r[j]));
                }
                t
            }
            Form::Fields => fields::encode(&m, k, &self.fields),
        };
        if let Some(starts) = tails {
            for w in starts.windows(2) {
                out.extend_from_slice(&data[w[0] + k..w[1]]);
            }
        }
        out
    }

    /// The region's bytes back from a form, with what the decoder knows: the shape (not the
    /// entries' starts), the count and the fields. None if the form is not one.
    pub fn restore(&self, form: Form, src: &[u8]) -> Option<Vec<u8>> {
        let (k, n) = (self.width(), self.count);
        let m = match form {
            Form::Bytes => {
                let t = src.get(..n * k)?;
                let mut m = vec![0u8; n * k];
                for j in 0..k {
                    for i in 0..n {
                        m[i * k + j] = t[j * n + i];
                    }
                }
                m
            }
            Form::Fields => fields::decode(src.get(..n * k)?, n, k, &self.fields)?,
        };
        let Shape::Entries { rule, .. } = &self.shape else {
            return (src.len() == n * k).then_some(m);
        };
        let mut out = Vec::with_capacity(src.len());
        let mut at = n * k;
        for h in m.chunks_exact(k) {
            let tail = rule.length(h, 0).checked_sub(k)?;
            out.extend_from_slice(h);
            out.extend_from_slice(src.get(at..at + tail)?);
            at += tail;
        }
        (at == src.len()).then_some(out)
    }
}

/// A region taken, its form, and what it saved under zstd -3 when taken (on itself and
/// LOCAL_CONTEXT around it).
#[derive(Clone, Debug)]
pub struct Choice {
    pub region: Region,
    pub form: Form,
    pub gain: i64,
}

/// The candidate regions of a file: chains of entries, then tables of fixed-size records.
pub fn candidates(data: &[u8]) -> Vec<Region> {
    let mut out: Vec<Region> =
        chain::chains(data).into_iter().map(|(p, r)| Region::entries(data, p, r, None)).collect();
    out.extend(fixed::find(data).into_iter().map(|(s, k, n)| Region::fixed(data, s, k, n)));
    out
}

/// zstd -3 of data's [s, e) and LOCAL_CONTEXT around it, with the regions taken in their forms
/// (the window widened to hold whole those it meets).
fn local_size(data: &[u8], taken: &[(usize, usize, &[u8])], s: usize, e: usize) -> usize {
    let (mut lo, mut hi) = (s.saturating_sub(LOCAL_CONTEXT), (e + LOCAL_CONTEXT).min(data.len()));
    for &(cs, ce, _) in taken {
        if cs < hi && lo < ce {
            (lo, hi) = (lo.min(cs), hi.max(ce));
        }
    }
    let mut sorted: Vec<_> = taken.iter().filter(|t| t.0 < hi && lo < t.1).collect();
    sorted.sort_unstable_by_key(|t| t.0);
    let mut parts: Vec<&[u8]> = Vec::new();
    let mut at = lo;
    for &&(cs, ce, form) in &sorted {
        parts.push(&data[at..cs]);
        parts.push(form);
        at = ce;
    }
    parts.push(&data[at..hi]);
    zcost::len(&parts)
}

/// The candidates to take, and in which form, with a line on each (as the prototype prints).
///
/// Each is measured alone (its best form; on its first JUDGE bytes, the gain scaled to its
/// size). Gains of regions apart roughly add up, so the set
/// apart with the largest sum of gains is found first (weighted interval scheduling: one big
/// region of a little gain must not shut out the tables it overlaps). Those, from the best,
/// and then the rest, from the best, are taken where they still make it smaller among those
/// taken before.
pub fn choose(data: &[u8], cands: Vec<Region>) -> (Vec<Choice>, Vec<String>) {
    // a region is judged on its first JUDGE bytes, its gains scaled to its size
    let judged: Vec<Region> = cands.iter().map(|c| c.prefix(JUDGE)).collect();
    let forms: Vec<[Vec<u8>; 2]> =
        judged.iter().map(|c| [c.render(data, Form::Bytes), c.render(data, Form::Fields)]).collect();
    let gains = |i: usize, taken: &[(usize, usize, &[u8])]| -> [i64; 2] {
        let c = &judged[i];
        let scale = (cands[i].end - cands[i].start) as f64 / (c.end - c.start) as f64;
        let before = local_size(data, taken, c.start, c.end) as i64;
        std::array::from_fn(|f| {
            let mut t = taken.to_vec();
            t.push((c.start, c.end, &forms[i][f]));
            ((before - local_size(data, &t, c.start, c.end) as i64) as f64 * scale) as i64
        })
    };
    let alone: Vec<i64> = (0..cands.len()).map(|i| *gains(i, &[]).iter().max().unwrap()).collect();
    let mut byend: Vec<usize> = (0..cands.len()).filter(|&i| alone[i] > 0).collect();
    byend.sort_unstable_by_key(|&i| (cands[i].end, i));
    let ends: Vec<usize> = byend.iter().map(|&i| cands[i].end).collect();
    let before = |j: usize, i: usize| ends[..j].partition_point(|&e| e <= cands[i].start);
    let mut sum = vec![0i64; byend.len() + 1];
    for (j, &i) in byend.iter().enumerate() {
        sum[j + 1] = sum[j].max(sum[before(j, i)] + alone[i]);
    }
    let mut picked = vec![false; cands.len()];
    let mut j = byend.len();
    while j > 0 {
        let i = byend[j - 1];
        let p = before(j - 1, i);
        if sum[j] == sum[p] + alone[i] {
            picked[i] = true;
            j = p;
        } else {
            j -= 1;
        }
    }
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_unstable_by_key(|&i| (!picked[i], -alone[i], i));

    let mut taken: Vec<(usize, Form, i64)> = Vec::new();
    let mut lines = Vec::new();
    for i in order {
        let c = &cands[i];
        let (s, e) = (c.start, c.end);
        if taken.iter().any(|&(t, _, _)| s < cands[t].end && cands[t].start < e) {
            lines.push(format!("  {s:#x}..{e:#x} {}: overlaps", c.label()));
            continue;
        }
        let t: Vec<(usize, usize, &[u8])> =
            taken.iter().map(|&(t, f, _)| (judged[t].start, judged[t].end, &forms[t][f as usize][..])).collect();
        let g = gains(i, &t);
        let f = if g[1] > g[0] { 1 } else { 0 };
        lines.push(format!(
            "  {s:#x}..{e:#x} {} ({:.1}%): bytes {:+}, fields {:+}{}",
            c.label(),
            100.0 * (e - s) as f64 / data.len() as f64,
            -g[0],
            -g[1],
            if g[f] > 0 { "" } else { ", left" }
        ));
        lines.push(format!("    {}", c.describe()));
        if g[f] > 0 {
            taken.push((i, if f == 1 { Form::Fields } else { Form::Bytes }, g[f]));
        }
    }
    let mut out: Vec<Choice> =
        taken.iter().map(|&(i, form, gain)| Choice { region: cands[i].clone(), form, gain }).collect();
    out.sort_unstable_by_key(|c| c.region.start);
    (out, lines)
}

/// The regions to take in data, sorted.
pub fn infer(data: &[u8]) -> Vec<Choice> {
    choose(data, candidates(data)).0
}

/// The regions to take in data, if a quick estimate ([`probe::estimate`]) says they may save
/// at least `least` of its zstd size: None (in tens of ms) where it does not.
pub fn infer_if(data: &[u8], least: f64) -> Option<Vec<Choice>> {
    (probe::estimate(data).saves >= least).then(|| infer(data))
}

/// The file with the regions taken in their forms.
pub fn apply(data: &[u8], choices: &[Choice]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut at = 0;
    for c in choices {
        out.extend_from_slice(&data[at..c.region.start]);
        out.extend(c.region.render(data, c.form));
        at = c.region.end;
    }
    out.extend_from_slice(&data[at..]);
    out
}

/// zstd -3 of data (the size the forms are judged against).
pub fn zstd_size(data: &[u8]) -> usize {
    zcost::len(&[data])
}
