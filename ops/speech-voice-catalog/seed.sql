-- speech.voices 发音人目录种子。幂等，可重复执行。
--
-- 放在 ops/ 而不在 migrations/：voice 目录是运营数据而非 schema，
-- migration 只建表（docs/tts-preview-api-design.md §2：「空目录是合法状态」）。
-- 应用启动会自动跑 migration，不应该顺手改写运营维护的目录。
--
-- styles 逐个发音人取自 Azure voices list 的 StyleList，**各不相同，不能互相照抄**：
-- 目录是 style allowlist 的唯一来源，写进去的 style 后端就放行；Azure 对不支持的 style
-- 不报错，直接忽略（实测 HTTP 200，照常出音频），所以抄错的后果是「选了没效果」这种
-- 静默错误，不会有人报错给你。改动前按 README 重新向 Azure 逐个核对。
--
-- 只写入身份与能力。enabled 和 rate/pitch 上下限归运维/建表默认所有：
-- 前者让被停用的发音人不会被重跑种子悄悄启用，后者复用 speech.voices 的列默认值。

INSERT INTO speech.voices
    (id, alias, provider, provider_voice_id, locale, gender, styles, provider_version)
VALUES
    (
        '019ffbda-1000-79d2-9272-28c5932b0bf8',
        'en-us-aria',
        'azure',
        'en-US-AriaNeural',
        'en-US',
        'female',
        '["angry","chat","cheerful","customerservice","empathetic","excited","friendly","hopeful","narration-professional","newscast-casual","newscast-formal","sad","shouting","terrified","unfriendly","whispering"]',
        'azure-voices-list-2026-08-13'
    ),
    (
        '019ffbda-1000-7ef5-a6cb-a9dd7aa26d9d',
        'en-us-davis',
        'azure',
        'en-US-DavisNeural',
        'en-US',
        'male',
        '["angry","chat","cheerful","excited","friendly","hopeful","sad","shouting","terrified","unfriendly","whispering"]',
        'azure-voices-list-2026-08-13'
    ),
    (
        '01a0219e-0800-767a-903a-7258b41a1c9c',
        'en-gb-sonia',
        'azure',
        'en-GB-SoniaNeural',
        'en-GB',
        'female',
        '["cheerful","sad"]',
        'azure-voices-list-2026-08-21'
    )
ON CONFLICT (alias) DO UPDATE SET
    provider = EXCLUDED.provider,
    provider_voice_id = EXCLUDED.provider_voice_id,
    locale = EXCLUDED.locale,
    gender = EXCLUDED.gender,
    styles = EXCLUDED.styles,
    provider_version = EXCLUDED.provider_version,
    updated_at = now()
-- 只在目录事实真的漂移时才写，重跑不动 updated_at。
WHERE ROW(
        speech.voices.provider,
        speech.voices.provider_voice_id,
        speech.voices.locale,
        speech.voices.gender,
        speech.voices.styles,
        speech.voices.provider_version
    ) IS DISTINCT FROM ROW(
        EXCLUDED.provider,
        EXCLUDED.provider_voice_id,
        EXCLUDED.locale,
        EXCLUDED.gender,
        EXCLUDED.styles,
        EXCLUDED.provider_version
    );
