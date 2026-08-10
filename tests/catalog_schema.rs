//! catalog schema 的数据库约束与默认种子契约测试。
//!
//! 用 `#[sqlx::test]` 为每个测试创建独立临时库并自动执行 `migrations/`。
//! 断言失败时会核对 PostgreSQL 错误码和固定索引名，避免把“表不存在”等错误误判为约束生效。

use sqlx::PgPool;
use uuid::Uuid;

const UNIQUE_VIOLATION: &str = "23505";
const FOREIGN_KEY_VIOLATION: &str = "23503";
const CHECK_VIOLATION: &str = "23514";

fn assert_db_error<T: std::fmt::Debug>(
    result: Result<T, sqlx::Error>,
    expected_code: &str,
    expected_constraint: Option<&str>,
    message: &str,
) {
    match result {
        Err(sqlx::Error::Database(db)) => {
            assert_eq!(
                db.code().as_deref(),
                Some(expected_code),
                "{message}：PostgreSQL 错误码不符，实际为 {:?}（{}）",
                db.code(),
                db.message()
            );
            if let Some(expected_constraint) = expected_constraint {
                assert_eq!(
                    db.constraint(),
                    Some(expected_constraint),
                    "{message}：命中的约束/索引名不符"
                );
            }
        }
        other => panic!("{message}：应返回数据库约束错误，实际为 {other:?}"),
    }
}

fn short_id(id: Uuid) -> String {
    let value = id.simple().to_string();
    value[value.len() - 8..].to_owned()
}

async fn part_id(pool: &PgPool, code: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM catalog.parts_of_speech WHERE code = $1")
        .bind(code)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|error| panic!("应能查到基本词性 {code}：{error}"))
}

async fn insert_part_values(
    pool: &PgPool,
    code: &str,
    name_zh: &str,
    name_en: &str,
    abbreviation: &str,
    sort_order: i32,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO catalog.parts_of_speech (
            id, code, name_zh, name_en, abbreviation, sort_order
        )
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(id)
    .bind(code)
    .bind(name_zh)
    .bind(name_en)
    .bind(abbreviation)
    .bind(sort_order)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn insert_part(pool: &PgPool, code: &str) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    let suffix = short_id(id);
    sqlx::query(
        r#"
        INSERT INTO catalog.parts_of_speech (
            id, code, name_zh, name_en, abbreviation
        )
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(id)
    .bind(code)
    .bind(format!("测试{suffix}"))
    .bind(format!("Test {suffix}"))
    .bind(format!("t{suffix}"))
    .execute(pool)
    .await?;
    Ok(id)
}

async fn insert_sub_values(
    pool: &PgPool,
    parent_id: Uuid,
    code: &str,
    name_zh: &str,
    name_en: &str,
    sort_order: i32,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO catalog.sub_parts_of_speech (
            id, part_of_speech_id, code, name_zh, name_en, sort_order
        )
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(id)
    .bind(parent_id)
    .bind(code)
    .bind(name_zh)
    .bind(name_en)
    .bind(sort_order)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn insert_sub(pool: &PgPool, parent_id: Uuid, code: &str) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    let suffix = short_id(id);
    sqlx::query(
        r#"
        INSERT INTO catalog.sub_parts_of_speech (
            id, part_of_speech_id, code, name_zh, name_en
        )
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(id)
    .bind(parent_id)
    .bind(code)
    .bind(format!("细分{suffix}"))
    .bind(format!("Sub {suffix}"))
    .execute(pool)
    .await?;
    Ok(id)
}

async fn insert_admin(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO admins (id, phone, password_hash, display_name) VALUES ($1, $2, 'hash', '审计管理员')",
    )
    .bind(id)
    .bind(format!("catalog-{}", id.simple()))
    .execute(pool)
    .await
    .expect("插入审计管理员应成功");
    id
}

// ===== schema、metadata 与默认种子 =====

#[sqlx::test]
async fn catalog_schema_and_metadata_seed_are_present(pool: PgPool) {
    let schema_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = 'catalog')",
    )
    .fetch_one(&pool)
    .await
    .expect("查询 catalog schema 应成功");
    assert!(schema_exists, "migration 应创建 catalog schema");

    let rows: Vec<(bool, i64, bool)> = sqlx::query_as(
        "SELECT id, version, updated_at IS NOT NULL FROM catalog.metadata ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("查询 catalog.metadata 应成功");
    assert_eq!(
        rows,
        vec![(true, 1, true)],
        "metadata 应初始化为唯一一行 version=1"
    );
}

#[sqlx::test]
async fn metadata_rejects_false_id_and_nonpositive_version(pool: PgPool) {
    let false_id = sqlx::query("INSERT INTO catalog.metadata (id, version) VALUES (FALSE, 1)")
        .execute(&pool)
        .await;
    assert_db_error(
        false_id,
        CHECK_VIOLATION,
        None,
        "metadata.id=FALSE 应被 CHECK 拒绝",
    );

    for invalid_version in [0_i64, -1] {
        let invalid = sqlx::query("UPDATE catalog.metadata SET version = $1 WHERE id = TRUE")
            .bind(invalid_version)
            .execute(&pool)
            .await;
        assert_db_error(
            invalid,
            CHECK_VIOLATION,
            None,
            "metadata.version 必须大于 0",
        );
    }
}

#[sqlx::test]
async fn part_of_speech_seeds_match_the_contract(pool: PgPool) {
    let actual: Vec<(String, String, String, String, i32)> = sqlx::query_as(
        r#"
        SELECT code, name_zh, name_en, abbreviation, sort_order
        FROM catalog.parts_of_speech
        ORDER BY sort_order, created_at, id
        "#,
    )
    .fetch_all(&pool)
    .await
    .expect("查询基本词性种子应成功");

    let expected = vec![
        ("noun", "名词", "NOUN", "n.", 10),
        ("pronoun", "代词", "PRONOUN", "pron.", 20),
        ("verb", "动词", "VERB", "v.", 30),
        ("adjective", "形容词", "ADJECTIVE", "adj.", 40),
        ("adverb", "副词", "ADVERB", "adv.", 50),
        ("preposition", "介词", "PREPOSITION", "prep.", 60),
        ("article", "冠词", "ARTICLE", "art.", 70),
        ("determiner", "限定词", "DETERMINER", "det.", 80),
        ("conjunction", "连词", "CONJUNCTION", "conj.", 90),
        ("numeral", "数词", "NUMERAL", "num.", 100),
        ("interjection", "感叹词", "INTERJECTION", "int.", 110),
    ]
    .into_iter()
    .map(|(code, zh, en, abbreviation, order)| {
        (
            code.to_owned(),
            zh.to_owned(),
            en.to_owned(),
            abbreviation.to_owned(),
            order,
        )
    })
    .collect::<Vec<_>>();

    assert_eq!(actual, expected, "11 个基本词性种子必须逐字段匹配设计契约");
}

#[sqlx::test]
async fn sub_part_of_speech_seeds_match_the_contract(pool: PgPool) {
    let actual: Vec<(String, String, String, String, i32)> = sqlx::query_as(
        r#"
        SELECT parent.code, child.code, child.name_zh, child.name_en, child.sort_order
        FROM catalog.sub_parts_of_speech AS child
        JOIN catalog.parts_of_speech AS parent ON parent.id = child.part_of_speech_id
        ORDER BY child.sort_order, child.created_at, child.id
        "#,
    )
    .fetch_all(&pool)
    .await
    .expect("查询细分词性种子应成功");

    let expected = vec![
        ("verb", "V-T", "及物动词", "Transitive verb", 10),
        ("verb", "V-I", "不及物动词", "Intransitive verb", 20),
        ("verb", "V-LINK", "系动词", "Linking verb", 30),
        ("verb", "AUX", "助动词", "Auxiliary verb", 40),
        ("verb", "MODAL", "情态动词", "Modal verb", 50),
        ("adjective", "ADJ", "形容词", "Adjective", 60),
        ("adverb", "ADV", "副词", "Adverb", 70),
        ("noun", "N-COUNT", "可数名词", "Countable noun", 80),
        ("noun", "N-UNCOUNT", "不可数名词", "Uncountable noun", 90),
        ("noun", "N-PROPER", "专有名词", "Proper noun", 100),
        ("noun", "N-PLURAL", "复数名词", "Plural noun", 110),
        ("noun", "N-SING", "单数名词", "Singular noun", 120),
        ("pronoun", "PRON", "代词", "Pronoun", 130),
        ("preposition", "PREP", "介词", "Preposition", 140),
        ("conjunction", "CONJ", "连词", "Conjunction", 150),
        ("determiner", "DET", "限定词", "Determiner", 160),
        ("article", "ART", "冠词", "Article", 170),
        ("numeral", "NUM", "数词", "Numeral", 180),
        ("interjection", "INT", "感叹词", "Interjection", 190),
    ]
    .into_iter()
    .map(|(parent, code, zh, en, order)| {
        (
            parent.to_owned(),
            code.to_owned(),
            zh.to_owned(),
            en.to_owned(),
            order,
        )
    })
    .collect::<Vec<_>>();

    assert_eq!(actual, expected, "19 个细分词性种子必须逐字段匹配设计契约");
}

#[sqlx::test]
async fn seeds_use_fixed_v7_ids_and_system_audit_fields(pool: PgPool) {
    let rows: Vec<(Uuid, bool, bool, i64)> = sqlx::query_as(
        r#"
        SELECT id,
               created_by_admin_id IS NULL,
               updated_by_admin_id IS NULL,
               revision
        FROM catalog.parts_of_speech
        UNION ALL
        SELECT id,
               created_by_admin_id IS NULL,
               updated_by_admin_id IS NULL,
               revision
        FROM catalog.sub_parts_of_speech
        "#,
    )
    .fetch_all(&pool)
    .await
    .expect("查询种子 ID 与审计字段应成功");

    assert_eq!(rows.len(), 30, "应检查全部 30 条默认种子");
    for (id, created_by_is_null, updated_by_is_null, revision) in rows {
        assert_eq!(
            id.get_version_num(),
            7,
            "种子 ID {id} 必须是预生成的 UUID v7"
        );
        assert!(
            created_by_is_null,
            "系统种子的 created_by_admin_id 必须为 NULL"
        );
        assert!(
            updated_by_is_null,
            "尚未修改的系统种子 updated_by_admin_id 必须为 NULL"
        );
        assert_eq!(revision, 1, "系统种子的 revision 必须从 1 开始");
    }
}

// ===== 基本词性：默认值、唯一索引与 CHECK =====

#[sqlx::test]
async fn part_defaults_are_correct(pool: PgPool) {
    let id = insert_part(&pool, "custom_default")
        .await
        .expect("插入基本词性应成功");
    let actual: (i32, i64, bool, bool, bool, bool) = sqlx::query_as(
        r#"
        SELECT sort_order,
               revision,
               created_by_admin_id IS NULL,
               updated_by_admin_id IS NULL,
               created_at IS NOT NULL,
               updated_at IS NOT NULL
        FROM catalog.parts_of_speech
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("查询基本词性默认值应成功");
    assert_eq!(actual, (0, 1, true, true, true, true));
}

#[sqlx::test]
async fn part_unique_values_use_fixed_index_names(pool: PgPool) {
    let cases = [
        (
            "noun",
            "唯一中文一",
            "Unique English One",
            "u1",
            "catalog_parts_of_speech_code_unique_idx",
        ),
        (
            "unique_name_zh",
            "名词",
            "Unique English Two",
            "u2",
            "catalog_parts_of_speech_name_zh_unique_idx",
        ),
        (
            "unique_name_en",
            "唯一中文三",
            "noun",
            "u3",
            "catalog_parts_of_speech_name_en_unique_idx",
        ),
        (
            "unique_abbreviation",
            "唯一中文四",
            "Unique English Four",
            "N.",
            "catalog_parts_of_speech_abbreviation_unique_idx",
        ),
    ];

    for (code, name_zh, name_en, abbreviation, expected_index) in cases {
        let result = insert_part_values(&pool, code, name_zh, name_en, abbreviation, 0).await;
        assert_db_error(
            result,
            UNIQUE_VIOLATION,
            Some(expected_index),
            "基本词性重复值应命中固定唯一索引",
        );
    }
}

#[sqlx::test]
async fn part_code_check_rejects_invalid_values_and_accepts_boundaries(pool: PgPool) {
    for invalid_code in [
        "",
        "NOUN",
        "1noun",
        "part-of-speech",
        "contains.dot",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let result = insert_part(&pool, invalid_code).await;
        assert_db_error(
            result,
            CHECK_VIOLATION,
            None,
            &format!("非法基本词性 code={invalid_code:?} 应被 CHECK 拒绝"),
        );
    }

    insert_part(&pool, "a")
        .await
        .expect("单字符小写 code 应合法");
    insert_part(&pool, "a2345678901234567890123456789012")
        .await
        .expect("32 字符 code 应合法");
    insert_part(&pool, "with_underscore")
        .await
        .expect("小写 code 应允许下划线");
}

#[sqlx::test]
async fn part_text_checks_reject_blank_untrimmed_and_overlong_values(pool: PgPool) {
    let cases = vec![
        (
            "bad_zh_blank",
            " ".to_owned(),
            "Valid En".to_owned(),
            "ok".to_owned(),
        ),
        (
            "bad_zh_trim",
            " 未裁剪".to_owned(),
            "Valid En 2".to_owned(),
            "ok2".to_owned(),
        ),
        (
            "bad_zh_long",
            "名".repeat(65),
            "Valid En 3".to_owned(),
            "ok3".to_owned(),
        ),
        (
            "bad_en_blank",
            "合法中文一".to_owned(),
            "".to_owned(),
            "ok4".to_owned(),
        ),
        (
            "bad_en_trim",
            "合法中文二".to_owned(),
            "Trailing ".to_owned(),
            "ok5".to_owned(),
        ),
        (
            "bad_en_long",
            "合法中文三".to_owned(),
            "e".repeat(65),
            "ok6".to_owned(),
        ),
        (
            "bad_abbr_blank",
            "合法中文四".to_owned(),
            "Valid En 4".to_owned(),
            " ".to_owned(),
        ),
        (
            "bad_abbr_trim",
            "合法中文五".to_owned(),
            "Valid En 5".to_owned(),
            " x".to_owned(),
        ),
        (
            "bad_abbr_long",
            "合法中文六".to_owned(),
            "Valid En 6".to_owned(),
            "a".repeat(17),
        ),
    ];

    for (code, name_zh, name_en, abbreviation) in cases {
        let result = insert_part_values(&pool, code, &name_zh, &name_en, &abbreviation, 0).await;
        assert_db_error(
            result,
            CHECK_VIOLATION,
            None,
            &format!("基本词性 {code} 的非法文本应被 CHECK 拒绝"),
        );
    }

    insert_part_values(
        &pool,
        "text_boundaries",
        &"中".repeat(64),
        &"e".repeat(64),
        &"a".repeat(16),
        0,
    )
    .await
    .expect("名称 64 字符、缩写 16 字符的上边界应合法");
}

#[sqlx::test]
async fn part_revision_must_be_positive(pool: PgPool) {
    for invalid_revision in [0_i64, -1] {
        let id = Uuid::now_v7();
        let result = sqlx::query(
            r#"
            INSERT INTO catalog.parts_of_speech (
                id, code, name_zh, name_en, abbreviation, revision
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(id)
        .bind(format!("revision_{}", invalid_revision.unsigned_abs()))
        .bind(format!("修订{}", short_id(id)))
        .bind(format!("Revision {}", short_id(id)))
        .bind(format!("r{}", &short_id(id)[..4]))
        .bind(invalid_revision)
        .execute(&pool)
        .await;
        assert_db_error(
            result,
            CHECK_VIOLATION,
            None,
            "基本词性 revision 必须大于 0",
        );
    }
}

#[sqlx::test]
async fn part_sort_order_accepts_full_integer_range_and_duplicates(pool: PgPool) {
    insert_part_values(
        &pool,
        "sort_min",
        "排序最小",
        "Sort Minimum",
        "smin",
        i32::MIN,
    )
    .await
    .expect("sort_order 应接受 INTEGER 最小值");
    insert_part_values(
        &pool,
        "sort_max",
        "排序最大",
        "Sort Maximum",
        "smax",
        i32::MAX,
    )
    .await
    .expect("sort_order 应接受 INTEGER 最大值");
    insert_part_values(
        &pool,
        "sort_same_a",
        "排序重复甲",
        "Sort Same A",
        "ssa",
        -10,
    )
    .await
    .expect("首个重复 sort_order 应成功");
    insert_part_values(
        &pool,
        "sort_same_b",
        "排序重复乙",
        "Sort Same B",
        "ssb",
        -10,
    )
    .await
    .expect("第二个重复 sort_order 应成功");
}

// ===== 细分词性：默认值、唯一索引、CHECK 与父级外键 =====

#[sqlx::test]
async fn sub_part_defaults_are_correct(pool: PgPool) {
    let parent = part_id(&pool, "noun").await;
    let id = insert_sub(&pool, parent, "CUSTOM_DEFAULT")
        .await
        .expect("插入细分词性应成功");
    let actual: (i32, i64, bool, bool, bool, bool) = sqlx::query_as(
        r#"
        SELECT sort_order,
               revision,
               created_by_admin_id IS NULL,
               updated_by_admin_id IS NULL,
               created_at IS NOT NULL,
               updated_at IS NOT NULL
        FROM catalog.sub_parts_of_speech
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .expect("查询细分词性默认值应成功");
    assert_eq!(actual, (0, 1, true, true, true, true));
}

#[sqlx::test]
async fn sub_part_unique_values_use_fixed_index_names(pool: PgPool) {
    let noun = part_id(&pool, "noun").await;
    let cases = [
        (
            "V-T",
            "唯一细分中文一",
            "Unique Sub One",
            "catalog_sub_parts_code_unique_idx",
        ),
        (
            "UNIQUE-ZH",
            "可数名词",
            "Unique Sub Two",
            "catalog_sub_parts_name_zh_unique_idx",
        ),
        (
            "UNIQUE-EN",
            "唯一细分中文三",
            "countable NOUN",
            "catalog_sub_parts_name_en_unique_idx",
        ),
    ];

    for (code, name_zh, name_en, expected_index) in cases {
        let result = insert_sub_values(&pool, noun, code, name_zh, name_en, 0).await;
        assert_db_error(
            result,
            UNIQUE_VIOLATION,
            Some(expected_index),
            "细分词性重复值应命中固定唯一索引",
        );
    }
}

#[sqlx::test]
async fn sub_part_names_may_repeat_under_different_parents(pool: PgPool) {
    let verb = part_id(&pool, "verb").await;
    insert_sub_values(&pool, verb, "CROSS-PARENT", "可数名词", "COUNTABLE NOUN", 0)
        .await
        .expect("中英文名唯一性只应限制在同一基本词性下");
}

#[sqlx::test]
async fn sub_part_code_check_rejects_invalid_values_and_accepts_boundaries(pool: PgPool) {
    let parent = part_id(&pool, "noun").await;
    for invalid_code in [
        "",
        "noun",
        "1-NOUN",
        "HAS.DOT",
        "HAS SPACE",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        let result = insert_sub(&pool, parent, invalid_code).await;
        assert_db_error(
            result,
            CHECK_VIOLATION,
            None,
            &format!("非法细分词性 code={invalid_code:?} 应被 CHECK 拒绝"),
        );
    }

    insert_sub(&pool, parent, "Z")
        .await
        .expect("单字符大写 code 应合法");
    insert_sub(&pool, parent, "A2345678901234567890123456789012")
        .await
        .expect("32 字符 code 应合法");
    insert_sub(&pool, parent, "WITH_DASH-AND_UNDERSCORE")
        .await
        .expect("细分 code 应允许连字符和下划线");
}

#[sqlx::test]
async fn sub_part_text_checks_reject_blank_untrimmed_and_overlong_values(pool: PgPool) {
    let parent = part_id(&pool, "noun").await;
    let cases = vec![
        ("BAD-ZH-BLANK", " ".to_owned(), "Valid Sub One".to_owned()),
        (
            "BAD-ZH-TRIM",
            "未裁剪 ".to_owned(),
            "Valid Sub Two".to_owned(),
        ),
        ("BAD-ZH-LONG", "名".repeat(65), "Valid Sub Three".to_owned()),
        ("BAD-EN-BLANK", "合法细分一".to_owned(), "".to_owned()),
        (
            "BAD-EN-TRIM",
            "合法细分二".to_owned(),
            " Leading".to_owned(),
        ),
        ("BAD-EN-LONG", "合法细分三".to_owned(), "e".repeat(65)),
    ];

    for (code, name_zh, name_en) in cases {
        let result = insert_sub_values(&pool, parent, code, &name_zh, &name_en, 0).await;
        assert_db_error(
            result,
            CHECK_VIOLATION,
            None,
            &format!("细分词性 {code} 的非法文本应被 CHECK 拒绝"),
        );
    }

    insert_sub_values(
        &pool,
        parent,
        "TEXT-BOUNDARY",
        &"中".repeat(64),
        &"e".repeat(64),
        0,
    )
    .await
    .expect("细分词性名称 64 字符的上边界应合法");
}

#[sqlx::test]
async fn sub_part_revision_must_be_positive(pool: PgPool) {
    let parent = part_id(&pool, "noun").await;
    for (index, invalid_revision) in [0_i64, -1].into_iter().enumerate() {
        let result = sqlx::query(
            r#"
            INSERT INTO catalog.sub_parts_of_speech (
                id, part_of_speech_id, code, name_zh, name_en, revision
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(parent)
        .bind(format!("BAD-REVISION-{index}"))
        .bind(format!("非法修订{index}"))
        .bind(format!("Invalid Revision {index}"))
        .bind(invalid_revision)
        .execute(&pool)
        .await;
        assert_db_error(
            result,
            CHECK_VIOLATION,
            None,
            "细分词性 revision 必须大于 0",
        );
    }
}

#[sqlx::test]
async fn sub_part_must_reference_an_existing_parent(pool: PgPool) {
    let result = insert_sub(&pool, Uuid::now_v7(), "GHOST-PARENT").await;
    assert_db_error(
        result,
        FOREIGN_KEY_VIOLATION,
        None,
        "细分词性不能引用不存在的基本词性",
    );
}

#[sqlx::test]
async fn deleting_part_cascades_its_sub_parts(pool: PgPool) {
    let parent = insert_part(&pool, "cascade_parent")
        .await
        .expect("插入待删除基本词性应成功");
    insert_sub(&pool, parent, "CASCADE-A")
        .await
        .expect("插入细分词性 A 应成功");
    insert_sub(&pool, parent, "CASCADE-B")
        .await
        .expect("插入细分词性 B 应成功");

    sqlx::query("DELETE FROM catalog.parts_of_speech WHERE id = $1")
        .bind(parent)
        .execute(&pool)
        .await
        .expect("删除未被词条引用的基本词性应成功");

    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM catalog.sub_parts_of_speech WHERE part_of_speech_id = $1",
    )
    .bind(parent)
    .fetch_one(&pool)
    .await
    .expect("查询级联删除结果应成功");
    assert_eq!(remaining, 0, "删除基本词性应级联删除其全部细分词性");
}

#[sqlx::test]
async fn sub_part_sort_order_accepts_full_integer_range_and_duplicates(pool: PgPool) {
    let parent = part_id(&pool, "noun").await;
    insert_sub_values(
        &pool,
        parent,
        "SORT-MIN",
        "细分最小",
        "Sub Minimum",
        i32::MIN,
    )
    .await
    .expect("细分 sort_order 应接受 INTEGER 最小值");
    insert_sub_values(
        &pool,
        parent,
        "SORT-MAX",
        "细分最大",
        "Sub Maximum",
        i32::MAX,
    )
    .await
    .expect("细分 sort_order 应接受 INTEGER 最大值");
    insert_sub_values(
        &pool,
        parent,
        "SORT-SAME-A",
        "细分重复甲",
        "Sub Same A",
        -10,
    )
    .await
    .expect("首个重复细分 sort_order 应成功");
    insert_sub_values(
        &pool,
        parent,
        "SORT-SAME-B",
        "细分重复乙",
        "Sub Same B",
        -10,
    )
    .await
    .expect("第二个重复细分 sort_order 应成功");
}

// ===== 审计外键与索引命名契约 =====

#[sqlx::test]
async fn audit_columns_reject_nonexistent_admins(pool: PgPool) {
    let ghost = Uuid::now_v7();
    for column in ["created_by_admin_id", "updated_by_admin_id"] {
        let id = Uuid::now_v7();
        let sql = match column {
            "created_by_admin_id" => {
                r#"
                INSERT INTO catalog.parts_of_speech (
                    id, code, name_zh, name_en, abbreviation, created_by_admin_id
                )
                VALUES ($1, $2, $3, $4, $5, $6)
                "#
            }
            "updated_by_admin_id" => {
                r#"
                INSERT INTO catalog.parts_of_speech (
                    id, code, name_zh, name_en, abbreviation, updated_by_admin_id
                )
                VALUES ($1, $2, $3, $4, $5, $6)
                "#
            }
            _ => unreachable!("审计列名来自固定测试用例"),
        };
        let result = sqlx::query(sql)
            .bind(id)
            .bind(format!("audit_{}", short_id(id)))
            .bind(format!("审计{}", short_id(id)))
            .bind(format!("Audit {}", short_id(id)))
            .bind(format!("a{}", &short_id(id)[..4]))
            .bind(ghost)
            .execute(&pool)
            .await;
        assert_db_error(
            result,
            FOREIGN_KEY_VIOLATION,
            None,
            &format!("基本词性 {column} 必须引用存在的管理员"),
        );
    }

    let parent = part_id(&pool, "noun").await;
    for (index, column) in ["created_by_admin_id", "updated_by_admin_id"]
        .into_iter()
        .enumerate()
    {
        let sql = match column {
            "created_by_admin_id" => {
                r#"
                INSERT INTO catalog.sub_parts_of_speech (
                    id, part_of_speech_id, code, name_zh, name_en, created_by_admin_id
                )
                VALUES ($1, $2, $3, $4, $5, $6)
                "#
            }
            "updated_by_admin_id" => {
                r#"
                INSERT INTO catalog.sub_parts_of_speech (
                    id, part_of_speech_id, code, name_zh, name_en, updated_by_admin_id
                )
                VALUES ($1, $2, $3, $4, $5, $6)
                "#
            }
            _ => unreachable!("审计列名来自固定测试用例"),
        };
        let result = sqlx::query(sql)
            .bind(Uuid::now_v7())
            .bind(parent)
            .bind(format!("AUDIT-{index}"))
            .bind(format!("细分审计{index}"))
            .bind(format!("Sub Audit {index}"))
            .bind(ghost)
            .execute(&pool)
            .await;
        assert_db_error(
            result,
            FOREIGN_KEY_VIOLATION,
            None,
            &format!("细分词性 {column} 必须引用存在的管理员"),
        );
    }
}

#[sqlx::test]
async fn all_audit_foreign_keys_use_on_delete_restrict(pool: PgPool) {
    let actual: Vec<(String, String, String)> = sqlx::query_as(
        r#"
        SELECT namespace.nspname || '.' || relation.relname AS table_name,
               attribute.attname AS column_name,
               constraint_row.confdeltype::text AS delete_action
        FROM pg_constraint AS constraint_row
        JOIN pg_class AS relation ON relation.oid = constraint_row.conrelid
        JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
        JOIN LATERAL unnest(constraint_row.conkey) AS key_column(attnum) ON TRUE
        JOIN pg_attribute AS attribute
          ON attribute.attrelid = constraint_row.conrelid
         AND attribute.attnum = key_column.attnum
        WHERE constraint_row.contype = 'f'
          AND constraint_row.confrelid = 'admins'::regclass
          AND namespace.nspname = 'catalog'
          AND relation.relname IN ('parts_of_speech', 'sub_parts_of_speech')
          AND attribute.attname IN ('created_by_admin_id', 'updated_by_admin_id')
        ORDER BY table_name, column_name
        "#,
    )
    .fetch_all(&pool)
    .await
    .expect("查询审计外键删除策略应成功");

    assert_eq!(
        actual,
        vec![
            (
                "catalog.parts_of_speech".to_owned(),
                "created_by_admin_id".to_owned(),
                "r".to_owned(),
            ),
            (
                "catalog.parts_of_speech".to_owned(),
                "updated_by_admin_id".to_owned(),
                "r".to_owned(),
            ),
            (
                "catalog.sub_parts_of_speech".to_owned(),
                "created_by_admin_id".to_owned(),
                "r".to_owned(),
            ),
            (
                "catalog.sub_parts_of_speech".to_owned(),
                "updated_by_admin_id".to_owned(),
                "r".to_owned(),
            ),
        ],
        "四个管理员审计外键都必须显式使用 ON DELETE RESTRICT"
    );
}

#[sqlx::test]
async fn referenced_admin_cannot_be_deleted(pool: PgPool) {
    let admin = insert_admin(&pool).await;
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO catalog.parts_of_speech (
            id, code, name_zh, name_en, abbreviation,
            created_by_admin_id, updated_by_admin_id
        )
        VALUES ($1, 'audited_part', '有审计词性', 'Audited Part', 'audit', $2, $2)
        "#,
    )
    .bind(id)
    .bind(admin)
    .execute(&pool)
    .await
    .expect("引用真实管理员的基本词性应插入成功");

    let result = sqlx::query("DELETE FROM admins WHERE id = $1")
        .bind(admin)
        .execute(&pool)
        .await;
    assert_db_error(
        result,
        FOREIGN_KEY_VIOLATION,
        None,
        "仍被 catalog 审计字段引用的管理员不得删除",
    );
}

#[sqlx::test]
async fn all_fixed_catalog_indexes_exist(pool: PgPool) {
    let actual: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT indexname
        FROM pg_indexes
        WHERE schemaname = 'catalog'
          AND indexname = ANY($1)
        ORDER BY indexname
        "#,
    )
    .bind(
        &[
            "catalog_parts_of_speech_abbreviation_unique_idx",
            "catalog_parts_of_speech_code_unique_idx",
            "catalog_parts_of_speech_name_en_unique_idx",
            "catalog_parts_of_speech_name_zh_unique_idx",
            "catalog_parts_of_speech_order_idx",
            "catalog_sub_parts_code_unique_idx",
            "catalog_sub_parts_name_en_unique_idx",
            "catalog_sub_parts_name_zh_unique_idx",
            "catalog_sub_parts_order_idx",
        ][..],
    )
    .fetch_all(&pool)
    .await
    .expect("查询 catalog 索引应成功");

    let expected = vec![
        "catalog_parts_of_speech_abbreviation_unique_idx".to_owned(),
        "catalog_parts_of_speech_code_unique_idx".to_owned(),
        "catalog_parts_of_speech_name_en_unique_idx".to_owned(),
        "catalog_parts_of_speech_name_zh_unique_idx".to_owned(),
        "catalog_parts_of_speech_order_idx".to_owned(),
        "catalog_sub_parts_code_unique_idx".to_owned(),
        "catalog_sub_parts_name_en_unique_idx".to_owned(),
        "catalog_sub_parts_name_zh_unique_idx".to_owned(),
        "catalog_sub_parts_order_idx".to_owned(),
    ];
    assert_eq!(actual, expected, "设计中固定名称的 9 个索引必须全部存在");
}
