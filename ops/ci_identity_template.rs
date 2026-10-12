//! CI-only helper, registered as a binary only in the disposable identity workspace.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Connection, Executor, PgConnection, SqlSafeStr, migrate::Migrator};
use std::{collections::BTreeMap, path::Path, str::FromStr, time::Instant};

static MIGRATOR: Migrator = sqlx::migrate!();

// BEGIN MIGRATION FINGERPRINT
fn migration_fingerprint(migrator: &sqlx::migrate::Migrator) -> String {
    use sha2::{Digest, Sha256};
    assert!(!migrator.ignore_missing && migrator.locking && !migrator.no_tx);
    assert_eq!(migrator.table_name, "_sqlx_migrations");
    assert!(migrator.create_schemas.is_empty());
    let mut hash = Sha256::new();
    for migration in migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
    {
        hash.update(migration.version.to_be_bytes());
        hash.update(migration.checksum.as_ref());
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
// END MIGRATION FINGERPRINT

fn sql(text: String) -> impl SqlSafeStr {
    sqlx::AssertSqlSafe(text)
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

async fn connect(name: &str) -> Result<PgConnection> {
    let options = sqlx::postgres::PgConnectOptions::from_str(&std::env::var("DATABASE_URL")?)?;
    ensure!(
        matches!(options.get_host(), "localhost" | "127.0.0.1" | "::1"),
        "template pilot requires a local isolated PostgreSQL service"
    );
    Ok(PgConnection::connect_with(&options.database(name)).await?)
}

async fn create(master: &mut PgConnection, name: &str, template: Option<&str>) -> Result<()> {
    let suffix = template
        .map(|t| format!(" TEMPLATE {} STRATEGY WAL_LOG", quote(t)))
        .unwrap_or_default();
    master
        .execute(sql(format!("CREATE DATABASE {}{suffix}", quote(name))))
        .await?;
    Ok(())
}

// OID-independent catalog definitions, including privileges and database settings.
// Keep complete initial rows; only known migration-clock/cluster-txid fields differ
// between two genuinely independent full migration runs.
async fn snapshot(name: &str) -> Result<Value> {
    let mut conn = connect(name).await?;
    let definitions: Vec<String> = sqlx::query_scalar(r#"
        WITH namespaces AS (SELECT oid FROM pg_namespace
          WHERE nspname NOT LIKE 'pg_%' AND nspname <> 'information_schema'),
        objects AS (
          SELECT 'schema' AS kind, nspname AS name, jsonb_build_array(pg_get_userbyid(nspowner), nspacl)::text AS definition
            FROM pg_namespace WHERE oid IN (SELECT oid FROM namespaces)
          UNION ALL SELECT 'relation', c.oid::regclass::text,
            jsonb_build_array(c.relkind, pg_get_userbyid(c.relowner), c.relacl, c.reloptions,
              c.relrowsecurity, c.relforcerowsecurity,
              CASE WHEN c.relkind IN ('v','m') THEN pg_get_viewdef(c.oid, true) END)::text
            FROM pg_class c WHERE c.relnamespace IN (SELECT oid FROM namespaces)
          UNION ALL SELECT 'column', a.attrelid::regclass::text || '.' || a.attname,
            jsonb_build_array(a.attnum, format_type(a.atttypid,a.atttypmod), a.attnotnull,
              a.attidentity, a.attgenerated, a.attcollation::regcollation::text,
              pg_get_expr(d.adbin,d.adrelid), a.attacl)::text
            FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid
            LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
            WHERE c.relnamespace IN (SELECT oid FROM namespaces) AND a.attnum>0 AND NOT a.attisdropped
          UNION ALL SELECT 'constraint', conrelid::regclass::text || '.' || conname,
            pg_get_constraintdef(oid, true) FROM pg_constraint WHERE connamespace IN (SELECT oid FROM namespaces)
          UNION ALL SELECT 'index', indexrelid::regclass::text, pg_get_indexdef(indexrelid)
            FROM pg_index JOIN pg_class c ON c.oid=indexrelid WHERE c.relnamespace IN (SELECT oid FROM namespaces)
          UNION ALL SELECT 'trigger', tgrelid::regclass::text || '.' || tgname, pg_get_triggerdef(t.oid,true)
            FROM pg_trigger t JOIN pg_class c ON c.oid=tgrelid WHERE NOT tgisinternal AND c.relnamespace IN (SELECT oid FROM namespaces)
          UNION ALL SELECT 'function', p.oid::regprocedure::text,
            jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl,p.prosecdef,p.proconfig)::text
            FROM pg_proc p WHERE p.pronamespace IN (SELECT oid FROM namespaces) AND p.prokind IN ('f','p')
          UNION ALL SELECT 'type', t.oid::regtype::text,
            jsonb_build_array(t.typtype,t.typbasetype::regtype::text,t.typnotnull,t.typdefault,
              (SELECT jsonb_agg(enumlabel ORDER BY enumsortorder) FROM pg_enum WHERE enumtypid=t.oid))::text
            FROM pg_type t WHERE t.typnamespace IN (SELECT oid FROM namespaces)
          UNION ALL SELECT 'sequence', schemaname || '.' || sequencename,
            jsonb_build_array(data_type,start_value,min_value,max_value,increment_by,cycle,cache_size)::text FROM pg_sequences
            WHERE schemaname NOT LIKE 'pg_%' AND schemaname <> 'information_schema'
          UNION ALL SELECT 'extension', extname, jsonb_build_array(extversion,extnamespace::regnamespace::text,pg_get_userbyid(extowner))::text FROM pg_extension
          UNION ALL SELECT 'database', '', jsonb_build_array(pg_get_userbyid(datdba),encoding,datcollate,datctype,datlocprovider,daticulocale,datacl)::text
            FROM pg_database WHERE datname=current_database()
          UNION ALL SELECT 'db-setting', CASE WHEN setrole=0 THEN '' ELSE pg_get_userbyid(setrole) END, to_jsonb(setconfig)::text
            FROM pg_db_role_setting WHERE setdatabase=(SELECT oid FROM pg_database WHERE datname=current_database())
        ) SELECT jsonb_build_array(kind,name,definition)::text FROM objects ORDER BY kind,name,definition
    "#).fetch_all(&mut conn).await?;
    let relations: Vec<(String, String, String)> = sqlx::query_as("SELECT n.nspname,c.relname,c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT LIKE 'pg_%' AND n.nspname <> 'information_schema' AND c.relkind IN ('r','p','m','S') ORDER BY 1,2")
        .fetch_all(&mut conn).await?;
    let mut data = BTreeMap::new();
    for (schema, table, kind) in relations {
        let qualified = format!("{}.{}", quote(&schema), quote(&table));
        let query = if kind == "S" {
            format!(
                "SELECT jsonb_build_object('last_value',last_value,'is_called',is_called) FROM {qualified}"
            )
        } else {
            format!("SELECT to_jsonb(t) FROM {qualified} t")
        };
        let mut rows: Vec<Value> = sqlx::query_scalar(sql(query)).fetch_all(&mut conn).await?;
        for row in &mut rows {
            let fields: &[&str] = match (schema.as_str(), table.as_str()) {
                ("public", "_sqlx_migrations") => &["installed_on", "execution_time"],
                ("catalog", "metadata") => &["updated_at"],
                ("catalog", "parts_of_speech" | "sub_parts_of_speech" | "form_types") => {
                    &["created_at", "updated_at"]
                }
                ("lexicon", "sentence_discovery_generation") => &["last_txid"],
                _ => &[],
            };
            for field in fields {
                ensure!(
                    row.as_object_mut().unwrap().remove(*field).is_some(),
                    "normalization field disappeared: {schema}.{table}.{field}"
                );
            }
        }
        rows.sort_by_key(Value::to_string);
        data.insert(format!("{schema}.{table}:{kind}"), rows);
    }
    conn.close().await?;
    Ok(json!({"definitions": definitions, "data": data}))
}

async fn verify(
    master: &mut PgConnection,
    template: &str,
    owned: &mut Vec<String>,
) -> Result<Value> {
    let started = Instant::now();
    let control = format!("{template}_fresh");
    create(master, &control, None).await?;
    owned.push(control.clone());
    let mut conn = connect(&control).await?;
    MIGRATOR.run(&mut conn).await?;
    conn.close().await?;
    let baseline = snapshot(&control).await?;
    let mut probes = Vec::new();
    for suffix in ["a", "b", "c"] {
        let name = format!("{template}_{suffix}");
        create(master, &name, Some(template)).await?;
        owned.push(name.clone());
        ensure!(
            snapshot(&name).await? == baseline,
            "fresh migration and {suffix} clone state differ"
        );
        probes.push(name);
    }
    let mut a = connect(&probes[0]).await?;
    // Commit data, DDL and an actual application sequence change in A.
    a.execute("CREATE TABLE public.ci_isolation_probe (id bigint PRIMARY KEY); INSERT INTO public.ci_isolation_probe VALUES (1); UPDATE catalog.metadata SET version=version+1; SELECT nextval('lexicon.surface_projection_event_offset_seq')").await?;
    ensure!(snapshot(&probes[1]).await? == baseline, "A mutated B");
    // Delete A while B is connected; SQLx cleanup must never affect another case.
    a.close().await?;
    let b = connect(&probes[1]).await?;
    master
        .execute(sql(format!("DROP DATABASE {}", quote(&probes[0]))))
        .await?;
    owned.retain(|name| name != &probes[0]);
    b.close().await?;
    ensure!(
        snapshot(&probes[1]).await? == baseline,
        "cleanup of A affected B"
    );
    // C predates mutation; additionally create D afterwards to prove template unchanged.
    let d = format!("{template}_d");
    create(master, &d, Some(template)).await?;
    owned.push(d.clone());
    ensure!(
        snapshot(&d).await? == baseline,
        "case mutations reached the template"
    );
    let mut dirty = connect(&probes[2]).await?;
    dirty.execute("UPDATE _sqlx_migrations SET success=false WHERE version=(SELECT max(version) FROM _sqlx_migrations)").await?;
    ensure!(
        matches!(
            MIGRATOR.run(&mut dirty).await,
            Err(sqlx::migrate::MigrateError::Dirty(_))
        ),
        "dirty ledger accepted"
    );
    dirty.close().await?;
    let mut checksum = connect(&d).await?;
    checksum.execute("UPDATE _sqlx_migrations SET checksum=decode('00','hex') WHERE version=(SELECT max(version) FROM _sqlx_migrations)").await?;
    ensure!(
        matches!(
            MIGRATOR.run(&mut checksum).await,
            Err(sqlx::migrate::MigrateError::VersionMismatch(_))
        ),
        "invalid checksum accepted"
    );
    checksum.close().await?;
    Ok(
        json!({"fresh_clone_equivalent": true, "data_schema_sequence_isolated": true, "cleanup_isolated": true, "template_unchanged": true, "dirty_rejected": true, "checksum_rejected": true, "state_sha256": Sha256::digest(baseline.to_string()).iter().map(|b| format!("{b:02x}")).collect::<String>(), "verification_ms": started.elapsed().as_millis()}),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    ensure!(
        args.len() == 4,
        "expected prepare|cleanup template-name report-path"
    );
    let (action, template, report) = (&args[1], &args[2], Path::new(&args[3]));
    let suffix = template.strip_prefix("_tsz_ci_tpl_").unwrap_or_default();
    ensure!(
        suffix.len() == 32 && suffix.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid template name"
    );
    let options = sqlx::postgres::PgConnectOptions::from_str(&std::env::var("DATABASE_URL")?)?;
    let mut master = connect(options.get_database().unwrap_or("postgres")).await?;
    if action == "cleanup" {
        ensure!(report.exists(), "missing ownership report");
        let evidence: Value = serde_json::from_slice(&std::fs::read(report)?)?;
        ensure!(
            evidence["template"] == *template,
            "template ownership mismatch"
        );
        master
            .execute(sql(format!("DROP DATABASE {}", quote(template))))
            .await?;
        return Ok(());
    }
    ensure!(
        action == "prepare" && !report.exists(),
        "invalid action or existing report"
    );
    let started = Instant::now();
    let mut owned = Vec::new();
    let result: Result<Value> = async {
        create(&mut master, template, None).await?;
        owned.push(template.clone());
        let mut source = connect(template).await?;
        MIGRATOR.run(&mut source).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success").fetch_one(&mut source).await?;
        ensure!(count as usize==MIGRATOR.iter().filter(|m| !m.migration_type.is_down_migration()).count(), "incomplete migration ledger");
        source.close().await?;
        master.execute(sql(format!("ALTER DATABASE {} ALLOW_CONNECTIONS false",quote(template)))).await?;
        let active: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=$1").bind(template).fetch_one(&mut master).await?;
        ensure!(active==0,"template connections remain open");
        let bootstrap_ms = started.elapsed().as_millis();
        let checks = verify(&mut master, template, &mut owned).await?;
        Ok(json!({"template":template,"migration_fingerprint":migration_fingerprint(&MIGRATOR),"migration_count":count,"bootstrap_ms":bootstrap_ms,"checks":checks}))
    }.await;
    // Drop only databases that this invocation successfully created. Never use
    // DROP IF EXISTS on caller-supplied names or terminate other sessions.
    for name in owned
        .iter()
        .rev()
        .filter(|name| result.is_err() || *name != template)
    {
        master
            .execute(sql(format!("DROP DATABASE {}", quote(name))))
            .await?;
    }
    let evidence = result?;
    std::fs::write(report, serde_json::to_vec_pretty(&evidence)?)?;
    println!("CI_TEMPLATE_CHECKS {}", evidence);
    Ok(())
}
