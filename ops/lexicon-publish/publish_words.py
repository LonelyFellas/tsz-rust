#!/usr/bin/env python3
"""通过 admin API 创建并发布 Smart Lexicon V3 词条，替代在管理后台逐步点击。

一个词条走完整条原生 V3 链路：
detect → create → forms(impact + complete) → meanings(complete) → validate → publish。

凭据从环境变量读取（也可用命令行覆盖）：
  TSZ_ADMIN_PHONE / TSZ_ADMIN_PASSWORD  登录用，没有可复用会话时必填
  TSZ_ADMIN_OTP_CODE                    可选，默认 000000（OtpSender::Mock 的固定码）
  TSZ_ADMIN_TOKEN                       可选，直接给 access token 就不用登录
  TSZ_BASE_URL                          可选，默认本地 http://127.0.0.1:8383

用法见同目录 README.md。
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from typing import Any

DEFAULT_BASE_URL = "http://127.0.0.1:8383"
DEFAULT_OTP_CODE = "000000"
DEFAULT_POS = "noun"
DEFAULT_LEVEL = "A1"
DEFAULT_FREQUENCY = "100"
DEFAULT_GRAMMAR = "general use"
DEFAULT_GROUP_ZH = "核心义"
DEFAULT_GROUP_EN = "core"
DEFAULT_STYLE = "normal"

LEVELS = ("A1", "A2", "B1", "B2", "C1", "C2")
DERIVED_FORM_TYPES = (
    "third_person_singular",
    "present_participle",
    "past_tense",
    "past_participle",
    "plural",
    "comparative",
    "superlative",
)

ADMIN_ROOT = "/api/v1/admin"
LEXICON_ROOT = f"{ADMIN_ROOT}/lexicon"
REFRESH_COOKIE = "admin_refresh_token"
# 后端 OTP 每个手机号一天只发 10 次码，所以会话要尽量靠 refresh token 续，别每次都重新登录。
SESSION_CACHE_DIR = pathlib.Path.home() / ".cache" / "tsz-lexicon-publish"


class SpecError(Exception):
    """输入词条描述不合法。"""


class ApiError(Exception):
    """后端返回了非预期状态码。"""

    def __init__(self, method: str, path: str, status: int, payload: Any) -> None:
        self.status = status
        self.payload = payload
        super().__init__(f"{method} {path} → HTTP {status}\n{_render_problem(payload)}")


def _render_problem(payload: Any) -> str:
    """只摘 RFC9457 里能定位问题的字段；surface_match_page 这种 meta 动辄上百行，不整段打。"""
    if not isinstance(payload, dict):
        return str(payload)
    brief = {
        key: payload[key]
        for key in ("code", "title", "detail", "field_issues", "issues")
        if key in payload
    }
    rendered = json.dumps(brief or payload, ensure_ascii=False, indent=2)
    return rendered if len(rendered) <= 4000 else f"{rendered[:4000]}…（已截断）"


def new_id() -> str:
    return str(uuid.uuid4())


def rich_text(text: str) -> dict[str, Any]:
    return {"version": 1, "text": text, "spans": [], "liaisons": []}


def _decode(raw: bytes) -> Any:
    if not raw:
        return None
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw.decode("utf-8", "replace")


def _cookie_value(headers: Any, name: str) -> str | None:
    for header in headers.get_all("Set-Cookie") or []:
        if header.startswith(f"{name}="):
            return header.split(";", 1)[0].split("=", 1)[1] or None
    return None


class AdminClient:
    def __init__(self, base_url: str, timeout: float = 30.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.token: str | None = None

    def call(
        self,
        method: str,
        path: str,
        body: Any = None,
        *,
        idempotency_key: str | None = None,
        expect: tuple[int, ...] = (200,),
    ) -> Any:
        status, payload, _ = self.raw(method, path, body, idempotency_key=idempotency_key)
        if status not in expect:
            raise ApiError(method, path, status, payload)
        return payload

    def raw(
        self,
        method: str,
        path: str,
        body: Any = None,
        *,
        idempotency_key: str | None = None,
        cookie: str | None = None,
    ) -> tuple[int, Any, Any]:
        headers = {"Accept": "application/json"}
        data = None
        if body is not None:
            data = json.dumps(body, ensure_ascii=False).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        if idempotency_key:
            headers["Idempotency-Key"] = idempotency_key
        if cookie:
            headers["Cookie"] = cookie
        request = urllib.request.Request(
            f"{self.base_url}{path}", data=data, headers=headers, method=method
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                return response.status, _decode(response.read()), response.headers
        except urllib.error.HTTPError as error:
            return error.code, _decode(error.read()), error.headers
        except urllib.error.URLError as error:
            raise SystemExit(f"无法连接 {self.base_url}{path}：{error.reason}") from error

    # --- 会话 ---

    def authenticate(
        self,
        phone: str,
        password: str,
        code: str,
        *,
        token: str | None = None,
        use_cache: bool = True,
    ) -> str:
        """建立会话，返回这次用的是哪条路径（打印给用户看）。

        优先级：显式 token → 缓存里没过期的 access token → refresh 轮换 → 手机号+密码+验证码。
        越靠前越不消耗验证码额度。
        """
        if token:
            self.token = token
            return "沿用显式传入的 access token"
        cache = self._load_cache() if use_cache else {}
        if cache.get("access_token") and cache.get("access_expires_at", 0) > time.time() + 60:
            self.token = cache["access_token"]
            return "复用缓存的 access token"
        if cache.get("refresh_token") and self._refresh(cache["refresh_token"], use_cache):
            return "用缓存的 refresh token 续期"
        if not phone or not password:
            raise SpecError(
                "没有可复用的会话，需要 TSZ_ADMIN_PHONE / TSZ_ADMIN_PASSWORD（或 --phone/--password）才能登录"
            )
        self._login(phone, password, code, use_cache)
        return "手机号+密码+验证码登录"

    def _login(self, phone: str, password: str, code: str, use_cache: bool) -> None:
        self.call("POST", f"{ADMIN_ROOT}/auth/login-code", {"phone": phone}, expect=(202,))
        status, payload, headers = self.raw(
            "POST",
            f"{ADMIN_ROOT}/auth/login",
            {"phone": phone, "code": code, "password": password},
        )
        if status != 200:
            raise ApiError("POST", f"{ADMIN_ROOT}/auth/login", status, payload)
        self.token = payload["access_token"]
        self._store_session(payload, headers, use_cache)

    def _refresh(self, refresh_token: str, use_cache: bool) -> bool:
        status, payload, headers = self.raw(
            "POST", f"{ADMIN_ROOT}/auth/refresh", cookie=f"{REFRESH_COOKIE}={refresh_token}"
        )
        if status != 200 or not isinstance(payload, dict):
            self._save_cache({})  # 这枚 refresh 已作废，别留着下次再撞一次重放检测
            return False
        self.token = payload["access_token"]
        self._store_session(payload, headers, use_cache)
        return True

    def _store_session(self, payload: dict[str, Any], headers: Any, use_cache: bool) -> None:
        if not use_cache:
            return
        cache = self._load_cache()
        cache["access_token"] = payload["access_token"]
        cache["access_expires_at"] = time.time() + payload.get("expires_in", 0)
        rotated = _cookie_value(headers, REFRESH_COOKIE)
        if rotated:
            cache["refresh_token"] = rotated
        self._save_cache(cache)

    def _cache_file(self) -> pathlib.Path:
        key = "".join(char if char.isalnum() else "-" for char in self.base_url)
        return SESSION_CACHE_DIR / f"{key}.json"

    def _load_cache(self) -> dict[str, Any]:
        try:
            return json.loads(self._cache_file().read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return {}

    def _save_cache(self, cache: dict[str, Any]) -> None:
        # 缓存只是省验证码额度，写不进去也不该打断发布。
        try:
            SESSION_CACHE_DIR.mkdir(parents=True, exist_ok=True)
            descriptor = os.open(
                self._cache_file(), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600
            )
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                json.dump(cache, handle)
        except OSError:
            pass

    def catalog(self) -> dict[str, list[str]]:
        """返回 {基本词性 code: [细分词性 code, ...]}，按后端 sort_order。"""
        payload = self.call("GET", f"{ADMIN_ROOT}/settings/parts-of-speech/catalog")
        return {
            part["code"]: [sub["code"] for sub in part["sub_parts"]]
            for part in payload["items"]
        }


# --- 输入规范化 ---


def _spelling_map(raw: Any, field: str, fallback: str | None = None) -> dict[str, str]:
    """把 "harbour" / {"common": ...} / {"uk": ..., "us": ...} 统一成方言字典。"""
    if raw is None:
        if fallback is None:
            raise SpecError(f"{field} 缺失")
        return {"common": fallback}
    if isinstance(raw, str):
        return {"common": raw}
    if not isinstance(raw, dict):
        raise SpecError(f"{field} 必须是字符串或对象")
    if "common" in raw:
        return {"common": str(raw["common"])}
    if "uk" in raw and "us" in raw:
        return {"uk": str(raw["uk"]), "us": str(raw["us"])}
    raise SpecError(f"{field} 必须给 common，或同时给 uk 和 us")


def _phonetic_map(raw: Any, field: str) -> dict[str, str]:
    if raw is None:
        return {}
    if isinstance(raw, str):
        return {"common": raw}
    if not isinstance(raw, dict):
        raise SpecError(f"{field} 必须是字符串或对象")
    return {key: str(value) for key, value in raw.items() if value}


def _to_uk_us(values: dict[str, str]) -> dict[str, str]:
    if "common" in values:
        return {"uk": values["common"], "us": values["common"]}
    return values


def normalize_spec(raw: dict[str, Any], catalog: dict[str, list[str]], defaults: argparse.Namespace) -> dict[str, Any]:
    """把宽松的输入描述补全成脚本内部使用的完整词条描述。"""
    if not isinstance(raw, dict):
        raise SpecError("词条描述必须是 JSON 对象")
    surface = str(raw.get("surface", "")).strip()
    if not surface:
        raise SpecError("surface 必填")
    kind = raw.get("kind", "word")
    if kind not in ("word", "phrase"):
        raise SpecError("kind 只能是 word 或 phrase")

    pos_entries = raw.get("pos")
    if not pos_entries:
        gloss = raw.get("gloss")
        if not gloss:
            raise SpecError("既没有 pos 列表，也没有 gloss，无法生成词条内容")
        pos_entries = [{"pos": raw.get("part_of_speech", defaults.pos), "senses": [{"gloss": gloss}]}]
    if not isinstance(pos_entries, list):
        raise SpecError("pos 必须是数组")

    normalized_pos = []
    for pos_entry in pos_entries:
        normalized_pos.append(_normalize_pos(pos_entry, surface, catalog, defaults))
    return {"surface": surface, "kind": kind, "pos": normalized_pos}


def _normalize_pos(raw: Any, surface: str, catalog: dict[str, list[str]], defaults: argparse.Namespace) -> dict[str, Any]:
    if not isinstance(raw, dict):
        raise SpecError("pos 元素必须是对象")
    code = str(raw.get("pos", defaults.pos))
    if code not in catalog:
        raise SpecError(f"基本词性 {code!r} 未在后端词性目录中配置（可用：{', '.join(sorted(catalog))}）")

    spelling = _spelling_map(raw.get("spelling"), "spelling", fallback=surface)
    phonetic = _phonetic_map(raw.get("pronunciation"), "pronunciation")

    extra_forms = []
    for extra in raw.get("extra_forms", []) or []:
        if not isinstance(extra, dict):
            raise SpecError("extra_forms 元素必须是对象")
        form_type = str(extra.get("form_type", ""))
        if form_type not in DERIVED_FORM_TYPES:
            raise SpecError(f"form_type {form_type!r} 无效（可用：{', '.join(DERIVED_FORM_TYPES)}）")
        extra_forms.append(
            {
                "form_type": form_type,
                "spelling": _spelling_map(extra.get("spelling"), f"extra_forms[{form_type}].spelling"),
                "pronunciation": _phonetic_map(extra.get("pronunciation"), "pronunciation"),
            }
        )

    # 一个词性内的所有词形必须共用同一套方言形状，任一处区分英美就整体升成 uk_us。
    distinguishes = any(
        "common" not in item["spelling"] or len(item["pronunciation"]) > 1
        for item in [{"spelling": spelling, "pronunciation": phonetic}, *extra_forms]
    )
    if distinguishes:
        spelling = _to_uk_us(spelling)
        phonetic = _to_uk_us(phonetic) if phonetic else {}
        for extra in extra_forms:
            extra["spelling"] = _to_uk_us(extra["spelling"])
            extra["pronunciation"] = _to_uk_us(extra["pronunciation"]) if extra["pronunciation"] else {}

    senses = raw.get("senses")
    if not senses:
        gloss = raw.get("gloss")
        if not gloss:
            raise SpecError(f"词性 {code} 既没有 senses，也没有 gloss")
        senses = [{"gloss": gloss}]
    if not isinstance(senses, list):
        raise SpecError("senses 必须是数组")

    sub_parts = catalog[code]
    normalized_senses = []
    for sense in senses:
        if isinstance(sense, str):
            sense = {"gloss": sense}
        if not isinstance(sense, dict):
            raise SpecError("senses 元素必须是字符串或对象")
        gloss = str(sense.get("gloss", "")).strip()
        if not gloss:
            raise SpecError("每个词义都要有中文释义 gloss")
        sub_pos = sense.get("sub_pos") or raw.get("sub_pos") or defaults.sub_pos
        if sub_pos is None:
            if not sub_parts:
                raise SpecError(f"词性 {code} 在后端没有配置细分词性，请先在后台补一个或显式传 sub_pos")
            sub_pos = sub_parts[0]
        if sub_pos not in sub_parts:
            raise SpecError(
                f"细分词性 {sub_pos!r} 不属于 {code}（可用：{', '.join(sub_parts) or '（空）'}）"
            )
        level = str(sense.get("level") or raw.get("level") or defaults.level)
        if level not in LEVELS:
            raise SpecError(f"level {level!r} 无效（可用：{', '.join(LEVELS)}）")
        frequency = str(sense.get("frequency") or raw.get("frequency") or defaults.frequency)
        group = sense.get("group") or raw.get("group") or {"zh": DEFAULT_GROUP_ZH, "en": DEFAULT_GROUP_EN}
        if isinstance(group, str):
            group = {"zh": group, "en": group}
        if not isinstance(group, dict) or not group.get("zh"):
            raise SpecError('group 必须是名称字符串，或 {"zh": ..., "en": ...}')
        normalized_senses.append(
            {
                "gloss": gloss,
                "sub_pos": sub_pos,
                "level": level,
                "frequency": frequency,
                "group": (str(group["zh"]), str(group.get("en") or group["zh"])),
                "grammar": str(sense.get("grammar") or raw.get("grammar") or defaults.grammar),
            }
        )

    return {
        "pos": code,
        "spelling": spelling,
        "pronunciation": phonetic,
        "extra_forms": extra_forms,
        "senses": normalized_senses,
    }


# --- 请求体构造 ---


def _pronunciation(spelling: str, phonetic: str | None) -> dict[str, Any]:
    # complete 要求音标三件套齐全，没给就按拼写兜一个占位值。
    dict_phonetic = phonetic or f"/{spelling}/"
    return {
        "id": new_id(),
        "dict_phonetic": dict_phonetic,
        "actual_pron": dict_phonetic.strip("/"),
        "style": DEFAULT_STYLE,
    }


def _variant(dialect: str, spelling: str, phonetic: str | None) -> dict[str, Any]:
    return {
        "id": new_id(),
        "dialect": dialect,
        "spelling": spelling,
        "origin": "manual",
        "pronunciations": [_pronunciation(spelling, phonetic)],
    }


def _concrete_form(form_type: str, spelling: dict[str, str], phonetic: dict[str, str]) -> dict[str, Any]:
    if "common" in spelling:
        regional = {"mode": "common", "common": _variant("common", spelling["common"], phonetic.get("common"))}
    else:
        regional = {
            "mode": "uk_us",
            "uk": _variant("uk", spelling["uk"], phonetic.get("uk")),
            "us": _variant("us", spelling["us"], phonetic.get("us")),
        }
    return {"id": new_id(), "form_type": form_type, "regional_variants": regional}


def _dialect_rules(spelling: dict[str, str]) -> dict[str, str]:
    if "common" in spelling:
        return {"spelling_mode": "unified", "phonetic_mode": "unified"}
    if spelling["uk"] == spelling["us"]:
        return {"spelling_mode": "unified", "phonetic_mode": "distinguish"}
    return {"spelling_mode": "distinguish", "phonetic_mode": "distinguish"}


def build_forms(spec: dict[str, Any]) -> dict[str, Any]:
    pos_blocks = []
    for pos in spec["pos"]:
        forms = [_concrete_form("base", pos["spelling"], pos["pronunciation"])]
        for extra in pos["extra_forms"]:
            forms.append(_concrete_form(extra["form_type"], extra["spelling"], extra["pronunciation"]))
        # 一个词性一组：complete 要求每组非空且含 base，每个词形至少属于一组。
        group = {
            "id": new_id(),
            "is_regular": True,
            "members": [{"id": new_id(), "form_id": form["id"]} for form in forms],
        }
        pos_blocks.append(
            {
                "pos_id": pos["pos_id"],
                "pos": pos["pos"],
                "dialect_rules": _dialect_rules(pos["spelling"]),
                "forms": forms,
                "form_groups": [group],
            }
        )
    return {"pos": pos_blocks}


def build_meanings(spec: dict[str, Any]) -> dict[str, Any]:
    groups: dict[tuple[str, str], str] = {}
    pos_blocks = []
    for pos in spec["pos"]:
        grammars: dict[str, str] = {}
        senses = []
        for sense in pos["senses"]:
            grammar_id = grammars.setdefault(sense["grammar"], new_id())
            group_id = groups.setdefault(sense["group"], new_id())
            senses.append(
                {
                    "id": new_id(),
                    "sub_pos": sense["sub_pos"],
                    "level": sense["level"],
                    "sense_group_id": group_id,
                    "frequency": sense["frequency"],
                    "depends_on_context": False,
                    "definitions": [
                        {
                            "definition_mode": "zh_definition",
                            "id": new_id(),
                            "content_id": new_id(),
                            "level": sense["level"],
                            "grammar_structure_id": grammar_id,
                            "content": rich_text(sense["gloss"]),
                        }
                    ],
                    "sentences": [],
                    "relations": [],
                }
            )
        pos_blocks.append(
            {
                "pos_id": pos["pos_id"],
                "grammar_structures": [
                    {
                        "id": grammar_id,
                        "variants": [
                            {"id": new_id(), "dialect": "common", "content": rich_text(text)}
                        ],
                    }
                    for text, grammar_id in grammars.items()
                ],
                "senses": senses,
            }
        )
    return {
        "sense_groups": [
            {"id": group_id, "name_zh": name_zh, "name_en": name_en}
            for (name_zh, name_en), group_id in groups.items()
        ],
        "pos": pos_blocks,
    }


# --- 发布链路 ---

# 同形词面确认：库里已有同词面的词条时，写操作会 409 并在回执里给一枚新令牌，带着它重放即可。
SURFACE_CONFIRMATION_CODES = (
    "surface_matches_changed",
    "surface_match_acknowledgement_required",
)


def surface_tokens(client: AdminClient, page: Any) -> dict[str, str]:
    """把 surface match 分页翻到末页取确认令牌——后端只在最后一页签发。"""
    tokens: dict[str, str] = {}
    while isinstance(page, dict):
        if page.get("surface_confirmation_token"):
            tokens["confirmed_surface_match_token"] = page["surface_confirmation_token"]
            if page.get("impact_confirmation_token"):
                tokens["confirmed_impact_token"] = page["impact_confirmation_token"]
            return tokens
        cursor = page.get("next_cursor")
        snapshot_id = page.get("snapshot_id")
        if not cursor or not snapshot_id:
            return tokens
        path = (
            f"{LEXICON_ROOT}/surface-match-snapshots/{snapshot_id}"
            f"?cursor={urllib.parse.quote(cursor, safe='')}"
        )
        status, payload, _ = client.raw("GET", path)
        if status != 200:
            raise ApiError("GET", path, status, payload)
        page = payload
    return tokens


def call_confirming_surfaces(
    client: AdminClient,
    method: str,
    path: str,
    body: dict[str, Any],
    *,
    expect: tuple[int, ...],
    idempotent: bool = False,
    attempts: int = 3,
) -> Any:
    body = dict(body)
    status: int = 0
    payload: Any = None
    for _ in range(attempts):
        # 换过 body 就得换幂等键，否则命中的是「同键不同体」的冲突分支。
        status, payload, _ = client.raw(
            method, path, body, idempotency_key=new_id() if idempotent else None
        )
        if status in expect:
            return payload
        if not isinstance(payload, dict) or payload.get("code") not in SURFACE_CONFIRMATION_CODES:
            break
        tokens = surface_tokens(client, (payload.get("meta") or {}).get("surface_match_page"))
        if not tokens.get("confirmed_surface_match_token"):
            break
        body.update(tokens)
    raise ApiError(method, path, status, payload)


def publish_entry(client: AdminClient, spec: dict[str, Any]) -> dict[str, Any]:
    surface = spec["surface"]

    detection = client.call(
        "POST",
        f"{LEXICON_ROOT}/detections",
        {"schema_version": 3, "language": "en", "kind": spec["kind"], "surface": surface},
    )

    create_body: dict[str, Any] = {
        "schema_version": 3,
        "detection_id": detection["detection_id"],
        "kind": spec["kind"],
    }
    create_body.update(surface_tokens(client, detection.get("surface_match_page")))
    created = call_confirming_surfaces(
        client,
        "POST",
        f"{LEXICON_ROOT}/entries",
        create_body,
        expect=(201,),
        idempotent=True,
    )
    entry_id = created["word"]["id"]
    revision = created["word"]["revision"]

    forms_content = build_forms(spec)
    impact = client.call(
        "POST",
        f"{LEXICON_ROOT}/entries/{entry_id}/steps/forms/impact",
        {"schema_version": 3, "base_revision": revision, "content": forms_content},
    )
    forms_body: dict[str, Any] = {
        "schema_version": 3,
        "base_revision": revision,
        "intent": "complete",
        "content": forms_content,
    }
    if impact.get("confirmation_token"):
        forms_body["confirmed_impact_token"] = impact["confirmation_token"]
    forms_body.update(surface_tokens(client, impact.get("surface_match_page")))

    saved = call_confirming_surfaces(
        client,
        "PUT",
        f"{LEXICON_ROOT}/entries/{entry_id}/steps/forms",
        forms_body,
        expect=(200,),
    )
    revision = saved["word"]["revision"]

    meanings = client.call(
        "PUT",
        f"{LEXICON_ROOT}/entries/{entry_id}/steps/meanings",
        {
            "schema_version": 3,
            "base_revision": revision,
            "intent": "complete",
            "content": build_meanings(spec),
        },
    )
    revision = meanings["word"]["revision"]

    validation = client.call(
        "POST",
        f"{LEXICON_ROOT}/entries/{entry_id}/validate",
        {"schema_version": 3, "base_revision": revision},
    )
    if not validation.get("valid"):
        raise ApiError(
            "POST", f"{LEXICON_ROOT}/entries/{entry_id}/validate", 200, validation
        )

    published = call_confirming_surfaces(
        client,
        "POST",
        f"{LEXICON_ROOT}/entries/{entry_id}/publications",
        {"schema_version": 3, "base_revision": revision},
        expect=(201,),
        idempotent=True,
    )
    return {
        "surface": surface,
        "entry_id": entry_id,
        "status": published["word"]["status"],
        "published_revision": published["word"]["published_revision"],
    }


# --- CLI ---


def parse_positional(value: str, defaults: argparse.Namespace) -> dict[str, Any]:
    """`harbour:港口` → 最小词条描述。"""
    surface, separator, gloss = value.partition(":")
    if not separator:
        raise SpecError(f"位置参数 {value!r} 缺少中文释义，格式是 单词:释义")
    return {"surface": surface.strip(), "gloss": gloss.strip(), "part_of_speech": defaults.pos}


def load_file(path: str) -> list[dict[str, Any]]:
    with open(path, encoding="utf-8") as handle:
        payload = json.load(handle)
    if isinstance(payload, dict) and "words" in payload:
        payload = payload["words"]
    if isinstance(payload, dict):
        payload = [payload]
    if not isinstance(payload, list):
        raise SpecError(f"{path} 必须是词条对象、词条数组，或 {{\"words\": [...]}}")
    return payload


def main() -> int:
    parser = argparse.ArgumentParser(
        description="创建并发布 Smart Lexicon V3 词条（默认打本地后端）",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "示例：\n"
            "  ./publish_words.py harbour:港口 apple:苹果\n"
            "  ./publish_words.py --file example-words.json\n"
            "  ./publish_words.py harbour:港口 --base-url http://47.121.142.19:8383\n"
        ),
    )
    parser.add_argument("words", nargs="*", metavar="单词:释义", help="最小模板批量发布")
    parser.add_argument("--file", help="词条描述 JSON（对象、数组或 {\"words\": [...]}）")
    parser.add_argument("--base-url", default=os.environ.get("TSZ_BASE_URL", DEFAULT_BASE_URL))
    parser.add_argument("--phone", default=os.environ.get("TSZ_ADMIN_PHONE"))
    parser.add_argument("--password", default=os.environ.get("TSZ_ADMIN_PASSWORD"))
    parser.add_argument("--code", default=os.environ.get("TSZ_ADMIN_OTP_CODE", DEFAULT_OTP_CODE))
    parser.add_argument(
        "--token",
        default=os.environ.get("TSZ_ADMIN_TOKEN"),
        help="直接用现成的 admin access token，跳过登录",
    )
    parser.add_argument(
        "--no-cache",
        action="store_true",
        help=f"不读写 {SESSION_CACHE_DIR} 里的会话缓存（每次都重新登录，会消耗验证码额度）",
    )
    parser.add_argument("--pos", default=DEFAULT_POS, help=f"默认基本词性（默认 {DEFAULT_POS}）")
    parser.add_argument("--sub-pos", default=None, help="默认细分词性（默认取该词性目录里的第一个）")
    parser.add_argument("--level", default=DEFAULT_LEVEL, help=f"默认 CEFR 等级（默认 {DEFAULT_LEVEL}）")
    parser.add_argument("--frequency", default=DEFAULT_FREQUENCY, help="默认词义词频 0–100")
    parser.add_argument("--grammar", default=DEFAULT_GRAMMAR, help="默认语法结构文本")
    parser.add_argument("--timeout", type=float, default=30.0)
    args = parser.parse_args()

    if not args.words and not args.file:
        parser.error("至少给一个 单词:释义，或用 --file 指定词条 JSON")

    try:
        raw_specs = [parse_positional(value, args) for value in args.words]
        if args.file:
            raw_specs.extend(load_file(args.file))
    except (SpecError, OSError, json.JSONDecodeError) as error:
        print(f"输入有误：{error}", file=sys.stderr)
        return 2

    client = AdminClient(args.base_url, timeout=args.timeout)
    try:
        source = client.authenticate(
            args.phone,
            args.password,
            args.code,
            token=args.token,
            use_cache=not args.no_cache,
        )
        print(f"→ {args.base_url}：{source}", flush=True)
        catalog = client.catalog()
    except SpecError as error:
        print(f"凭据不足：{error}", file=sys.stderr)
        return 2
    except ApiError as error:
        print(f"登录或拉取词性目录失败：{error}", file=sys.stderr)
        if error.status == 401:
            print(
                "提示：验证码是一次性的，且后端有 60 秒冷却 / 每天 10 次上限；"
                "冷却期内重新登录会拿到 401。等一分钟再试，或用 --token 传一个现成的 access token。",
                file=sys.stderr,
            )
        return 1

    failures = 0
    for raw in raw_specs:
        surface = raw.get("surface", "?") if isinstance(raw, dict) else "?"
        try:
            spec = normalize_spec(raw, catalog, args)
            for pos in spec["pos"]:
                pos["pos_id"] = new_id()
            result = publish_entry(client, spec)
        except (SpecError, ApiError) as error:
            failures += 1
            print(f"✗ {surface} 发布失败：{error}", file=sys.stderr)
            if isinstance(error, ApiError) and "surface-match-snapshots" in str(error):
                print(
                    "  提示：同词面候选超过一页（20 条）才会走这个翻页接口，而确认令牌只在末页签发。"
                    "翻页失败就拿不到令牌，先清掉库里重复的同词面词条再试。",
                    file=sys.stderr,
                )
            continue
        print(
            f"✓ {result['surface']} → {result['entry_id']}"
            f"（{result['status']}，published_revision={result['published_revision']}）",
            flush=True,
        )

    total = len(raw_specs)
    print(f"完成：{total - failures}/{total} 个词条已发布")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
