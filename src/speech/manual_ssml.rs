use std::collections::BTreeMap;

use quick_xml::{Reader, events::Event};

use super::{AudioOutputFormat, SpeechModelError, SpeechOptions, SynthesisRequest, Voice};
use crate::lexicon::dto::RichTextV2;

fn invalid() -> SpeechModelError {
    SpeechModelError::InvalidRichText
}

pub(crate) fn request(
    voice: Voice,
    raw: String,
    format: AudioOutputFormat,
    rate_bounds: (i16, i16),
    pitch_bounds: (i16, i16),
) -> Result<SynthesisRequest, SpeechModelError> {
    if raw.len() > 32_000 || raw.trim().is_empty() || raw.chars().any(|ch| !matches!(ch, '\u{9}' | '\u{a}' | '\u{d}' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')) {
        return Err(invalid());
    }
    let mut reader = Reader::from_str(&raw);
    let mut stack = Vec::<String>::new();
    let mut voices = 0;
    let mut root = false;
    let mut declared = false;
    let mut mstts_namespace = false;
    let mut finished = false;
    let mut text = String::new();
    loop {
        let event = reader.read_event().map_err(|_| invalid())?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(element) | Event::Empty(element) => {
                let name = std::str::from_utf8(element.name().as_ref())
                    .map_err(|_| invalid())?
                    .to_owned();
                if stack.len() >= 32 || finished {
                    return Err(invalid());
                }
                let attrs = element
                    .attributes()
                    .map(|attribute| {
                        let attribute = attribute.map_err(|_| invalid())?;
                        Ok((
                            std::str::from_utf8(attribute.key.as_ref())
                                .map_err(|_| invalid())?
                                .to_owned(),
                            attribute
                                .decoded_and_normalized_value(
                                    quick_xml::XmlVersion::Explicit1_0,
                                    reader.decoder(),
                                )
                                .map_err(|_| invalid())?
                                .into_owned(),
                        ))
                    })
                    .collect::<Result<BTreeMap<_, _>, SpeechModelError>>()?;
                if name == "mstts:express-as" && !mstts_namespace {
                    return Err(invalid());
                }
                validate_element(&voice, &name, &attrs, &stack, rate_bounds, pitch_bounds)?;
                if name == "speak" {
                    if root {
                        return Err(invalid());
                    }
                    root = true;
                    mstts_namespace = attrs.contains_key("xmlns:mstts");
                }
                if name == "voice" {
                    voices += 1;
                    if voices > 1 {
                        return Err(invalid());
                    }
                }
                if !empty {
                    stack.push(name);
                } else if name == "speak" {
                    finished = true;
                }
            }
            Event::End(element) => {
                let name = std::str::from_utf8(element.name().as_ref())
                    .map_err(|_| invalid())?
                    .to_owned();
                if stack.pop().as_deref() != Some(name.as_str()) {
                    return Err(invalid());
                }
                if stack.is_empty() {
                    finished = true;
                }
            }
            Event::Text(value) => {
                let value = value.decode().map_err(|_| invalid())?;
                if !value.trim().is_empty() && !stack.iter().any(|name| name == "voice") {
                    return Err(invalid());
                }
                text.push_str(&value);
            }
            Event::GeneralRef(value) => {
                if !stack.iter().any(|name| name == "voice") {
                    return Err(invalid());
                }
                let character =
                    if let Some(character) = value.resolve_char_ref().map_err(|_| invalid())? {
                        character
                    } else {
                        match value.decode().map_err(|_| invalid())?.as_ref() {
                            "amp" => '&',
                            "lt" => '<',
                            "gt" => '>',
                            "quot" => '"',
                            "apos" => '\'',
                            _ => return Err(invalid()),
                        }
                    };
                if !matches!(character, '\u{9}' | '\u{a}' | '\u{d}' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
                {
                    return Err(invalid());
                }
                text.push(character);
            }
            Event::Comment(_) => {}
            Event::Decl(value) if !root && !declared => {
                if value.version().map_err(|_| invalid())?.as_ref() != b"1.0"
                    || value
                        .encoding()
                        .transpose()
                        .map_err(|_| invalid())?
                        .is_some_and(|encoding| !encoding.eq_ignore_ascii_case(b"utf-8"))
                {
                    return Err(invalid());
                }
                declared = true;
            }
            Event::Eof => break,
            _ => return Err(invalid()),
        }
        if text.chars().count() > 500 {
            return Err(invalid());
        }
    }
    if !root || !finished || !stack.is_empty() || voices != 1 || text.trim().is_empty() {
        return Err(invalid());
    }
    let options = SpeechOptions::new(&voice, None, 0, 0, 100)?.with_output_format(format);
    Ok(SynthesisRequest::new(
        voice,
        options,
        RichTextV2 {
            version: 2,
            text,
            annotations: vec![],
        },
    )?
    .with_manual_ssml(raw))
}

fn number(value: &str, suffix: &str) -> Result<i16, SpeechModelError> {
    value
        .strip_suffix(suffix)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())
}
fn validate_element(
    voice: &Voice,
    name: &str,
    attrs: &BTreeMap<String, String>,
    stack: &[String],
    rate: (i16, i16),
    pitch: (i16, i16),
) -> Result<(), SpeechModelError> {
    let allowed: &[&str] = match name {
        "speak" => &["version", "xmlns", "xmlns:mstts", "xml:lang"],
        "voice" => &["name"],
        "prosody" => &["rate", "pitch", "volume"],
        "break" => &["time"],
        "phoneme" => &["alphabet", "ph"],
        "say-as" => &["interpret-as"],
        "sub" => &["alias"],
        "mstts:express-as" => &["style", "styledegree", "role"],
        "p" | "s" => &[],
        _ => return Err(invalid()),
    };
    if attrs.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid());
    }
    let get = |key: &str| attrs.get(key).map(String::as_str);
    match name {
        "speak" => {
            if !stack.is_empty()
                || get("version") != Some("1.0")
                || get("xmlns") != Some("http://www.w3.org/2001/10/synthesis")
                || get("xml:lang") != Some(voice.locale())
                || get("xmlns:mstts").is_some_and(|value| value != "https://www.w3.org/2001/mstts")
            {
                return Err(invalid());
            }
        }
        "voice" => {
            if stack != ["speak"] || get("name") != Some(voice.provider_voice_id()) {
                return Err(invalid());
            }
        }
        _ => {
            if !stack.iter().any(|value| value == "voice")
                || stack
                    .iter()
                    .any(|value| ["phoneme", "say-as", "sub", "break"].contains(&value.as_str()))
            {
                return Err(invalid());
            }
            if voice.is_hd_flash() {
                return Err(SpeechModelError::UnsupportedModelCapability);
            }
            match name {
                "prosody" => {
                    if stack.iter().any(|value| value == "prosody") {
                        return Err(invalid());
                    }
                    let rate_value = get("rate")
                        .map(|value| number(value, "%"))
                        .transpose()?
                        .unwrap_or(0);
                    let pitch_value = get("pitch")
                        .map(|value| number(value, "st"))
                        .transpose()?
                        .unwrap_or(0);
                    let volume = get("volume")
                        .map(|value| value.parse::<i16>().map_err(|_| invalid()))
                        .transpose()?
                        .unwrap_or(100);
                    if !(rate.0..=rate.1).contains(&rate_value)
                        || !(pitch.0..=pitch.1).contains(&pitch_value)
                    {
                        return Err(invalid());
                    }
                    SpeechOptions::new(
                        voice,
                        None,
                        rate_value,
                        i8::try_from(pitch_value).map_err(|_| invalid())?,
                        volume,
                    )?;
                }
                "mstts:express-as" => {
                    if stack.iter().any(|value| value == name) {
                        return Err(invalid());
                    }
                    let style = get("style").ok_or_else(invalid)?;
                    let degree = get("styledegree")
                        .map(|value| value.parse::<f64>().map_err(|_| invalid()))
                        .transpose()?;
                    SpeechOptions::new(voice, Some(style.into()), 0, 0, 100)?.with_expression(
                        voice,
                        degree,
                        get("role").map(str::to_owned),
                    )?;
                }
                "break" => {
                    let duration = number(get("time").ok_or_else(invalid)?, "ms")?;
                    if !(0..=5000).contains(&duration) {
                        return Err(invalid());
                    }
                }
                "phoneme" => {
                    if !matches!(voice.locale(), "en-GB" | "en-US") {
                        return Err(SpeechModelError::UnsupportedPhonemeLocale);
                    }
                    if !matches!(get("alphabet"), Some("ipa" | "ups"))
                        || get("ph").is_none_or(|value| {
                            value.trim().is_empty()
                                || value.len() > 1600
                                || value.chars().any(char::is_control)
                        })
                    {
                        return Err(invalid());
                    }
                    if get("alphabet") == Some("ups")
                        && get("ph").is_some_and(|value| !value.is_ascii())
                    {
                        return Err(invalid());
                    }
                }
                "say-as"
                    if !matches!(
                        get("interpret-as"),
                        Some(
                            "characters"
                                | "spell-out"
                                | "cardinal"
                                | "ordinal"
                                | "digits"
                                | "fraction"
                                | "telephone"
                        )
                    ) =>
                {
                    return Err(invalid());
                }
                "sub"
                    if get("alias").is_none_or(|value| {
                        value.trim().is_empty()
                            || value.chars().count() > 500
                            || value.chars().any(char::is_control)
                    }) =>
                {
                    return Err(invalid());
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn voice() -> Voice {
        Voice::new("azure", "en-GB-SoniaNeural", "en-GB", []).unwrap()
    }
    fn wrap(body: &str) -> String {
        format!(
            r#"<speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" xml:lang="en-GB"><voice name="en-GB-SoniaNeural">{body}</voice></speak>"#
        )
    }
    #[test]
    fn validates_exact_original_and_rejects_external_or_unknown_markup() {
        let raw = wrap(
            r#"<prosody rate="-27%" pitch="+0st">A &amp; B<break time="500ms"/><sub alias="kitten">cat</sub></prosody>"#,
        );
        let request = request(
            voice(),
            raw.clone(),
            AudioOutputFormat::default(),
            (-50, 100),
            (-12, 12),
        )
        .unwrap();
        assert_eq!(super::super::build_ssml(&request).unwrap(), raw);
        for body in [
            "cat\0",
            "&#0;",
            r#"<audio src="https://example.com/audio"/>"#,
            r#"<prosody rate="+900%">bad</prosody>"#,
            r#"<say-as interpret-as="unknown">bad</say-as>"#,
            r#"<phoneme alphabet="ipa" ph="kæt"><sub alias="kitten">cat</sub></phoneme>"#,
        ] {
            assert!(request_invalid(wrap(body)));
        }
        assert!(request_invalid(format!(
            "<!DOCTYPE speak [<!ENTITY x SYSTEM 'file:///tmp/x'>]>{}",
            wrap("&x;")
        )));
        assert!(request_invalid(
            wrap("cat").replace("SoniaNeural", "OtherNeural")
        ));
    }
    fn request_invalid(raw: String) -> bool {
        request(
            voice(),
            raw,
            AudioOutputFormat::default(),
            (-50, 100),
            (-12, 12),
        )
        .is_err()
    }
}
