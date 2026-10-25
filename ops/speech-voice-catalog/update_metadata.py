#!/usr/bin/env python3
"""Merge an offline Azure Voice Gallery response into the existing voice snapshot."""

import argparse
import datetime
import json
import re
from pathlib import Path

SOURCE_URL = "https://speech.microsoft.com/portal/voicegallery"
SNAPSHOT = Path(__file__).with_name("catalog-snapshot.json")


def metadata_for(voice, entry, checked_at):
    properties = voice["properties"]
    if (
        voice["locale"] != entry["locale"]
        or properties["Gender"].lower() != entry["gender"]
        or properties["ReleaseScope"] != "PublicGA"
        or properties["FrontendVoiceType"] != "Neural"
    ):
        raise ValueError(f"Incompatible Gallery voice: {entry['provider_voice_id']}")
    series = next(
        (tag["tags"] for tag in voice["voiceTags"] if tag["name"] == "modelSeries"),
        None,
    )
    if series not in (["Monolingual"], ["Multilingual"], ["HDFlash"]):
        raise ValueError(f"Unknown model series: {entry['provider_voice_id']}")
    age_group = properties.get("AgeGroups") or None
    if age_group not in (None, "Adult", "YoungAdult", "Child", "Senior"):
        raise ValueError(f"Unknown age group: {entry['provider_voice_id']}")

    def tags(field):
        value = properties.get(field)
        return [tag.strip() for tag in value.split(",") if tag.strip()] if value else None

    return {
        "provider_voice_id": entry["provider_voice_id"],
        "model": {"Monolingual": "neural", "Multilingual": "multilingual", "HDFlash": "hd_flash"}[series[0]],
        "age_group": age_group,
        "description": voice.get("description") or None,
        "personalities": tags("Personality"),
        "scenarios": tags("TailoredScenarios"),
        "roles": (tags("VoiceRoleNames") or []) if "VoiceRoleNames" in properties else None,
        "source_url": SOURCE_URL,
        "checked_at": checked_at,
    }


def update_metadata(snapshot, gallery, checked_at):
    timestamp = datetime.datetime.fromisoformat(checked_at.replace("Z", "+00:00"))
    if timestamp.utcoffset() is None:
        raise ValueError("checked_at must include a timezone")
    checked_at = timestamp.astimezone(datetime.timezone.utc).isoformat().replace("+00:00", "Z")
    ids = {entry["provider_voice_id"] for entry in snapshot}
    selected = {}
    for voice in gallery:
        voice_id = voice.get("shortName")
        if voice_id in ids:
            if voice_id in selected:
                raise ValueError(f"Duplicate Gallery voice: {voice_id}")
            selected[voice_id] = voice
    if ids != selected.keys():
        raise ValueError(f"Missing Gallery voices: {sorted(ids - selected.keys())}")
    result = []
    for entry in snapshot:
        if not re.fullmatch(r"en-[A-Z]{2}", entry["locale"]):
            raise ValueError("Only native English locales are supported")
        metadata = metadata_for(selected[entry["provider_voice_id"]], entry, checked_at)
        result.append({**entry, "metadata": metadata})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True, help="Saved Gallery JSON response")
    parser.add_argument("--checked-at", required=True, help="Actual query time, including timezone")
    args = parser.parse_args()
    snapshot = json.loads(SNAPSHOT.read_text(encoding="utf-8"))
    gallery = json.loads(args.input.read_text(encoding="utf-8"))
    updated = update_metadata(snapshot, gallery, args.checked_at)
    SNAPSHOT.write_text(json.dumps(updated, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"Updated metadata for {len(updated)} existing voices; synthesis capabilities unchanged")


if __name__ == "__main__":
    main()
