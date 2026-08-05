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
use sha2::{Digest, Sha256};
use sqlx::postgres::PgConnection;

const COPY_CHUNK_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq)]
struct Args {
    version: String,
    source_version: String,
    rules_version: String,
    terms: PathBuf,
    regions: PathBuf,
    expected_terms: Option<u64>,
    expected_regions: Option<u64>,
    expected_evidence: Option<u64>,
}

fn usage() -> &'static str {
    "usage: import_dictionary --version VERSION --terms FILE --regions FILE \
     [--source-version VERSION] [--rules-version VERSION] \
     [--expected-terms N] [--expected-regions N] [--expected-evidence N]"
}

fn next_value(iter: &mut impl Iterator<Item = OsString>, flag: &str) -> anyhow::Result<OsString> {
    iter.next()
        .with_context(|| format!("{flag} requires a value"))
}

fn parse_count(value: OsString, flag: &str) -> anyhow::Result<u64> {
    value
        .into_string()
        .map_err(|_| anyhow::anyhow!("{flag} must be valid UTF-8"))?
        .parse()
        .with_context(|| format!("{flag} must be a non-negative integer"))
}

fn parse_args_from(values: impl IntoIterator<Item = OsString>) -> anyhow::Result<Args> {
    let mut iter = values.into_iter();
    let mut version = None;
    let mut source_version = "enwiktionary-2026-07-06".to_owned();
    let mut rules_version = "region-family-v1-rare-uncommon-filter".to_owned();
    let mut terms = None;
    let mut regions = None;
    let mut expected_terms = None;
    let mut expected_regions = None;
    let mut expected_evidence = None;

    while let Some(flag) = iter.next() {
        let flag = flag
            .into_string()
            .map_err(|_| anyhow::anyhow!("argument name must be valid UTF-8"))?;
        match flag.as_str() {
            "--version" => {
                version = Some(
                    next_value(&mut iter, &flag)?
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("--version must be valid UTF-8"))?,
                );
            }
            "--source-version" => {
                source_version = next_value(&mut iter, &flag)?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("--source-version must be valid UTF-8"))?;
            }
            "--rules-version" => {
                rules_version = next_value(&mut iter, &flag)?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("--rules-version must be valid UTF-8"))?;
            }
            "--terms" => terms = Some(PathBuf::from(next_value(&mut iter, &flag)?)),
            "--regions" => regions = Some(PathBuf::from(next_value(&mut iter, &flag)?)),
            "--expected-terms" => {
                expected_terms = Some(parse_count(next_value(&mut iter, &flag)?, &flag)?);
            }
            "--expected-regions" => {
                expected_regions = Some(parse_count(next_value(&mut iter, &flag)?, &flag)?);
            }
            "--expected-evidence" => {
                expected_evidence = Some(parse_count(next_value(&mut iter, &flag)?, &flag)?);
            }
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            unknown => bail!("unknown argument: {unknown}\n{}", usage()),
        }
    }

    Ok(Args {
        version: version.context("--version is required")?,
        source_version,
        rules_version,
        terms: terms.context("--terms is required")?,
        regions: regions.context("--regions is required")?,
        expected_terms,
        expected_regions,
        expected_evidence,
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
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing into String cannot fail");
    }
    Ok(encoded)
}

fn append_csv_json_line(destination: &mut String, json: &str) {
    destination.push('"');
    for character in json.chars() {
        if character == '"' {
            destination.push('"');
        }
        destination.push(character);
    }
    destination.push_str("\"\n");
}

async fn copy_jsonl(
    connection: &mut PgConnection,
    path: &Path,
    table: &str,
) -> anyhow::Result<u64> {
    let statement = format!("COPY {table} (payload) FROM STDIN WITH (FORMAT csv)");
    let mut copy = connection.copy_in_raw(&statement).await?;
    let feed_result: anyhow::Result<u64> = async {
        let mut source = open_jsonl(path)?;
        let mut line = String::new();
        let mut chunk = String::with_capacity(COPY_CHUNK_BYTES + 4096);
        let mut rows = 0_u64;

        loop {
            line.clear();
            if source.read_line(&mut line)? == 0 {
                break;
            }
            let json = line.trim_end_matches(['\r', '\n']);
            if json.is_empty() {
                continue;
            }
            append_csv_json_line(&mut chunk, json);
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

    match feed_result {
        Ok(rows) => {
            let copied = copy.finish().await?;
            ensure!(
                copied == rows,
                "COPY row count mismatch for {}: read {rows}, copied {copied}",
                path.display()
            );
            Ok(copied)
        }
        Err(error) => {
            let _ = copy.abort("dictionary import input failed").await;
            Err(error)
        }
    }
}

fn validate_expected(name: &str, actual: u64, expected: Option<u64>) -> anyhow::Result<()> {
    if let Some(expected) = expected {
        ensure!(
            actual == expected,
            "{name} count mismatch: expected {expected}, got {actual}"
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    dotenvy::from_filename(".env").ok();
    let args = parse_args_from(env::args_os().skip(1))?;

    ensure!(
        args.terms.is_file(),
        "terms file not found: {}",
        args.terms.display()
    );
    ensure!(
        args.regions.is_file(),
        "regions file not found: {}",
        args.regions.display()
    );

    println!("Calculating input checksums...");
    let terms_sha256 = sha256(&args.terms)?;
    let regions_sha256 = sha256(&args.regions)?;

    let database_url = env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pool = tsz_rust::platform::connect_db(&database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    let mut tx = pool.begin().await?;
    let dataset_id: i64 = sqlx::query_scalar(
        r#"
        INSERT INTO dictionary.datasets (
            version, source_name, source_version, rules_version,
            terms_sha256, regions_sha256, status
        )
        VALUES ($1, 'Kaikki English Wiktionary', $2, $3, $4, $5, 'importing')
        RETURNING id
        "#,
    )
    .bind(&args.version)
    .bind(&args.source_version)
    .bind(&args.rules_version)
    .bind(&terms_sha256)
    .bind(&regions_sha256)
    .fetch_one(&mut *tx)
    .await
    .with_context(|| format!("failed to create dictionary dataset {}", args.version))?;

    sqlx::query(
        "CREATE TEMP TABLE dictionary_import_terms (payload JSONB NOT NULL) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "CREATE TEMP TABLE dictionary_import_regions (payload JSONB NOT NULL) ON COMMIT DROP",
    )
    .execute(&mut *tx)
    .await?;

    println!("Streaming terms into PostgreSQL staging...");
    let staged_terms = copy_jsonl(tx.as_mut(), &args.terms, "dictionary_import_terms").await?;
    validate_expected("terms", staged_terms, args.expected_terms)?;

    println!("Expanding terms into dictionary.terms...");
    let inserted_terms = sqlx::query(
        r#"
        INSERT INTO dictionary.terms (
            dataset_id, normalized_term, term, kind, pos, status, warning_tags,
            sense_count, filtered_cold_sense_count, region_family,
            source_regions, region_evidence_types
        )
        SELECT
            $1,
            payload->>'normalized_term',
            payload->>'term',
            payload->>'kind',
            ARRAY(SELECT jsonb_array_elements_text(payload->'pos')),
            payload->>'status',
            ARRAY(SELECT jsonb_array_elements_text(payload->'warning_tags')),
            (payload->>'sense_count')::INTEGER,
            (payload->>'filtered_cold_sense_count')::INTEGER,
            payload->>'region_family',
            ARRAY(SELECT jsonb_array_elements_text(payload->'source_regions')),
            ARRAY(SELECT jsonb_array_elements_text(payload->'region_evidence_types'))
        FROM dictionary_import_terms
        "#,
    )
    .bind(dataset_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    ensure!(
        inserted_terms == staged_terms,
        "not all staged terms were inserted"
    );

    println!("Streaming regional evidence into PostgreSQL staging...");
    let staged_regions =
        copy_jsonl(tx.as_mut(), &args.regions, "dictionary_import_regions").await?;
    validate_expected("regional surfaces", staged_regions, args.expected_regions)?;

    println!("Expanding regional surfaces...");
    let inserted_regions = sqlx::query(
        r#"
        INSERT INTO dictionary.region_surfaces (
            dataset_id, normalized_term, term, region_family, families,
            source_regions, evidence_types, pos, targets, is_headword
        )
        SELECT
            $1,
            payload->>'normalized_term',
            payload->>'term',
            payload->>'region_family',
            ARRAY(SELECT jsonb_array_elements_text(payload->'families')),
            ARRAY(SELECT jsonb_array_elements_text(payload->'source_regions')),
            ARRAY(SELECT jsonb_array_elements_text(payload->'evidence_types')),
            ARRAY(SELECT jsonb_array_elements_text(payload->'pos')),
            ARRAY(SELECT jsonb_array_elements_text(payload->'targets')),
            (payload->>'is_headword')::BOOLEAN
        FROM dictionary_import_regions
        "#,
    )
    .bind(dataset_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    ensure!(
        inserted_regions == staged_regions,
        "not all staged regional surfaces were inserted"
    );

    println!("Expanding original region evidence details...");
    let inserted_evidence = sqlx::query(
        r#"
        INSERT INTO dictionary.region_evidence (
            dataset_id, normalized_term, evidence_type,
            original_region_tags, raw_tags, pos, targets
        )
        SELECT
            $1,
            source.payload->>'normalized_term',
            detail.value->>'type',
            ARRAY(SELECT jsonb_array_elements_text(detail.value->'original_region_tags')),
            ARRAY(SELECT jsonb_array_elements_text(detail.value->'raw_tags')),
            detail.value->>'pos',
            ARRAY(SELECT jsonb_array_elements_text(detail.value->'targets'))
        FROM dictionary_import_regions AS source
        CROSS JOIN LATERAL jsonb_array_elements(source.payload->'evidence') AS detail(value)
        "#,
    )
    .bind(dataset_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    validate_expected(
        "evidence details",
        inserted_evidence,
        args.expected_evidence,
    )?;

    sqlx::query("UPDATE dictionary.datasets SET status = 'retired' WHERE status = 'active'")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        r#"
        UPDATE dictionary.datasets
        SET status = 'active',
            term_count = $2,
            regional_surface_count = $3,
            evidence_count = $4,
            activated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(dataset_id)
    .bind(i32::try_from(inserted_terms).context("term count exceeds INTEGER")?)
    .bind(i32::try_from(inserted_regions).context("region count exceeds INTEGER")?)
    .bind(i32::try_from(inserted_evidence).context("evidence count exceeds INTEGER")?)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    for (table, statement) in [
        ("dictionary.terms", "ANALYZE dictionary.terms"),
        (
            "dictionary.region_surfaces",
            "ANALYZE dictionary.region_surfaces",
        ),
        (
            "dictionary.region_evidence",
            "ANALYZE dictionary.region_evidence",
        ),
    ] {
        if let Err(error) = sqlx::query(statement).execute(&pool).await {
            tracing::warn!(%error, table, "dictionary import succeeded but ANALYZE failed");
        }
    }

    println!(
        "Import complete: dataset={} terms={} regional_surfaces={} evidence_details={}",
        args.version, inserted_terms, inserted_regions, inserted_evidence
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_encoding_preserves_json_quotes() {
        let mut output = String::new();
        append_csv_json_line(&mut output, r#"{"word":"colour","tags":["UK"]}"#);
        assert_eq!(
            output,
            "\"{\"\"word\"\":\"\"colour\"\",\"\"tags\"\":[\"\"UK\"\"]}\"\n"
        );
    }

    #[test]
    fn parses_required_and_optional_arguments() {
        let args = parse_args_from(
            [
                "--version",
                "v1",
                "--terms",
                "terms.jsonl.gz",
                "--regions",
                "regions.jsonl.gz",
                "--expected-terms",
                "12",
            ]
            .map(OsString::from),
        )
        .expect("valid arguments should parse");

        assert_eq!(args.version, "v1");
        assert_eq!(args.terms, PathBuf::from("terms.jsonl.gz"));
        assert_eq!(args.expected_terms, Some(12));
    }
}
