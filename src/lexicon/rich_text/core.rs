use super::*;

// --- canonicalization ---

pub fn canonicalize(value: &mut RichText) -> Result<(), Vec<RichTextIssue>> {
    match value {
        RichText::V1(value) => canonicalize_v1(value),
        RichText::V2(value) => canonicalize_v2(value),
    }
}

pub fn is_valid(value: &RichText) -> bool {
    let mut value = value.clone();
    canonicalize(&mut value).is_ok()
}

/// Validates and canonicalizes every rich-text value in the meanings payload.
///
/// Invalid values are left untouched so the regular draft validator can attach
/// the error to the owning domain node. Valid V2 values are normalized before
/// they are hashed or persisted.
pub fn canonicalize_meanings(content: &mut DraftMeaningsStepContent) -> bool {
    let mut valid = true;
    for pos in &mut content.pos {
        for grammar in &mut pos.grammar_structures {
            for variant in &mut grammar.variants {
                valid &= canonicalize(&mut variant.content).is_ok();
            }
        }
        for sense in &mut pos.senses {
            for definition in &mut sense.definitions {
                match definition {
                    WordDefinitionV2::ZhDefinition { content, .. }
                    | WordDefinitionV2::ZhSentence { content, .. } => {
                        valid &= canonicalize(content).is_ok();
                    }
                    WordDefinitionV2::EnDefinition { content, .. }
                    | WordDefinitionV2::EnSentence { content, .. } => {
                        valid &= canonicalize_english_text(content);
                    }
                }
            }
            for sentence in &mut sense.sentences {
                valid &= canonicalize_english_text(&mut sentence.en_text);
                valid &= canonicalize(&mut sentence.zh_text).is_ok();
            }
        }
    }
    valid
}

fn canonicalize_english_text(value: &mut EnglishTextV2) -> bool {
    match value {
        EnglishTextV2::Unified { common } => canonicalize(&mut common.value).is_ok(),
        EnglishTextV2::Distinguish { uk, us, .. } => {
            let mut valid = true;
            for slot in [uk, us] {
                if let DialectVariantSlotV2::Ready { variant } = slot {
                    valid &= canonicalize(&mut variant.value).is_ok();
                }
            }
            valid
        }
    }
}

// Compatibility path for the legacy Go wire: keep the two 500-item limits
// independent and reject a liaison at the final codepoint. V2 alone follows
// the stricter voice-editor annotation rules.
fn canonicalize_v1(value: &mut RichTextV1) -> Result<(), Vec<RichTextIssue>> {
    let mut issues = Vec::new();
    let codepoints = value.text.chars().count();
    if value.version != 1 {
        push_issue(
            &mut issues,
            "invalid_version",
            "version",
            "RichText V1 的 version 必须为 1",
        );
    }
    if codepoints > MAX_RICH_TEXT_CODEPOINTS {
        push_issue(
            &mut issues,
            "text_too_long",
            "text",
            "正文不能超过 5000 个 Unicode 码点",
        );
    }
    if value.text.contains('\0') {
        push_issue(
            &mut issues,
            "nul_character_not_allowed",
            "text",
            "正文不能包含 NUL 字符",
        );
    }
    if value.spans.len() > MAX_RICH_TEXT_ANNOTATIONS
        || value.liaisons.len() > MAX_RICH_TEXT_ANNOTATIONS
    {
        push_issue(
            &mut issues,
            "too_many_annotations",
            "spans",
            "标注不能超过 500 个",
        );
    }
    for (index, span) in value.spans.iter().enumerate() {
        if span.start >= span.end || span.end > codepoints {
            push_issue(
                &mut issues,
                "invalid_range",
                format!("spans[{index}]"),
                "标注区间必须是正文内非空的 [start, end)",
            );
        }
    }
    for (index, liaison) in value.liaisons.iter().enumerate() {
        let end = liaison.checked_add(2);
        if end.is_none_or(|end| end > codepoints) {
            push_issue(
                &mut issues,
                "invalid_range",
                format!("liaisons[{index}]"),
                "连读位置必须连接正文内相邻的两个码点",
            );
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

fn canonicalize_v2(value: &mut RichTextV2) -> Result<(), Vec<RichTextIssue>> {
    let issues = validate_v2(value);
    if !issues.is_empty() {
        return Err(issues);
    }

    let mut pauses = BTreeMap::new();
    let mut ranges = Vec::new();
    for annotation in value.annotations.drain(..) {
        if let RichTextAnnotation::Pause { at, .. } = annotation {
            pauses.insert(at, annotation);
        } else {
            ranges.push(annotation);
        }
    }
    ranges.sort_by(compare_range_annotations);

    let mut merged: Vec<RichTextAnnotation> = Vec::new();
    for annotation in ranges {
        if matches!(annotation, RichTextAnnotation::Phoneme { .. }) {
            merged.push(annotation);
            continue;
        }
        let (start, end) = range_bounds(&annotation).expect("range annotation");
        let previous = merged
            .iter_mut()
            .rev()
            .find(|candidate| same_merge_attributes(candidate, &annotation));
        if let Some(previous) = previous {
            let (_, previous_end) = range_bounds(previous).expect("mergeable range annotation");
            if start <= previous_end {
                set_range_end(previous, previous_end.max(end));
                continue;
            }
        }
        merged.push(annotation);
    }

    value.annotations = merged.into_iter().chain(pauses.into_values()).collect();
    value.annotations.sort_by(compare_final_annotations);
    Ok(())
}

// --- validation ---

pub(super) fn validate_v2(value: &RichTextV2) -> Vec<RichTextIssue> {
    let mut issues = Vec::new();
    let codepoints = value.text.chars().count();
    if value.version != 2 {
        push_issue(
            &mut issues,
            "invalid_version",
            "version",
            "RichText V2 的 version 必须为 2",
        );
    }
    if codepoints > MAX_RICH_TEXT_CODEPOINTS {
        push_issue(
            &mut issues,
            "text_too_long",
            "text",
            "正文不能超过 5000 个 Unicode 码点",
        );
    }
    if value.text.contains('\0') {
        push_issue(
            &mut issues,
            "nul_character_not_allowed",
            "text",
            "正文不能包含 NUL 字符",
        );
    }
    if value.annotations.len() > MAX_RICH_TEXT_ANNOTATIONS {
        push_issue(
            &mut issues,
            "too_many_annotations",
            "annotations",
            "标注不能超过 500 个",
        );
    }

    for (index, annotation) in value.annotations.iter().enumerate() {
        let path = format!("annotations[{index}]");
        match annotation {
            RichTextAnnotation::Pause { at, duration_ms } => {
                if *at > codepoints || !(1..=MAX_PAUSE_MS).contains(duration_ms) {
                    push_issue(
                        &mut issues,
                        "invalid_pause",
                        path,
                        "停顿位置必须合法，时长必须是 1–5000ms 的整数",
                    );
                }
            }
            RichTextAnnotation::Phoneme {
                start,
                end,
                phoneme,
                ..
            } => {
                validate_range(&mut issues, &value.text, *start, *end, &path);
                let phoneme = phoneme.trim();
                if phoneme.contains('\0') {
                    push_issue(
                        &mut issues,
                        "nul_character_not_allowed",
                        format!("{path}.phoneme"),
                        "IPA 不能包含 NUL 字符",
                    );
                }
                if phoneme.is_empty() || phoneme.chars().count() > MAX_PHONEME_CODEPOINTS {
                    push_issue(
                        &mut issues,
                        "invalid_phoneme",
                        path,
                        "IPA 不能为空且不能超过 200 个码点",
                    );
                }
            }
            annotation => {
                let (start, end) = range_bounds(annotation).expect("non-pause annotation");
                validate_range(&mut issues, &value.text, start, end, &path);
            }
        }
    }

    let mut phonemes = value
        .annotations
        .iter()
        .filter(|annotation| matches!(annotation, RichTextAnnotation::Phoneme { .. }))
        .collect::<Vec<_>>();
    phonemes.sort_by(|left, right| compare_range_positions(left, right));
    if phonemes.windows(2).any(|window| {
        let (_, previous_end) = range_bounds(window[0]).expect("phoneme range");
        let (next_start, _) = range_bounds(window[1]).expect("phoneme range");
        next_start < previous_end
    }) {
        push_issue(
            &mut issues,
            "overlapping_phoneme",
            "annotations",
            "IPA 标注之间不能重叠",
        );
    }

    let emphases = value
        .annotations
        .iter()
        .filter(|annotation| matches!(annotation, RichTextAnnotation::Emphasis { .. }))
        .collect::<Vec<_>>();
    let mut crossing_found = false;
    let mut pause_inside_found = false;
    for phoneme in phonemes {
        let (phoneme_start, phoneme_end) = range_bounds(phoneme).expect("phoneme range");
        if !crossing_found
            && emphases.iter().any(|emphasis| {
                let (start, end) = range_bounds(emphasis).expect("emphasis range");
                start < phoneme_end
                    && end > phoneme_start
                    && (start > phoneme_start || end < phoneme_end)
            })
        {
            push_issue(
                &mut issues,
                "crossing_speech_marks",
                "annotations",
                "重音若与 IPA 重叠，必须完整包含 IPA 区间",
            );
            crossing_found = true;
        }
        if !pause_inside_found
            && value.annotations.iter().any(|annotation| {
                matches!(annotation, RichTextAnnotation::Pause { at, .. }
                    if *at > phoneme_start && *at < phoneme_end)
            })
        {
            push_issue(
                &mut issues,
                "pause_inside_phoneme",
                "annotations",
                "停顿不能插在 IPA 标注内部",
            );
            pause_inside_found = true;
        }
    }
    issues
}

fn validate_range(
    issues: &mut Vec<RichTextIssue>,
    text: &str,
    start: usize,
    end: usize,
    path: &str,
) {
    let codepoints = text.chars().count();
    if start >= end || end > codepoints {
        push_issue(
            issues,
            "invalid_range",
            path,
            "标注区间必须是正文内非空的 [start, end)",
        );
    } else if range_contains_newline(text, start, end) {
        push_issue(issues, "cross_paragraph", path, "标注不能跨越段落换行");
    }
}

fn range_contains_newline(text: &str, start: usize, end: usize) -> bool {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .any(|character| character == '\n')
}

fn compare_range_positions(left: &RichTextAnnotation, right: &RichTextAnnotation) -> Ordering {
    let (left_start, left_end) = range_bounds(left).expect("range annotation");
    let (right_start, right_end) = range_bounds(right).expect("range annotation");
    left_start.cmp(&right_start).then(left_end.cmp(&right_end))
}

pub(super) fn compare_range_annotations(
    left: &RichTextAnnotation,
    right: &RichTextAnnotation,
) -> Ordering {
    annotation_position(left)
        .cmp(&annotation_position(right))
        .then_with(|| {
            range_bounds(left)
                .map(|(_, end)| end)
                .unwrap_or(annotation_position(left))
                .cmp(
                    &range_bounds(right)
                        .map(|(_, end)| end)
                        .unwrap_or(annotation_position(right)),
                )
        })
        .then(annotation_type_rank(left).cmp(&annotation_type_rank(right)))
        .then_with(|| annotation_json(left).cmp(&annotation_json(right)))
}

pub(super) fn compare_final_annotations(
    left: &RichTextAnnotation,
    right: &RichTextAnnotation,
) -> Ordering {
    annotation_position(left)
        .cmp(&annotation_position(right))
        .then(annotation_type_rank(left).cmp(&annotation_type_rank(right)))
        .then_with(|| annotation_json(left).cmp(&annotation_json(right)))
}

fn annotation_position(annotation: &RichTextAnnotation) -> usize {
    match annotation {
        RichTextAnnotation::Pause { at, .. } => *at,
        annotation => range_bounds(annotation).expect("range annotation").0,
    }
}

fn annotation_type_rank(annotation: &RichTextAnnotation) -> u8 {
    match annotation {
        RichTextAnnotation::Pause { .. } => 0,
        RichTextAnnotation::Highlight { .. } => 1,
        RichTextAnnotation::Liaison { .. } => 2,
        RichTextAnnotation::Emphasis { .. } => 3,
        RichTextAnnotation::Phoneme { .. } => 4,
    }
}

fn annotation_json(annotation: &RichTextAnnotation) -> String {
    serde_json::to_string(annotation).expect("RichText annotation serialization is infallible")
}

pub(super) fn range_bounds(annotation: &RichTextAnnotation) -> Option<(usize, usize)> {
    match annotation {
        RichTextAnnotation::Emphasis { start, end, .. }
        | RichTextAnnotation::Phoneme { start, end, .. }
        | RichTextAnnotation::Liaison { start, end }
        | RichTextAnnotation::Highlight { start, end, .. } => Some((*start, *end)),
        RichTextAnnotation::Pause { .. } => None,
    }
}

pub(super) fn set_range_end(annotation: &mut RichTextAnnotation, next_end: usize) {
    match annotation {
        RichTextAnnotation::Emphasis { end, .. }
        | RichTextAnnotation::Phoneme { end, .. }
        | RichTextAnnotation::Liaison { end, .. }
        | RichTextAnnotation::Highlight { end, .. } => *end = next_end,
        RichTextAnnotation::Pause { .. } => unreachable!("pause is not a range annotation"),
    }
}

pub(super) fn same_merge_attributes(left: &RichTextAnnotation, right: &RichTextAnnotation) -> bool {
    match (left, right) {
        (
            RichTextAnnotation::Emphasis { level: left, .. },
            RichTextAnnotation::Emphasis { level: right, .. },
        ) => left == right,
        (
            RichTextAnnotation::Highlight { color: left, .. },
            RichTextAnnotation::Highlight { color: right, .. },
        ) => left == right,
        (RichTextAnnotation::Liaison { .. }, RichTextAnnotation::Liaison { .. }) => true,
        _ => false,
    }
}

pub(super) fn push_issue(
    issues: &mut Vec<RichTextIssue>,
    code: &'static str,
    path: impl Into<String>,
    message: &'static str,
) {
    issues.push(RichTextIssue {
        code,
        path: path.into(),
        message,
    });
}
