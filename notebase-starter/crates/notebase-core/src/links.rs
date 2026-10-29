//! Pure wiki-link operations. Paths and bodies come from the caller; no I/O occurs here.
use crate::index::extract::markdown_options;
use comrak::{nodes::NodeValue, Arena};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ops::Range;
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkNote {
    pub path: String,
    pub title: String,
    pub aliases: Vec<String>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiLink {
    /// The target includes its optional #anchor; label is display text only.
    pub target: String,
    pub label: Option<String>,
    /// Human-readable, one-based Unicode scalar positions.
    pub line: usize,
    pub col: usize,
    /// Half-open byte offsets in the unmodified UTF-8 source, including [[ and ]].
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    pub target_path: Option<String>,
    /// Preferred candidate first, then distance and stable path order.
    pub candidates: Vec<String>,
    /// True whenever there is more than one candidate, even with a nearest match.
    pub ambiguous: bool,
}

struct Lines {
    starts: Vec<usize>,
}
impl Lines {
    fn new(raw: &str) -> Self {
        let bytes = raw.as_bytes();
        let mut starts = vec![0];
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\r' {
                i += usize::from(bytes.get(i + 1) == Some(&b'\n'));
                starts.push(i + 1);
            } else if bytes[i] == b'\n' {
                starts.push(i + 1);
            }
            i += 1;
        }
        Self { starts }
    }
    fn position(&self, raw: &str, byte: usize) -> (usize, usize) {
        let line = self.starts.partition_point(|&start| start <= byte) - 1;
        (line + 1, raw[self.starts[line]..byte].chars().count() + 1)
    }
    fn span(&self, pos: comrak::nodes::Sourcepos, len: usize) -> Option<Range<usize>> {
        let start = *self.starts.get(pos.start.line.checked_sub(1)?)?;
        let end = *self.starts.get(pos.end.line.checked_sub(1)?)?;
        let start = (start + pos.start.column.saturating_sub(1)).min(len);
        let end = if pos.end.column == 0 {
            self.starts.get(pos.end.line).copied().unwrap_or(len)
        } else {
            (end + pos.end.column).min(len)
        };
        (start < end).then_some(start..end)
    }
}

fn frontmatter_end(raw: &str, lines: &Lines) -> Option<usize> {
    let first_end = lines.starts.get(1).copied().unwrap_or(raw.len());
    let first = raw[..first_end].trim_end_matches(['\r', '\n']);
    if first.trim_start_matches('\u{feff}').trim_end() != "---" {
        return None;
    }
    for (line, &start) in lines.starts.iter().enumerate().skip(1) {
        let end = lines.starts.get(line + 1).copied().unwrap_or(raw.len());
        let text = raw[start..end].trim_end_matches(['\r', '\n']).trim_end();
        if text == "---" || text == "..." {
            return Some(end);
        }
    }
    None
}

/// Mask metadata without removing bytes/newlines, so comrak positions still refer to raw.
fn exclusions(raw: &str, lines: &Lines) -> Vec<bool> {
    let mut excluded = vec![false; raw.len()];
    let mut input = raw.as_bytes().to_vec();
    let frontmatter = frontmatter_end(raw, lines);
    let prefix = frontmatter.unwrap_or(0);
    // A BOM is not indentation. Remove it only for parsing, then restore its
    // three bytes in first-line AST columns when mapping back to the source.
    let bom = if frontmatter.is_none() && raw.starts_with('\u{feff}') {
        3
    } else {
        0
    };
    excluded[..prefix.max(bom)].fill(true);
    for byte in &mut input[..prefix] {
        if *byte != b'\r' && *byte != b'\n' {
            *byte = b' ';
        }
    }
    // Replacing a complete prefix with ASCII spaces preserves valid UTF-8.
    let input = String::from_utf8(input).expect("masked UTF-8 prefix");
    let arena = Arena::new();
    let root = comrak::parse_document(&arena, &input[bom..], &markdown_options());
    for node in root.descendants() {
        let data = node.data.borrow();
        if matches!(
            data.value,
            NodeValue::Code(_)
                | NodeValue::CodeBlock(_)
                | NodeValue::HtmlInline(_)
                | NodeValue::HtmlBlock(_)
                | NodeValue::Link(_)
                | NodeValue::Image(_)
        ) {
            let mut pos = data.sourcepos;
            if pos.start.line == 1 {
                pos.start.column += bom;
            }
            if pos.end.line == 1 {
                pos.end.column += bom;
            }
            if let Some(range) = lines.span(pos, raw.len()) {
                excluded[range].fill(true);
            }
        }
    }
    excluded
}

fn escaped(bytes: &[u8], at: usize) -> bool {
    bytes[..at]
        .iter()
        .rev()
        .take_while(|&&b| b == b'\\')
        .count()
        % 2
        == 1
}
fn delimiter(text: &str, byte: u8) -> Option<usize> {
    text.as_bytes()
        .iter()
        .enumerate()
        .find_map(|(i, &b)| (b == byte && !escaped(text.as_bytes(), i)).then_some(i))
}
fn unescape(text: &str, preserve_anchor_syntax: bool) -> String {
    let mut chars = text.chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek().is_some_and(|c| c.is_ascii_punctuation()) {
            let next = chars.next().unwrap();
            if preserve_anchor_syntax && matches!(next, '#' | '\\') {
                out.push('\\');
            }
            out.push(next);
        } else {
            out.push(c);
        }
    }
    out
}

struct Parsed {
    links: Vec<WikiLink>,
    /// All well-formed wiki syntax, including embeds, is excluded from mentions.
    spans: Vec<Range<usize>>,
}
fn scan(raw: &str, lines: &Lines, excluded: &[bool]) -> Parsed {
    let bytes = raw.as_bytes();
    let mut result = Parsed {
        links: vec![],
        spans: vec![],
    };
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i..i + 2] != *b"[[" || excluded[i] || escaped(bytes, i) {
            i += 1;
            continue;
        }
        let start = i;
        i += 2;
        let mut end = None;
        while i + 1 < bytes.len() {
            if excluded[i]
                || matches!(bytes[i], b'\r' | b'\n')
                || (bytes[i..i + 2] == *b"[[" && !escaped(bytes, i))
            {
                break;
            }
            if bytes[i..i + 2] == *b"]]" && !escaped(bytes, i) && !excluded[i + 1] {
                end = Some(i + 2);
                break;
            }
            i += 1;
        }
        let Some(end) = end else { continue };
        let inner = &raw[start + 2..end - 2];
        let split = delimiter(inner, b'|');
        let target = inner[..split.unwrap_or(inner.len())].trim();
        if !target.is_empty() {
            result.spans.push(start..end);
            let embed = start > 0 && bytes[start - 1] == b'!' && !escaped(bytes, start - 1);
            if !embed {
                let (line, col) = lines.position(raw, start);
                result.links.push(WikiLink {
                    target: unescape(target, true),
                    label: split.map(|at| unescape(&inner[at + 1..], false)),
                    line,
                    col,
                    start,
                    end,
                });
            }
        }
        i = end;
    }
    result
}

pub fn parse(raw: &str) -> Vec<WikiLink> {
    let lines = Lines::new(raw);
    scan(raw, &lines, &exclusions(raw, &lines)).links
}

fn folded(text: &str) -> String {
    text.trim()
        .nfc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .nfc()
        .collect()
}
/// Lexical vault-relative normalization, deliberately independent of the host filesystem.
fn normalized_path(path: &str) -> Option<String> {
    let slash = path.trim().replace('\\', "/");
    if slash.contains(['\0', ':']) {
        return None;
    }
    let mut parts = vec![];
    for part in slash.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/").nfc().collect())
}
fn without_md(path: &str) -> &str {
    if path
        .as_bytes()
        .get(path.len().saturating_sub(3)..)
        .is_some_and(|end| end.eq_ignore_ascii_case(b".md"))
    {
        &path[..path.len() - 3]
    } else {
        path
    }
}
fn path_key(path: &str) -> Option<String> {
    normalized_path(path).map(|p| folded(without_md(&p)))
}
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}
fn distance(source: &str, target: &str) -> (usize, usize) {
    let source = parent(source)
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    let target = parent(target)
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    let common = source
        .iter()
        .zip(&target)
        .take_while(|(a, b)| a == b)
        .count();
    (
        usize::MAX - common,
        source.len() + target.len() - 2 * common,
    )
}

struct Resolver {
    paths: BTreeMap<String, Vec<String>>,
    names: BTreeMap<String, Vec<String>>,
}
impl Resolver {
    fn new(notes: &[LinkNote]) -> Self {
        let mut resolver = Self {
            paths: BTreeMap::new(),
            names: BTreeMap::new(),
        };
        for note in notes {
            if let Some(path) = path_key(&note.path) {
                resolver
                    .paths
                    .entry(path.clone())
                    .or_default()
                    .push(note.path.clone());
                let stem = path.rsplit('/').next().unwrap_or(&path);
                for name in std::iter::once(note.title.as_str())
                    .chain(note.aliases.iter().map(String::as_str))
                    .chain(std::iter::once(stem))
                {
                    let name = folded(name);
                    if !name.is_empty() {
                        resolver
                            .names
                            .entry(name)
                            .or_default()
                            .push(note.path.clone());
                    }
                }
            }
        }
        for paths in resolver
            .paths
            .values_mut()
            .chain(resolver.names.values_mut())
        {
            paths.sort();
            paths.dedup();
        }
        resolver
    }
    fn result(&self, source: &str, mut candidates: Vec<String>) -> Resolution {
        let source = path_key(source).unwrap_or_default();
        candidates.sort_by_cached_key(|p| {
            let key = path_key(p).unwrap_or_default();
            (distance(&source, &key), key, p.clone())
        });
        candidates.dedup();
        Resolution {
            target_path: candidates.first().cloned(),
            ambiguous: candidates.len() > 1,
            candidates,
        }
    }
    fn resolve(&self, source: &str, target: &str) -> Resolution {
        let target = target.trim();
        let name = target[..delimiter(target, b'#').unwrap_or(target.len())].trim();
        if name.is_empty() {
            return self.result(
                source,
                if target.starts_with('#') {
                    path_key(source)
                        .and_then(|p| self.paths.get(&p).cloned())
                        .unwrap_or_else(|| normalized_path(source).into_iter().collect())
                } else {
                    vec![]
                },
            );
        }
        let name = name.replace("\\#", "#");
        let slash = name.replace('\\', "/");
        let relative = slash.starts_with("./") || slash.starts_with("../");
        let path = if relative {
            normalized_path(source)
                .and_then(|source| normalized_path(&format!("{}/{}", parent(&source), slash)))
        } else {
            normalized_path(&slash)
        };
        let Some(path) = path else {
            return self.result(source, vec![]);
        };
        if let Some(candidates) = path_key(&path).and_then(|p| self.paths.get(&p)) {
            return self.result(source, candidates.clone());
        }
        if relative || slash.starts_with('/') {
            return self.result(source, vec![]);
        }
        let name = folded(without_md(&name));
        self.result(source, self.names.get(&name).cloned().unwrap_or_default())
    }
}

pub fn resolve(source_path: &str, target: &str, notes: &[LinkNote]) -> Resolution {
    Resolver::new(notes).resolve(source_path, target)
}

/// Rewrite only the path portion of links whose selected resolution is old_path.
/// Anchors, display labels, whitespace, encoding markers, and line endings survive verbatim.
pub fn rename_body(
    source: &LinkNote,
    old_path: &str,
    new_path: &str,
    notes: &[LinkNote],
) -> String {
    let (Some(old), Some(new)) = (path_key(old_path), normalized_path(new_path)) else {
        return source.body.clone();
    };
    if path_key(&new).as_ref() == Some(&old) {
        return source.body.clone();
    }
    let resolver = Resolver::new(notes);
    let mut body = source.body.clone();
    for link in parse(&source.body).into_iter().rev() {
        let resolution = resolver.resolve(&source.path, &link.target);
        if resolution
            .target_path
            .as_deref()
            .and_then(path_key)
            .as_ref()
            != Some(&old)
        {
            continue;
        }
        let inner = &source.body[link.start + 2..link.end - 2];
        let target_end = delimiter(inner, b'|').unwrap_or(inner.len());
        let target = &inner[..target_end];
        let anchor = delimiter(target, b'#').unwrap_or(target.len());
        let old_part = &target[..anchor];
        let start = old_part.len() - old_part.trim_start().len();
        let end = old_part.trim_end().len().max(start);
        let replacement = without_md(&new)
            .replace('\\', "\\\\")
            .replace('|', "\\|")
            .replace('#', "\\#")
            .replace('[', "\\[")
            .replace(']', "\\]");
        body.replace_range(link.start + 2 + start..link.start + 2 + end, &replacement);
    }
    body
}

struct Relation {
    source: usize,
    target: String,
    link: WikiLink,
    resolution: Resolution,
}
fn relations(notes: &[LinkNote], resolver: &Resolver) -> Vec<Relation> {
    let mut edges = vec![];
    for (source, note) in notes.iter().enumerate() {
        for link in parse(&note.body) {
            let resolution = resolver.resolve(&note.path, &link.target);
            if let Some(target) = resolution.target_path.clone() {
                edges.push(Relation {
                    source,
                    target,
                    link,
                    resolution,
                });
            }
        }
    }
    edges.sort_by(|a, b| {
        notes[a.source]
            .path
            .cmp(&notes[b.source].path)
            .then(a.link.start.cmp(&b.link.start))
    });
    edges
}
fn context(body: &str, start: usize, end: usize) -> &str {
    let left = body[..start]
        .char_indices()
        .rev()
        .nth(39)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let right = body[end..]
        .char_indices()
        .nth(40)
        .map(|(i, _)| end + i)
        .unwrap_or(body.len());
    &body[left..right]
}

pub fn backlinks(path: &str, notes: &[LinkNote]) -> Value {
    let resolver = Resolver::new(notes);
    let edges = relations(notes, &resolver);
    let wanted = path_key(path);
    let mut outgoing: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for edge in &edges {
        if let (Some(from), Some(to)) = (path_key(&notes[edge.source].path), path_key(&edge.target))
        {
            outgoing.entry(from).or_default().insert(to);
        }
    }
    // Reachability from the requested target detects self and transitive cycles in one pass.
    let mut reachable = BTreeSet::new();
    let mut queue: VecDeque<_> = wanted.clone().into_iter().collect();
    while let Some(from) = queue.pop_front() {
        if reachable.insert(from.clone()) {
            queue.extend(outgoing.get(&from).into_iter().flatten().cloned());
        }
    }
    let items: Vec<_> = edges
        .iter()
        .filter(|e| wanted.is_some() && path_key(&e.target) == wanted)
        .map(|edge| {
            let source = &notes[edge.source];
            let link = &edge.link;
            let snippet = context(&source.body, link.start, link.end);
            json!({"source":source.path,"sourcePath":source.path,"sourceTitle":source.title,
                "targetPath":edge.target,"target":link.target,"label":link.label,
                "line":link.line,"col":link.col,"start":link.start,"end":link.end,
                "snippet":snippet,"context":snippet,"ambiguous":edge.resolution.ambiguous,
                "candidates":edge.resolution.candidates,
                "circular":path_key(&source.path).is_some_and(|p| reachable.contains(&p))})
        })
        .collect();
    json!({"path":path,"total":items.len(),"items":items})
}

/// Canonically decomposed case-insensitive text plus a map back to original byte spans.
fn mention_text(text: &str) -> (String, Vec<usize>, Vec<usize>) {
    let mut folded = String::new();
    let mut starts = vec![];
    let mut ends = vec![];
    for (start, c) in text.char_indices() {
        let end = start + c.len_utf8();
        for part in c.to_string().nfd().flat_map(char::to_lowercase) {
            folded.push(part);
            starts.extend(std::iter::repeat_n(start, part.len_utf8()));
            ends.extend(std::iter::repeat_n(end, part.len_utf8()));
        }
    }
    (folded, starts, ends)
}
fn western_word(c: char) -> bool {
    let cjk =
        matches!(c as u32, 0x2e80..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff | 0x20000..=0x323af);
    (c.is_alphanumeric() && !cjk) || c == '_'
}
fn word_boundary(text: &str, start: usize, end: usize) -> bool {
    let first = text[start..].chars().next().unwrap();
    let last = text[start..end]
        .chars()
        .rev()
        .find(|&c| unicode_normalization::char::canonical_combining_class(c) == 0)
        .unwrap_or(first);
    !(western_word(first) && text[..start].chars().next_back().is_some_and(western_word)
        || western_word(last) && text[end..].chars().next().is_some_and(western_word))
}

pub fn mentions(path: &str, notes: &[LinkNote]) -> Value {
    let wanted = path_key(path);
    let target = notes
        .iter()
        .find(|n| wanted.is_some() && path_key(&n.path) == wanted);
    let Some(target) = target else {
        return json!({"path":path,"items":[],"total":0});
    };
    let mut names: BTreeSet<String> = std::iter::once(target.title.as_str())
        .chain(target.aliases.iter().map(String::as_str))
        .filter(|s| !s.trim().is_empty())
        .map(|s| mention_text(s.trim()).0)
        .collect();
    if names.is_empty() {
        names.insert(
            mention_text(without_md(
                target.path.rsplit('/').next().unwrap_or(&target.path),
            ))
            .0,
        );
    }
    let mut ordered: Vec<_> = notes
        .iter()
        .filter(|n| path_key(&n.path) != wanted)
        .collect();
    ordered.sort_by(|a, b| a.path.cmp(&b.path));
    let mut items = vec![];
    for source in ordered {
        let lines = Lines::new(&source.body);
        let mut excluded = exclusions(&source.body, &lines);
        for span in scan(&source.body, &lines, &excluded).spans {
            excluded[span].fill(true);
        }
        let (text, starts, ends) = mention_text(&source.body);
        let mut matches = vec![];
        for name in &names {
            if name.is_empty() {
                continue;
            }
            for (offset, _) in text.match_indices(name) {
                let stop = offset + name.len();
                let start = starts[offset];
                let end = ends[stop - 1];
                // Never select half a lowercase expansion or half a decomposed character.
                if offset > 0 && starts[offset - 1] == start
                    || stop < starts.len() && starts[stop] == starts[stop - 1]
                    || text[stop..].chars().next().is_some_and(|c| {
                        unicode_normalization::char::canonical_combining_class(c) != 0
                    })
                    || excluded[start..end].iter().any(|&v| v)
                    || !word_boundary(&source.body, start, end)
                {
                    continue;
                }
                matches.push((start, end));
            }
        }
        matches.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        matches.dedup();
        let mut previous_end = 0;
        for (start, end) in matches {
            if start < previous_end {
                continue;
            }
            previous_end = end;
            let (line, col) = lines.position(&source.body, start);
            let snippet = context(&source.body, start, end);
            items.push(
                json!({"source":source.path,"sourcePath":source.path,"sourceTitle":source.title,
                "text":&source.body[start..end],"start":start,"end":end,"line":line,"col":col,
                "snippet":snippet,"context":snippet}),
            );
        }
    }
    json!({"path":path,"total":items.len(),"items":items})
}

/// A centered graph is an undirected breadth-first neighborhood of resolved directed edges.
/// Unresolved links do not create phantom nodes; repeated links collapse into one graph edge.
pub fn graph(center: Option<&str>, max_nodes: usize, notes: &[LinkNote]) -> Value {
    let limit = max_nodes.min(500);
    let resolver = Resolver::new(notes);
    let edges = relations(notes, &resolver);
    let nodes: BTreeMap<_, _> = notes.iter().map(|n| (n.path.clone(), n)).collect();
    let edges: BTreeSet<_> = edges
        .into_iter()
        .filter_map(|edge| {
            nodes
                .contains_key(&edge.target)
                .then(|| (notes[edge.source].path.clone(), edge.target))
        })
        .collect();
    let mut neighbors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (from, to) in &edges {
        neighbors
            .entry(from.clone())
            .or_default()
            .insert(to.clone());
        neighbors
            .entry(to.clone())
            .or_default()
            .insert(from.clone());
    }
    let mut selected = vec![];
    let mut truncated = false;
    if let Some(center) = center {
        let center = path_key(center)
            .and_then(|p| resolver.paths.get(&p))
            .and_then(|paths| paths.first());
        let mut queue: VecDeque<_> = center.into_iter().cloned().collect();
        let mut visited = BTreeSet::new();
        while let Some(path) = queue.pop_front() {
            if !visited.insert(path.clone()) {
                continue;
            }
            if selected.len() == limit {
                truncated = true;
                break;
            }
            selected.push(path.clone());
            queue.extend(
                neighbors
                    .get(&path)
                    .into_iter()
                    .flatten()
                    .filter(|p| !visited.contains(*p))
                    .cloned(),
            );
        }
    } else {
        selected.extend(nodes.keys().take(limit).cloned());
        truncated = nodes.len() > limit;
    }
    let kept: BTreeSet<_> = selected.iter().collect();
    let edges: Vec<_> = edges
        .into_iter()
        .filter(|(from, to)| kept.contains(from) && kept.contains(to))
        .map(|(from, to)| json!({"from":from,"to":to,"source":from,"target":to}))
        .collect();
    let nodes: Vec<_> = selected
        .iter()
        .map(|path| json!({"path":path,"title":nodes[path].title}))
        .collect();
    json!({"nodes":nodes,"edges":edges,"truncated":truncated,"center":center,"maxNodes":limit})
}
