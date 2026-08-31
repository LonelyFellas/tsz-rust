//! 例句目标发现的纯内核。
//!
//! 本模块只定义 token、pattern、命中位置与候选集合，不依赖 HTTP、association、
//! Pending 或数据库 DTO。当前 [`DiscoveryCatalog`] 是标准库实现的正确性参考；生产接入
//! FST/Aho-Corasick 时应保留这些输入输出语义，并把索引查询替换为 adapter。

use std::collections::BTreeMap;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind as AhoMatchKind};
use fst::{Map, MapBuilder};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CodepointRange {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) ordinal: usize,
    pub(crate) range: CodepointRange,
    pub(crate) surface: String,
    pub(crate) normalized: String,
    pub(crate) hard_boundary_before: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SourceSegment {
    pub(crate) range: CodepointRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AliasPosting<C> {
    pub(crate) surface: String,
    pub(crate) candidates: Vec<C>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContiguousPattern<C> {
    pub(crate) literals: Vec<String>,
    pub(crate) candidates: Vec<C>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct GapRule {
    /// 这里只表达允许跳过的 token 数，不表达 object 等语法角色。
    pub(crate) min_tokens: usize,
    pub(crate) max_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeparablePattern<C> {
    pub(crate) literal_segments: Vec<Vec<String>>,
    pub(crate) gaps: Vec<GapRule>,
    pub(crate) candidates: Vec<C>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum MatchKind {
    Word,
    ContiguousPhrase,
    SeparablePhrase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveryMatch<C> {
    pub(crate) kind: MatchKind,
    pub(crate) source_segments: Vec<SourceSegment>,
    pub(crate) normalized_surface: String,
    pub(crate) candidates: Vec<C>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveryResult<C> {
    pub(crate) tokens: Vec<Token>,
    pub(crate) matches: Vec<DiscoveryMatch<C>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CatalogBuildError {
    InvalidAliasSurface(String),
    EmptyContiguousPattern,
    EmptySeparablePattern,
    InvalidLiteral(String),
    InvalidGapShape,
    InvalidGapRule(GapRule),
    IndexBuild,
}

#[derive(Debug, Clone)]
pub(crate) struct DiscoveryCatalog<C> {
    alias_index: Map<Vec<u8>>,
    alias_postings: Vec<Vec<C>>,
    contiguous_automaton: Option<AhoCorasick>,
    contiguous_patterns: Vec<ContiguousPattern<C>>,
    separable_patterns: Vec<SeparablePattern<C>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiscoveryIndexMetrics {
    pub(crate) alias_key_count: usize,
    pub(crate) alias_fst_bytes: usize,
    pub(crate) contiguous_pattern_count: usize,
    pub(crate) contiguous_automaton_bytes: usize,
}

/// 上层只依赖这个中性入口；后续 FST/Aho-Corasick snapshot 实现同一接口即可。
pub(crate) trait DiscoveryEngine<C> {
    fn discover(&self, text: &str) -> DiscoveryResult<C>;
}

impl<C> DiscoveryCatalog<C>
where
    C: Clone + Ord,
{
    pub(crate) fn build(
        aliases: Vec<AliasPosting<C>>,
        contiguous_patterns: Vec<ContiguousPattern<C>>,
        separable_patterns: Vec<SeparablePattern<C>>,
    ) -> Result<Self, CatalogBuildError> {
        let mut aliases_by_surface = BTreeMap::<String, Vec<C>>::new();
        for posting in aliases {
            let normalized = normalize_single_literal(&posting.surface)
                .map_err(|_| CatalogBuildError::InvalidAliasSurface(posting.surface.clone()))?;
            aliases_by_surface
                .entry(normalized)
                .or_default()
                .extend(posting.candidates);
        }
        for candidates in aliases_by_surface.values_mut() {
            candidates.sort_unstable();
            candidates.dedup();
        }
        let mut alias_bytes = Vec::new();
        let alias_postings = {
            let mut builder =
                MapBuilder::new(&mut alias_bytes).map_err(|_| CatalogBuildError::IndexBuild)?;
            let mut postings = Vec::with_capacity(aliases_by_surface.len());
            for (index, (surface, candidates)) in aliases_by_surface.into_iter().enumerate() {
                builder
                    .insert(surface, index as u64)
                    .map_err(|_| CatalogBuildError::IndexBuild)?;
                postings.push(candidates);
            }
            builder
                .finish()
                .map_err(|_| CatalogBuildError::IndexBuild)?;
            postings
        };
        let alias_index = Map::new(alias_bytes).map_err(|_| CatalogBuildError::IndexBuild)?;

        let mut contiguous_by_literals = BTreeMap::<Vec<String>, Vec<C>>::new();
        for pattern in contiguous_patterns {
            if pattern.literals.len() < 2 {
                return Err(CatalogBuildError::EmptyContiguousPattern);
            }
            let literals = normalize_literals(pattern.literals)?;
            contiguous_by_literals
                .entry(literals)
                .or_default()
                .extend(pattern.candidates);
        }
        let contiguous_patterns = contiguous_by_literals
            .into_iter()
            .map(|(literals, mut candidates)| {
                candidates.sort_unstable();
                candidates.dedup();
                ContiguousPattern {
                    literals,
                    candidates,
                }
            })
            .collect::<Vec<_>>();
        let contiguous_automaton = if contiguous_patterns.is_empty() {
            None
        } else {
            let patterns = contiguous_patterns
                .iter()
                .map(|pattern| pattern.literals.join("\u{1f}"))
                .collect::<Vec<_>>();
            Some(
                AhoCorasickBuilder::new()
                    .match_kind(AhoMatchKind::Standard)
                    .build(patterns)
                    .map_err(|_| CatalogBuildError::IndexBuild)?,
            )
        };

        let mut separable_by_shape = BTreeMap::<(Vec<Vec<String>>, Vec<GapRule>), Vec<C>>::new();
        for pattern in separable_patterns {
            if pattern.literal_segments.len() < 2
                || pattern.literal_segments.iter().any(Vec::is_empty)
            {
                return Err(CatalogBuildError::EmptySeparablePattern);
            }
            if pattern.gaps.len() + 1 != pattern.literal_segments.len() {
                return Err(CatalogBuildError::InvalidGapShape);
            }
            if let Some(rule) = pattern
                .gaps
                .iter()
                .find(|rule| rule.min_tokens > rule.max_tokens)
            {
                return Err(CatalogBuildError::InvalidGapRule(*rule));
            }
            let literal_segments = pattern
                .literal_segments
                .into_iter()
                .map(normalize_literals)
                .collect::<Result<Vec<_>, _>>()?;
            separable_by_shape
                .entry((literal_segments, pattern.gaps))
                .or_default()
                .extend(pattern.candidates);
        }
        let separable_patterns = separable_by_shape
            .into_iter()
            .map(|((literal_segments, gaps), mut candidates)| {
                candidates.sort_unstable();
                candidates.dedup();
                SeparablePattern {
                    literal_segments,
                    gaps,
                    candidates,
                }
            })
            .collect();

        Ok(Self {
            alias_index,
            alias_postings,
            contiguous_automaton,
            contiguous_patterns,
            separable_patterns,
        })
    }

    pub(crate) fn index_metrics(&self) -> DiscoveryIndexMetrics {
        DiscoveryIndexMetrics {
            alias_key_count: self.alias_postings.len(),
            alias_fst_bytes: self.alias_index.as_fst().as_bytes().len(),
            contiguous_pattern_count: self.contiguous_patterns.len(),
            contiguous_automaton_bytes: self
                .contiguous_automaton
                .as_ref()
                .map_or(0, AhoCorasick::memory_usage),
        }
    }
}

impl<C> DiscoveryEngine<C> for DiscoveryCatalog<C>
where
    C: Clone + Ord,
{
    fn discover(&self, text: &str) -> DiscoveryResult<C> {
        discover(text, self)
    }
}

pub(crate) fn tokenize(text: &str) -> Vec<Token> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut byte_offsets = text
        .char_indices()
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    byte_offsets.push(text.len());
    let mut token_ranges = Vec::<CodepointRange>::new();
    let mut current_start = None;

    for (index, character) in characters.iter().copied().enumerate() {
        if is_token_character(character) {
            current_start.get_or_insert(index);
        } else if let Some(start) = current_start.take() {
            push_trimmed_token_range(&characters, start, index, &mut token_ranges);
        }
    }
    if let Some(start) = current_start {
        push_trimmed_token_range(&characters, start, characters.len(), &mut token_ranges);
    }

    let mut previous_end = 0;
    token_ranges
        .into_iter()
        .enumerate()
        .map(|(ordinal, range)| {
            let surface = text[byte_offsets[range.start]..byte_offsets[range.end]].to_owned();
            let hard_boundary_before = characters[previous_end..range.start]
                .iter()
                .copied()
                .any(is_hard_boundary);
            previous_end = range.end;
            Token {
                ordinal,
                range,
                normalized: normalize_surface(&surface),
                surface,
                hard_boundary_before,
            }
        })
        .collect()
}

pub(crate) fn discover<C>(text: &str, catalog: &DiscoveryCatalog<C>) -> DiscoveryResult<C>
where
    C: Clone + Ord,
{
    let tokens = tokenize(text);
    let mut matches = Vec::new();

    for token in &tokens {
        if let Some(posting_id) = catalog.alias_index.get(&token.normalized) {
            let candidates = &catalog.alias_postings[posting_id as usize];
            matches.push(DiscoveryMatch {
                kind: MatchKind::Word,
                source_segments: vec![SourceSegment { range: token.range }],
                normalized_surface: token.normalized.clone(),
                candidates: candidates.clone(),
            });
        }
    }

    if let Some(automaton) = &catalog.contiguous_automaton {
        let (haystack, starts, ends) = contiguous_haystack(&tokens);
        for found in automaton.find_overlapping_iter(&haystack) {
            let Some(&start) = starts.get(&found.start()) else {
                continue;
            };
            let Some(&end) = ends.get(&found.end()) else {
                continue;
            };
            if tokens[start + 1..end]
                .iter()
                .any(|token| token.hard_boundary_before)
            {
                continue;
            }
            let pattern = &catalog.contiguous_patterns[found.pattern().as_usize()];
            matches.push(DiscoveryMatch {
                kind: MatchKind::ContiguousPhrase,
                source_segments: vec![SourceSegment {
                    range: CodepointRange {
                        start: tokens[start].range.start,
                        end: tokens[end - 1].range.end,
                    },
                }],
                normalized_surface: pattern.literals.join(" "),
                candidates: pattern.candidates.clone(),
            });
        }
    }

    for pattern in &catalog.separable_patterns {
        for start in 0..tokens.len() {
            let Some(first_end) = match_literal_at(&tokens, start, &pattern.literal_segments[0])
            else {
                continue;
            };
            let mut matched_token_ranges = vec![(start, first_end)];
            extend_separable_match(
                &tokens,
                pattern,
                1,
                first_end,
                &mut matched_token_ranges,
                &mut matches,
            );
        }
    }

    matches.sort_by(|left, right| {
        let left_start = left.source_segments[0].range.start;
        let right_start = right.source_segments[0].range.start;
        left_start
            .cmp(&right_start)
            .then_with(|| right.source_segments.len().cmp(&left.source_segments.len()))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.normalized_surface.cmp(&right.normalized_surface))
            .then_with(|| left.candidates.cmp(&right.candidates))
    });

    DiscoveryResult { tokens, matches }
}

#[allow(dead_code)] // Reusable for future non-HTTP internal consumers; the admin already sends canonical ranges.
pub(crate) fn merge_selected_token_segments(
    tokens: &[Token],
    selected_ordinals: &[usize],
) -> Result<Vec<SourceSegment>, SelectionError> {
    if selected_ordinals.is_empty() {
        return Err(SelectionError::Empty);
    }

    let mut ordinals = selected_ordinals.to_vec();
    ordinals.sort_unstable();
    ordinals.dedup();

    let mut segments = Vec::<SourceSegment>::new();
    let mut previous_ordinal = None;
    for ordinal in ordinals {
        let token = tokens
            .get(ordinal)
            .filter(|token| token.ordinal == ordinal)
            .ok_or(SelectionError::TokenOutOfRange(ordinal))?;
        if previous_ordinal.is_some_and(|previous| ordinal == previous + 1) {
            if let Some(segment) = segments.last_mut() {
                segment.range.end = token.range.end;
            }
        } else {
            segments.push(SourceSegment { range: token.range });
        }
        previous_ordinal = Some(ordinal);
    }

    Ok(segments)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum SelectionError {
    Empty,
    TokenOutOfRange(usize),
}

pub(crate) fn source_fingerprint(
    text: &str,
    segments: &[SourceSegment],
) -> Result<String, SegmentError> {
    validate_segments(text, segments)?;

    // Versioned FNV-1a is intentionally a stable grouping fingerprint, not a security digest.
    // An HTTP adapter that uses it as a tamper-proof token must wrap it in the platform digest.
    let mut hash = 0xcbf29ce484222325_u64;
    update_stable_hash(&mut hash, b"source-fingerprint-v1");
    update_stable_hash(&mut hash, &(text.len() as u64).to_be_bytes());
    update_stable_hash(&mut hash, text.as_bytes());
    update_stable_hash(&mut hash, &(segments.len() as u64).to_be_bytes());
    for segment in segments {
        update_stable_hash(&mut hash, &(segment.range.start as u64).to_be_bytes());
        update_stable_hash(&mut hash, &(segment.range.end as u64).to_be_bytes());
    }
    Ok(format!("sf1-{hash:016x}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegmentError {
    Empty,
    EmptyRange,
    OutOfRange,
    NotStrictlyOrdered,
}

pub(crate) fn any_segment_intersection(left: &[SourceSegment], right: &[SourceSegment]) -> bool {
    left.iter().any(|left_segment| {
        right.iter().any(|right_segment| {
            left_segment.range.start < right_segment.range.end
                && right_segment.range.start < left_segment.range.end
        })
    })
}

pub(crate) fn codepoint_slice(text: &str, range: CodepointRange) -> Option<&str> {
    if range.end < range.start || range.end > text.chars().count() {
        return None;
    }
    let byte_start = text
        .char_indices()
        .nth(range.start)
        .map_or(text.len(), |(offset, _)| offset);
    let byte_end = text
        .char_indices()
        .nth(range.end)
        .map_or(text.len(), |(offset, _)| offset);
    Some(&text[byte_start..byte_end])
}

fn normalize_literals(literals: Vec<String>) -> Result<Vec<String>, CatalogBuildError> {
    literals
        .into_iter()
        .map(|literal| {
            normalize_single_literal(&literal)
                .map_err(|_| CatalogBuildError::InvalidLiteral(literal))
        })
        .collect()
}

fn normalize_single_literal(literal: &str) -> Result<String, ()> {
    let mut tokens = tokenize(literal);
    if tokens.len() != 1 {
        return Err(());
    }
    let token = tokens.pop().expect("length checked above");
    if token.range.start != 0 || token.range.end != literal.chars().count() {
        return Err(());
    }
    Ok(token.normalized)
}

fn normalize_surface(surface: &str) -> String {
    surface
        .nfkc()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{02bc}' => '\'',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            other => other,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_token_character(character: char) -> bool {
    character.is_alphanumeric() || is_connector(character) || is_combining_mark(character)
}

fn is_connector(character: char) -> bool {
    matches!(
        character,
        '\'' | '\u{2018}' | '\u{2019}' | '\u{02bc}' | '-' | '\u{2010}' | '\u{2011}'
    )
}

fn is_combining_mark(character: char) -> bool {
    matches!(
        character,
        '\u{0300}'..='\u{036f}'
            | '\u{1ab0}'..='\u{1aff}'
            | '\u{1dc0}'..='\u{1dff}'
            | '\u{20d0}'..='\u{20ff}'
            | '\u{fe20}'..='\u{fe2f}'
    )
}

fn is_hard_boundary(character: char) -> bool {
    matches!(
        character,
        '.' | '!' | '?' | ';' | ':' | '\n' | '\r' | '。' | '！' | '？' | '；' | '：'
    )
}

fn push_trimmed_token_range(
    characters: &[char],
    mut start: usize,
    mut end: usize,
    target: &mut Vec<CodepointRange>,
) {
    while start < end && is_connector(characters[start]) {
        start += 1;
    }
    while end > start && is_connector(characters[end - 1]) {
        end -= 1;
    }
    if start < end
        && characters[start..end]
            .iter()
            .any(|character| character.is_alphanumeric())
    {
        target.push(CodepointRange { start, end });
    }
}

fn contiguous_haystack(
    tokens: &[Token],
) -> (String, BTreeMap<usize, usize>, BTreeMap<usize, usize>) {
    let mut haystack = String::new();
    let mut starts = BTreeMap::new();
    let mut ends = BTreeMap::new();
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 {
            haystack.push('\u{1f}');
        }
        starts.insert(haystack.len(), index);
        haystack.push_str(&token.normalized);
        ends.insert(haystack.len(), index + 1);
    }
    (haystack, starts, ends)
}

fn match_literal_at(tokens: &[Token], start: usize, literals: &[String]) -> Option<usize> {
    let end = start.checked_add(literals.len())?;
    if end > tokens.len() {
        return None;
    }
    for (offset, literal) in literals.iter().enumerate() {
        let token = &tokens[start + offset];
        if token.normalized != *literal || (offset > 0 && token.hard_boundary_before) {
            return None;
        }
    }
    Some(end)
}

fn extend_separable_match<C>(
    tokens: &[Token],
    pattern: &SeparablePattern<C>,
    literal_segment_index: usize,
    previous_end: usize,
    matched_token_ranges: &mut Vec<(usize, usize)>,
    matches: &mut Vec<DiscoveryMatch<C>>,
) where
    C: Clone + Ord,
{
    if literal_segment_index == pattern.literal_segments.len() {
        matches.push(DiscoveryMatch {
            kind: MatchKind::SeparablePhrase,
            source_segments: source_segments_from_token_ranges(tokens, matched_token_ranges),
            normalized_surface: pattern
                .literal_segments
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
            candidates: pattern.candidates.clone(),
        });
        return;
    }

    let gap = pattern.gaps[literal_segment_index - 1];
    let largest_possible_gap = tokens.len().saturating_sub(previous_end).saturating_sub(1);
    let max_gap = gap.max_tokens.min(largest_possible_gap);
    if gap.min_tokens > max_gap {
        return;
    }
    for gap_tokens in gap.min_tokens..=max_gap {
        let Some(next_start) = previous_end.checked_add(gap_tokens) else {
            continue;
        };
        if next_start >= tokens.len()
            || tokens[previous_end..=next_start]
                .iter()
                .any(|token| token.hard_boundary_before)
        {
            continue;
        }
        let Some(next_end) = match_literal_at(
            tokens,
            next_start,
            &pattern.literal_segments[literal_segment_index],
        ) else {
            continue;
        };
        matched_token_ranges.push((next_start, next_end));
        extend_separable_match(
            tokens,
            pattern,
            literal_segment_index + 1,
            next_end,
            matched_token_ranges,
            matches,
        );
        matched_token_ranges.pop();
    }
}

fn source_segments_from_token_ranges(
    tokens: &[Token],
    token_ranges: &[(usize, usize)],
) -> Vec<SourceSegment> {
    let mut result = Vec::<SourceSegment>::new();
    let mut previous_token_end = None;
    for &(start, end) in token_ranges {
        let range = CodepointRange {
            start: tokens[start].range.start,
            end: tokens[end - 1].range.end,
        };
        if previous_token_end == Some(start) {
            result
                .last_mut()
                .expect("adjacent segment has a predecessor")
                .range
                .end = range.end;
        } else {
            result.push(SourceSegment { range });
        }
        previous_token_end = Some(end);
    }
    result
}

fn validate_segments(text: &str, segments: &[SourceSegment]) -> Result<(), SegmentError> {
    if segments.is_empty() {
        return Err(SegmentError::Empty);
    }
    let text_len = text.chars().count();
    let mut previous_end = None;
    for segment in segments {
        if segment.range.start == segment.range.end {
            return Err(SegmentError::EmptyRange);
        }
        if segment.range.start > segment.range.end || segment.range.end > text_len {
            return Err(SegmentError::OutOfRange);
        }
        if previous_end.is_some_and(|end| segment.range.start < end) {
            return Err(SegmentError::NotStrictlyOrdered);
        }
        previous_end = Some(segment.range.end);
    }
    Ok(())
}

fn update_stable_hash(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment_surface<'a>(text: &'a str, segment: &SourceSegment) -> &'a str {
        codepoint_slice(text, segment.range).expect("test segment must point into the source")
    }

    fn catalog(
        aliases: Vec<AliasPosting<&'static str>>,
        contiguous: Vec<ContiguousPattern<&'static str>>,
        separable: Vec<SeparablePattern<&'static str>>,
    ) -> DiscoveryCatalog<&'static str> {
        DiscoveryCatalog::build(aliases, contiguous, separable).expect("valid test catalog")
    }

    #[test]
    fn tokenizer_uses_codepoint_ranges_and_preserves_word_internal_connectors() {
        let text = "🙂 Café cafe\u{301} don't well‑known. Next";
        let tokens = tokenize(text);

        assert_eq!(
            tokens
                .iter()
                .map(|token| token.surface.as_str())
                .collect::<Vec<_>>(),
            ["Café", "cafe\u{301}", "don't", "well‑known", "Next"]
        );
        assert_eq!(tokens[0].range.start, 2, "emoji is one Unicode codepoint");
        assert_eq!(tokens[0].normalized, "café");
        assert_eq!(tokens[2].normalized, "don't");
        assert_eq!(tokens[3].normalized, "well-known");
        assert!(tokens[4].hard_boundary_before);
        for token in &tokens {
            assert_eq!(
                codepoint_slice(text, token.range),
                Some(token.surface.as_str())
            );
        }
    }

    #[test]
    fn repeated_alias_occurrences_keep_independent_source_positions() {
        let catalog = catalog(
            vec![AliasPosting {
                surface: "locations".to_owned(),
                candidates: vec!["location-base"],
            }],
            Vec::new(),
            Vec::new(),
        );

        let result = discover("Locations beside locations.", &catalog);
        let word_matches = result
            .matches
            .iter()
            .filter(|item| item.kind == MatchKind::Word)
            .collect::<Vec<_>>();

        assert_eq!(word_matches.len(), 2);
        assert_ne!(
            word_matches[0].source_segments,
            word_matches[1].source_segments
        );
    }

    #[test]
    fn lookup_normalization_unifies_compatibility_and_combining_forms() {
        let catalog = catalog(
            vec![
                AliasPosting {
                    surface: "center".to_owned(),
                    candidates: vec!["center-base"],
                },
                AliasPosting {
                    surface: "café".to_owned(),
                    candidates: vec!["cafe-base"],
                },
            ],
            Vec::new(),
            Vec::new(),
        );

        let result = catalog.discover("ＣＥＮＴＥＲ cafe\u{301}");

        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.matches[0].candidates, ["center-base"]);
        assert_eq!(result.matches[1].candidates, ["cafe-base"]);
    }

    #[test]
    fn one_alias_returns_every_base_candidate_without_guessing() {
        let catalog = catalog(
            vec![AliasPosting {
                surface: "ran".to_owned(),
                candidates: vec!["run-verb-b", "run-verb-a", "run-verb-a"],
            }],
            Vec::new(),
            Vec::new(),
        );

        let result = discover("ran", &catalog);

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].candidates, ["run-verb-a", "run-verb-b"]);
    }

    #[test]
    fn contiguous_phrase_matching_returns_overlapping_short_and_long_patterns() {
        let catalog = catalog(
            Vec::new(),
            vec![
                ContiguousPattern {
                    literals: vec!["central".to_owned(), "location".to_owned()],
                    candidates: vec!["central-location-base"],
                },
                ContiguousPattern {
                    literals: vec!["in".to_owned(), "central".to_owned(), "location".to_owned()],
                    candidates: vec!["in-central-location-base"],
                },
                ContiguousPattern {
                    literals: vec!["cat".to_owned(), "nap".to_owned()],
                    candidates: vec!["cat-base"],
                },
            ],
            Vec::new(),
        );

        let result = discover("concatenate in central location", &catalog);
        let phrase_matches = result
            .matches
            .iter()
            .filter(|item| item.kind == MatchKind::ContiguousPhrase)
            .collect::<Vec<_>>();

        assert_eq!(phrase_matches.len(), 2);
        assert!(phrase_matches.iter().any(|item| {
            item.normalized_surface == "central location"
                && item.candidates == ["central-location-base"]
        }));
        assert!(phrase_matches.iter().any(|item| {
            item.normalized_surface == "in central location"
                && item.candidates == ["in-central-location-base"]
        }));
        assert!(
            phrase_matches
                .iter()
                .all(|item| item.candidates != ["cat-base"]),
            "token matching must not treat cat as a substring of concatenate"
        );
    }

    #[test]
    fn duplicate_contiguous_patterns_merge_their_candidate_postings() {
        let catalog = catalog(
            Vec::new(),
            vec![
                ContiguousPattern {
                    literals: vec!["central".to_owned(), "location".to_owned()],
                    candidates: vec!["candidate-b"],
                },
                ContiguousPattern {
                    literals: vec!["Central".to_owned(), "LOCATION".to_owned()],
                    candidates: vec!["candidate-a"],
                },
            ],
            Vec::new(),
        );

        let result = discover("central location", &catalog);

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].candidates, ["candidate-a", "candidate-b"]);
    }

    #[test]
    fn contiguous_phrase_does_not_cross_a_hard_sentence_boundary() {
        let catalog = catalog(
            Vec::new(),
            vec![ContiguousPattern {
                literals: vec!["central".to_owned(), "location".to_owned()],
                candidates: vec!["central-location-base"],
            }],
            Vec::new(),
        );

        let result = catalog.discover("central. location");

        assert!(result.matches.is_empty());
    }

    #[test]
    fn separable_pattern_obeys_literal_order_and_gap_bounds() {
        let catalog = catalog(
            Vec::new(),
            Vec::new(),
            vec![SeparablePattern {
                literal_segments: vec![vec!["turn".to_owned()], vec!["off".to_owned()]],
                gaps: vec![GapRule {
                    min_tokens: 1,
                    max_tokens: 3,
                }],
                candidates: vec!["turn-off-base"],
            }],
        );
        let text =
            "turn the light off; turn it immediately off; turn off; turn one two three four off";

        let result = discover(text, &catalog);
        let matches = result
            .matches
            .iter()
            .filter(|item| item.kind == MatchKind::SeparablePhrase)
            .collect::<Vec<_>>();

        assert_eq!(matches.len(), 2);
        for item in matches {
            assert_eq!(item.source_segments.len(), 2);
            assert_eq!(segment_surface(text, &item.source_segments[0]), "turn");
            assert_eq!(segment_surface(text, &item.source_segments[1]), "off");
        }
    }

    #[test]
    fn separable_pattern_never_crosses_a_hard_punctuation_boundary() {
        let catalog = catalog(
            Vec::new(),
            Vec::new(),
            vec![SeparablePattern {
                literal_segments: vec![vec!["turn".to_owned()], vec!["off".to_owned()]],
                gaps: vec![GapRule {
                    min_tokens: 1,
                    max_tokens: 8,
                }],
                candidates: vec!["turn-off-base"],
            }],
        );

        let result = discover("turn the light. off", &catalog);

        assert!(result.matches.is_empty());
    }

    #[test]
    fn separable_gap_is_lexical_only_and_large_limits_are_bounded_by_the_sentence() {
        let catalog = catalog(
            Vec::new(),
            Vec::new(),
            vec![SeparablePattern {
                literal_segments: vec![vec!["turn".to_owned()], vec!["off".to_owned()]],
                gaps: vec![GapRule {
                    min_tokens: 1,
                    max_tokens: usize::MAX,
                }],
                candidates: vec!["turn-off-base"],
            }],
        );

        let result = catalog.discover("turn quickly off");

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].source_segments.len(), 2);
    }

    #[test]
    fn zero_gap_separable_literals_are_canonicalized_to_one_segment() {
        let catalog = catalog(
            Vec::new(),
            Vec::new(),
            vec![SeparablePattern {
                literal_segments: vec![vec!["turn".to_owned()], vec!["off".to_owned()]],
                gaps: vec![GapRule {
                    min_tokens: 0,
                    max_tokens: 0,
                }],
                candidates: vec!["turn-off-base"],
            }],
        );

        let result = catalog.discover("turn off");

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].source_segments.len(), 1);
        assert_eq!(
            segment_surface("turn off", &result.matches[0].source_segments[0]),
            "turn off"
        );
    }

    #[test]
    fn selected_tokens_are_sorted_deduplicated_and_adjacent_runs_are_merged() {
        let text = "turn the light off today";
        let tokens = tokenize(text);

        let segments =
            merge_selected_token_segments(&tokens, &[4, 2, 0, 1, 1]).expect("selection is valid");

        assert_eq!(segments.len(), 2);
        assert_eq!(segment_surface(text, &segments[0]), "turn the light");
        assert_eq!(segment_surface(text, &segments[1]), "today");
    }

    #[test]
    fn invalid_manual_selection_and_catalog_shapes_fail_closed() {
        let tokens = tokenize("turn off");
        assert_eq!(
            merge_selected_token_segments(&tokens, &[]),
            Err(SelectionError::Empty)
        );
        assert_eq!(
            merge_selected_token_segments(&tokens, &[2]),
            Err(SelectionError::TokenOutOfRange(2))
        );
        assert!(matches!(
            DiscoveryCatalog::<&str>::build(
                vec![AliasPosting {
                    surface: "two words".to_owned(),
                    candidates: vec!["invalid"],
                }],
                Vec::new(),
                Vec::new(),
            ),
            Err(CatalogBuildError::InvalidAliasSurface(surface)) if surface == "two words"
        ));
        assert!(matches!(
            DiscoveryCatalog::<&str>::build(
                Vec::new(),
                vec![ContiguousPattern {
                    literals: vec!["word".to_owned()],
                    candidates: vec!["invalid"],
                }],
                Vec::new(),
            ),
            Err(CatalogBuildError::EmptyContiguousPattern)
        ));
        assert!(matches!(
            DiscoveryCatalog::<&str>::build(
                Vec::new(),
                Vec::new(),
                vec![SeparablePattern {
                    literal_segments: vec![vec!["turn".to_owned()], vec!["off".to_owned()]],
                    gaps: Vec::new(),
                    candidates: vec!["invalid"],
                }],
            ),
            Err(CatalogBuildError::InvalidGapShape)
        ));
    }

    #[test]
    fn source_fingerprint_is_stable_and_binds_text_and_positions() {
        let text = "turn the light off";
        let tokens = tokenize(text);
        let separated = merge_selected_token_segments(&tokens, &[0, 3]).expect("valid selection");
        let middle = merge_selected_token_segments(&tokens, &[1, 2]).expect("valid selection");

        let first = source_fingerprint(text, &separated).expect("valid segments");
        let second = source_fingerprint(text, &separated).expect("valid segments");

        assert_eq!(first, second);
        assert_ne!(
            first,
            source_fingerprint("Turn the light off", &separated).expect("same codepoint layout")
        );
        assert_ne!(
            first,
            source_fingerprint(text, &middle).expect("different positions")
        );
    }

    #[test]
    fn source_fingerprint_rejects_empty_out_of_range_and_overlapping_segments() {
        let text = "turn off";
        assert_eq!(source_fingerprint(text, &[]), Err(SegmentError::Empty));
        assert_eq!(
            source_fingerprint(
                text,
                &[SourceSegment {
                    range: CodepointRange { start: 4, end: 4 },
                }]
            ),
            Err(SegmentError::EmptyRange)
        );
        assert_eq!(
            source_fingerprint(
                text,
                &[SourceSegment {
                    range: CodepointRange { start: 0, end: 9 },
                }]
            ),
            Err(SegmentError::OutOfRange)
        );
        assert_eq!(
            source_fingerprint(
                text,
                &[
                    SourceSegment {
                        range: CodepointRange { start: 0, end: 4 },
                    },
                    SourceSegment {
                        range: CodepointRange { start: 3, end: 8 },
                    },
                ]
            ),
            Err(SegmentError::NotStrictlyOrdered)
        );
    }

    #[test]
    fn segment_intersection_uses_only_real_half_open_ranges() {
        let turn_and_off = [
            SourceSegment {
                range: CodepointRange { start: 0, end: 4 },
            },
            SourceSegment {
                range: CodepointRange { start: 15, end: 18 },
            },
        ];
        let middle = [SourceSegment {
            range: CodepointRange { start: 5, end: 14 },
        }];
        let shared_turn = [SourceSegment {
            range: CodepointRange { start: 0, end: 4 },
        }];
        let touching_after = [SourceSegment {
            range: CodepointRange { start: 18, end: 20 },
        }];

        assert!(!any_segment_intersection(&turn_and_off, &middle));
        assert!(any_segment_intersection(&turn_and_off, &shared_turn));
        assert!(!any_segment_intersection(&turn_and_off, &touching_after));
    }
}
