use std::{
    env,
    ffi::OsString,
    fmt::Write as _,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use flate2::read::GzDecoder;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgConnection;

use tsz_rust::lexicon::normalization::normalize_headword;

const COPY_CHUNK_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq)]
struct Args {
    dataset_version: String,
    contents: PathBuf,
    source_locator: String,
    source_version: String,
    expected_records: Option<u64>,
    parser_version: String,
    replace_existing: bool,
    validate_only: bool,
}

fn usage() -> &'static str {
    "usage: import_dictionary_content --dataset-version VERSION --contents FILE \
     --source-locator URL --source-version VERSION [--expected-records N] [--parser-version VERSION] \
     [--replace-existing] [--validate-only]"
}

fn next_value(iter: &mut impl Iterator<Item = OsString>, flag: &str) -> anyhow::Result<OsString> {
    iter.next()
        .with_context(|| format!("{flag} requires a value"))
}

fn text_value(value: OsString, flag: &str) -> anyhow::Result<String> {
    value
        .into_string()
        .map_err(|_| anyhow::anyhow!("{flag} must be valid UTF-8"))
}

fn parse_args_from(values: impl IntoIterator<Item = OsString>) -> anyhow::Result<Args> {
    let mut iter = values.into_iter();
    let mut dataset_version = None;
    let mut contents = None;
    let mut source_locator = None;
    let mut source_version = None;
    let mut expected_records = None;
    let mut parser_version = "forms-sounds-v1".to_owned();
    let mut replace_existing = false;
    let mut validate_only = false;
    while let Some(flag) = iter.next() {
        let flag = text_value(flag, "argument name")?;
        match flag.as_str() {
            "--dataset-version" => {
                dataset_version = Some(text_value(next_value(&mut iter, &flag)?, &flag)?);
            }
            "--contents" => contents = Some(PathBuf::from(next_value(&mut iter, &flag)?)),
            "--source-locator" => {
                source_locator = Some(text_value(next_value(&mut iter, &flag)?, &flag)?);
            }
            "--source-version" => {
                source_version = Some(text_value(next_value(&mut iter, &flag)?, &flag)?);
            }
            "--expected-records" => {
                expected_records = Some(
                    text_value(next_value(&mut iter, &flag)?, &flag)?
                        .parse()
                        .with_context(|| format!("{flag} must be a non-negative integer"))?,
                );
            }
            "--parser-version" => {
                parser_version = text_value(next_value(&mut iter, &flag)?, &flag)?;
            }
            "--replace-existing" => replace_existing = true,
            "--validate-only" => validate_only = true,
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            unknown => bail!("unknown argument: {unknown}\n{}", usage()),
        }
    }
    Ok(Args {
        dataset_version: dataset_version.context("--dataset-version is required")?,
        contents: contents.context("--contents is required")?,
        source_locator: source_locator.context("--source-locator is required")?,
        source_version: source_version.context("--source-version is required")?,
        expected_records,
        parser_version,
        replace_existing,
        validate_only,
    })
}

fn open_jsonl(path: &Path) -> anyhow::Result<Box<dyn BufRead>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    if path.extension().is_some_and(|extension| extension == "gz") {
        Ok(Box::new(BufReader::new(GzDecoder::new(file))))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

fn sha256(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut encoded = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut encoded, "{byte:02x}").unwrap();
    }
    Ok(encoded)
}

fn append_csv_field(destination: &mut String, value: &str) {
    destination.push('"');
    for character in value.chars() {
        if character == '"' {
            destination.push('"');
        }
        destination.push(character);
    }
    destination.push('"');
}

fn parse_record(raw: &str, line_number: u64) -> anyhow::Result<(String, Value)> {
    let payload: Value = serde_json::from_str(raw)
        .with_context(|| format!("invalid JSONL record at line {line_number}"))?;
    ensure!(
        payload["lang_code"] == "en",
        "only English records are accepted"
    );
    let word = payload["word"]
        .as_str()
        .context("record word is required")?;
    ensure!(payload["pos"].is_string(), "record pos is required");
    ensure!(
        payload["senses"].is_array(),
        "record senses must be an array"
    );
    ensure!(
        payload.get("forms").is_none_or(Value::is_array),
        "record forms must be an array"
    );
    ensure!(
        payload.get("sounds").is_none_or(Value::is_array),
        "record sounds must be an array"
    );
    let normalized = normalize_headword(word)
        .with_context(|| {
            format!("cannot normalize dictionary word {word:?} at line {line_number}")
        })?
        .key;
    Ok((normalized, payload))
}

fn validate_contents(path: &Path) -> anyhow::Result<u64> {
    let mut source = open_jsonl(path)?;
    let mut line = String::new();
    let mut rows = 0_u64;
    loop {
        line.clear();
        if source.read_line(&mut line)? == 0 {
            break;
        }
        let raw = line.trim_end_matches(['\r', '\n']);
        if raw.is_empty() {
            continue;
        }
        parse_record(raw, rows + 1)?;
        rows += 1;
    }
    Ok(rows)
}

fn validate_replacement(
    existing: Option<(&str, &str)>,
    replace_existing: bool,
    input_sha256: &str,
    source_locator: &str,
) -> anyhow::Result<()> {
    let Some((existing_sha256, existing_locator)) = existing else {
        return Ok(());
    };
    ensure!(
        replace_existing,
        "content is already imported for this dataset"
    );
    ensure!(
        existing_sha256 == input_sha256 && existing_locator == source_locator,
        "replacement content must keep the existing input SHA-256 and source locator"
    );
    Ok(())
}

async fn copy_contents(connection: &mut PgConnection, path: &Path) -> anyhow::Result<u64> {
    let mut copy = connection
        .copy_in_raw(
            "COPY dictionary_import_contents (normalized_term, payload) FROM STDIN WITH (FORMAT csv)",
        )
        .await?;
    let result: anyhow::Result<u64> = async {
        let mut source = open_jsonl(path)?;
        let mut line = String::new();
        let mut chunk = String::with_capacity(COPY_CHUNK_BYTES + 4096);
        let mut rows = 0_u64;
        loop {
            line.clear();
            if source.read_line(&mut line)? == 0 {
                break;
            }
            let raw = line.trim_end_matches(['\r', '\n']);
            if raw.is_empty() {
                continue;
            }
            let (normalized, _) = parse_record(raw, rows + 1)?;
            append_csv_field(&mut chunk, &normalized);
            chunk.push(',');
            append_csv_field(&mut chunk, raw);
            chunk.push('\n');
            rows += 1;
            if chunk.len() >= COPY_CHUNK_BYTES {
                copy.send(chunk.as_bytes()).await?;
                chunk.clear();
            }
            if rows.is_multiple_of(100_000) {
                println!("{}: {rows} rows streamed", path.display());
            }
        }
        if !chunk.is_empty() {
            copy.send(chunk.as_bytes()).await?;
        }
        Ok(rows)
    }
    .await;
    match result {
        Ok(rows) => {
            let copied = copy.finish().await?;
            ensure!(copied == rows, "COPY row count mismatch");
            Ok(rows)
        }
        Err(error) => {
            let _ = copy.abort("dictionary content import input failed").await;
            Err(error)
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    dotenvy::from_filename(".env").ok();
    let args = parse_args_from(env::args_os().skip(1))?;
    ensure!(args.contents.is_file(), "contents file not found");
    ensure!(
        !args.source_locator.trim().is_empty(),
        "source locator is empty"
    );
    ensure!(
        !args.parser_version.trim().is_empty(),
        "parser version is empty"
    );
    ensure!(
        !args.source_version.trim().is_empty(),
        "source version is empty"
    );
    let input_sha256 = sha256(&args.contents)?;
    if args.validate_only {
        let records = validate_contents(&args.contents)?;
        if let Some(expected) = args.expected_records {
            ensure!(
                records == expected,
                "record count mismatch: expected {expected}, got {records}"
            );
        }
        println!("Content validation complete: records={records} sha256={input_sha256}");
        return Ok(());
    }
    let database_url = env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pool = tsz_rust::platform::connect_db(&database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    let mut tx = pool.begin().await?;
    let dataset_id: i64 = sqlx::query_scalar(
        "SELECT id FROM dictionary.datasets WHERE version = $1 AND status = 'active'",
    )
    .bind(&args.dataset_version)
    .fetch_optional(&mut *tx)
    .await?
    .with_context(|| format!("active dataset not found: {}", args.dataset_version))?;
    let existing = sqlx::query_as::<_, (String, String)>(
        "SELECT input_sha256, source_locator FROM dictionary.content_imports WHERE dataset_id=$1",
    )
    .bind(dataset_id)
    .fetch_optional(&mut *tx)
    .await?;
    validate_replacement(
        existing
            .as_ref()
            .map(|(sha256, locator)| (sha256.as_str(), locator.as_str())),
        args.replace_existing,
        &input_sha256,
        &args.source_locator,
    )?;
    if existing.is_some() {
        sqlx::query("DELETE FROM dictionary.entry_contents WHERE dataset_id=$1")
            .bind(dataset_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "CREATE TEMP TABLE dictionary_import_contents (normalized_term TEXT NOT NULL, payload JSONB NOT NULL) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await?;
    let staged = copy_contents(tx.as_mut(), &args.contents).await?;
    if let Some(expected) = args.expected_records {
        ensure!(
            staged == expected,
            "record count mismatch: expected {expected}, got {staged}"
        );
    }
    let inserted = sqlx::query(
        r#"INSERT INTO dictionary.entry_contents (
               dataset_id, source_key, normalized_term, pos, senses, forms, sounds, source_locator
           )
           SELECT $1,
                  concat('kaikki:', normalized_term, ':', payload->>'pos', ':', md5(payload::text)),
                  normalized_term, payload->>'pos', payload->'senses',
                  coalesce(payload->'forms', '[]'::jsonb),
                  coalesce(payload->'sounds', '[]'::jsonb), $2
           FROM dictionary_import_contents"#,
    )
    .bind(dataset_id)
    .bind(&args.source_locator)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    ensure!(
        inserted == staged,
        "not all staged content records were inserted"
    );
    sqlx::query(
        r#"INSERT INTO dictionary.content_imports
           (dataset_id, input_sha256, source_locator, source_version, record_count, parser_version)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT (dataset_id) DO UPDATE SET
             record_count = EXCLUDED.record_count,
             parser_version = EXCLUDED.parser_version,
             imported_at = now()"#,
    )
    .bind(dataset_id)
    .bind(&input_sha256)
    .bind(&args.source_locator)
    .bind(&args.source_version)
    .bind(i64::try_from(staged).context("record count exceeds BIGINT")?)
    .bind(&args.parser_version)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    sqlx::query("ANALYZE dictionary.entry_contents")
        .execute(&pool)
        .await?;
    println!(
        "Content import complete: dataset={} records={} sha256={}",
        args.dataset_version, inserted, input_sha256
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_required_arguments() {
        let args = parse_args_from(
            [
                "--dataset-version",
                "v1",
                "--contents",
                "english.jsonl.gz",
                "--source-locator",
                "https://kaikki.org/source",
                "--source-version",
                "enwiktionary-test",
                "--expected-records",
                "12",
                "--validate-only",
            ]
            .map(OsString::from),
        )
        .unwrap();
        assert_eq!(args.dataset_version, "v1");
        assert_eq!(args.expected_records, Some(12));
        assert_eq!(args.source_version, "enwiktionary-test");
        assert_eq!(args.parser_version, "forms-sounds-v1");
        assert!(!args.replace_existing);
        assert!(args.validate_only);
    }

    #[test]
    fn csv_field_escapes_json_quotes() {
        let mut value = String::new();
        append_csv_field(&mut value, r#"{"word":"bank"}"#);
        assert_eq!(value, r#""{""word"":""bank""}""#);
    }

    #[test]
    fn rejects_non_array_forms_and_sounds() {
        assert!(
            parse_record(
                r#"{"lang_code":"en","word":"child","pos":"noun","senses":[],"forms":{}}"#,
                1
            )
            .is_err()
        );
        assert!(
            parse_record(
                r#"{"lang_code":"en","word":"child","pos":"noun","senses":[],"sounds":"ipa"}"#,
                1
            )
            .is_err()
        );
    }

    #[test]
    fn replacement_requires_explicit_flag_and_identical_source_identity() {
        let existing = Some(("same-sha", "https://kaikki.org/source"));
        assert!(
            validate_replacement(existing, false, "same-sha", "https://kaikki.org/source").is_err()
        );
        assert!(
            validate_replacement(existing, true, "different", "https://kaikki.org/source").is_err()
        );
        assert!(
            validate_replacement(existing, true, "same-sha", "https://other.example/source")
                .is_err()
        );
        validate_replacement(existing, true, "same-sha", "https://kaikki.org/source").unwrap();
        validate_replacement(None, false, "new-sha", "https://kaikki.org/source").unwrap();
    }
}
