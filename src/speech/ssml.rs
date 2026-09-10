use crate::lexicon::dto::{RichTextAnnotation, RichTextEmphasisLevel, RichTextPhonemeAlphabet};

use super::{SpeechModelError, SynthesisRequest};

pub fn build_ssml(request: &SynthesisRequest) -> Result<String, SpeechModelError> {
    let mut output = String::with_capacity(request.content().text.len() + 256);
    output.push_str(r#"<speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" xmlns:mstts="https://www.w3.org/2001/mstts" xml:lang=""#);
    push_attr(&mut output, request.voice().locale());
    output.push_str(r#""><voice name=""#);
    push_attr(&mut output, request.voice().provider_voice_id());
    output.push_str(r#"">"#);
    if let Some(style) = request.options().style() {
        output.push_str(r#"<mstts:express-as style=""#);
        push_attr(&mut output, style);
        output.push_str(r#"">"#);
    }
    output.push_str("<prosody rate=\"");
    output.push_str(&format_signed(request.options().rate_percent(), "%"));
    output.push_str("\" pitch=\"");
    output.push_str(&format_signed(request.options().pitch_semitones(), "st"));
    output.push_str("\">");

    // 词性提示符（a job 里的 a）是教学标注，只指示词性，不参与朗读：这一段的字符、
    // 它自己的 emphasis、以及整段被它盖住的音标都不进 SSML，否则空的 phoneme 标签
    // 会把 ph 里的音标当正文读出来。
    let silent = request
        .content()
        .annotations
        .iter()
        .filter_map(|annotation| match annotation {
            RichTextAnnotation::Emphasis {
                start,
                end,
                level: RichTextEmphasisLevel::Grammar,
            } => Some((*start, *end)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let is_silent = |position: usize| {
        silent
            .iter()
            .any(|(start, end)| *start <= position && position < *end)
    };

    let ranges = request
        .content()
        .annotations
        .iter()
        .filter(|annotation| {
            matches!(
                annotation,
                RichTextAnnotation::Emphasis { .. } | RichTextAnnotation::Phoneme { .. }
            )
        })
        .filter(|annotation| match annotation {
            RichTextAnnotation::Emphasis {
                level: RichTextEmphasisLevel::Grammar,
                ..
            } => false,
            _ => range(annotation)
                .is_none_or(|(start, end)| (start..end).any(|position| !is_silent(position))),
        })
        .collect::<Vec<_>>();
    let codepoints = request.content().text.chars().collect::<Vec<_>>();
    let mut spoken = false;

    for position in 0..=codepoints.len() {
        let mut ending = ranges
            .iter()
            .copied()
            .filter(|annotation| range(annotation).is_some_and(|(_, end)| end == position))
            .collect::<Vec<_>>();
        ending.sort_by_key(|annotation| {
            (
                std::cmp::Reverse(range(annotation).unwrap().0),
                match annotation {
                    RichTextAnnotation::Phoneme { .. } => 0,
                    RichTextAnnotation::Emphasis { .. } => 1,
                    _ => unreachable!(),
                },
            )
        });
        for annotation in ending {
            close_range(&mut output, annotation);
        }

        for annotation in request.content().annotations.iter().filter(|annotation| {
            // 提示符整段当作不存在，连它自己带的停顿也不该留在朗读里。
            matches!(annotation, RichTextAnnotation::Pause { at, .. }
                if *at == position && !is_silent(position))
        }) {
            if let RichTextAnnotation::Pause { duration_ms, .. } = annotation {
                output.push_str("<break time=\"");
                output.push_str(&duration_ms.to_string());
                output.push_str("ms\"/>");
            }
        }

        let mut starting = ranges
            .iter()
            .copied()
            .filter(|annotation| range(annotation).is_some_and(|(start, _)| start == position))
            .collect::<Vec<_>>();
        starting.sort_by_key(|annotation| {
            (
                std::cmp::Reverse(range(annotation).unwrap().1),
                match annotation {
                    RichTextAnnotation::Emphasis { .. } => 0,
                    RichTextAnnotation::Phoneme { .. } => 1,
                    _ => unreachable!(),
                },
            )
        });
        for annotation in starting {
            open_range(&mut output, annotation)?;
        }

        if let Some(character) = codepoints.get(position) {
            // 提示符里的空白照常留下：吞掉它会让两侧的词粘成一个，
            //「go to a school」标了「 a 」就会读成「go toschool」。
            if !is_silent(position) || character.is_whitespace() {
                push_text_character(&mut output, *character);
                spoken |= !character.is_whitespace();
            }
        }
    }

    // 空正文和整段都是提示符是同一回事：没有东西可念，别把空 prosody 送给供应商。
    if !spoken {
        return Err(SpeechModelError::NothingToSpeak);
    }

    output.push_str("</prosody>");
    if request.options().style().is_some() {
        output.push_str("</mstts:express-as>");
    }
    output.push_str("</voice></speak>");
    Ok(output)
}

fn range(annotation: &RichTextAnnotation) -> Option<(usize, usize)> {
    match annotation {
        RichTextAnnotation::Emphasis { start, end, .. }
        | RichTextAnnotation::Phoneme { start, end, .. } => Some((*start, *end)),
        _ => None,
    }
}

fn open_range(
    output: &mut String,
    annotation: &RichTextAnnotation,
) -> Result<(), SpeechModelError> {
    match annotation {
        // 三分类（功能词 / 核心词 / 语法词）是教学标注，不是韵律指令；试听一律沿用
        // 三分类落地之前的 `strong`，等产品定了各类该怎么读再分开映射。
        RichTextAnnotation::Emphasis { .. } => output.push_str(r#"<emphasis level="strong">"#),
        RichTextAnnotation::Phoneme {
            alphabet: RichTextPhonemeAlphabet::Ipa,
            phoneme,
            ..
        } => {
            output.push_str(r#"<phoneme alphabet="ipa" ph=""#);
            push_attr(output, phoneme.trim());
            output.push_str(r#"">"#);
        }
        _ => return Err(SpeechModelError::InvalidRichText),
    }
    Ok(())
}

fn close_range(output: &mut String, annotation: &RichTextAnnotation) {
    match annotation {
        RichTextAnnotation::Emphasis { .. } => output.push_str("</emphasis>"),
        RichTextAnnotation::Phoneme { .. } => output.push_str("</phoneme>"),
        _ => unreachable!("only speech range annotations are collected"),
    }
}

fn format_signed<T: std::fmt::Display + PartialOrd + Default>(value: T, suffix: &str) -> String {
    let sign = if value >= T::default() { "+" } else { "" };
    format!("{sign}{value}{suffix}")
}

fn push_text_character(output: &mut String, character: char) {
    match character {
        '&' => output.push_str("&amp;"),
        '<' => output.push_str("&lt;"),
        '>' => output.push_str("&gt;"),
        _ => output.push(character),
    }
}

fn push_attr(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ => output.push(character),
        }
    }
}
