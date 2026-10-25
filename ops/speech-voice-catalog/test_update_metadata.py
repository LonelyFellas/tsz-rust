import copy
import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("update_metadata", Path(__file__).with_name("update_metadata.py"))
updater = importlib.util.module_from_spec(spec)
spec.loader.exec_module(updater)


class UpdateMetadataTests(unittest.TestCase):
    def setUp(self):
        self.snapshot = [{
            "provider_voice_id": "en-GB-RyanNeural",
            "locale": "en-GB",
            "gender": "male",
            "styles": ["chat", "cheerful"],
        }]
        self.voice = {
            "shortName": "en-GB-RyanNeural",
            "locale": "en-GB",
            "description": "Official description",
            "properties": {
                "Gender": "Male",
                "ReleaseScope": "PublicGA",
                "FrontendVoiceType": "Neural",
                "AgeGroups": "Adult",
                "VoiceRoleNames": "",
                "VoiceStyleNames": "chat,cheerful,sad,whispering,Default",
            },
            "voiceTags": [{"name": "modelSeries", "tags": ["Monolingual"]}],
        }
        self.checked_at = "2026-10-25T13:14:11Z"

    def test_existing_ids_and_capabilities_are_preserved_and_missing_tags_stay_unknown(self):
        original = copy.deepcopy(self.snapshot)
        extra = {"shortName": "en-AU-ExtraNeural"}
        result = updater.update_metadata(self.snapshot, [self.voice, extra], self.checked_at)
        self.assertEqual(self.snapshot, original)
        self.assertEqual(len(result), 1)
        without_metadata = {key: value for key, value in result[0].items() if key != "metadata"}
        self.assertEqual(without_metadata, original[0])
        metadata = result[0]["metadata"]
        self.assertEqual(metadata["roles"], [])
        self.assertIsNone(metadata["personalities"])
        self.assertIsNone(metadata["scenarios"])
        self.assertEqual(metadata["model"], "neural")
        self.assertEqual(metadata["source_url"], "https://speech.microsoft.com/portal/voicegallery")
        self.assertEqual(metadata["checked_at"], self.checked_at)

    def test_model_comes_from_provider_tag_and_missing_roles_are_unknown(self):
        self.voice["voiceTags"][0]["tags"] = ["Multilingual"]
        self.voice["properties"]["AgeGroups"] = "YoungAdult"
        self.voice["properties"]["Personality"] = "calm,knowledgable"
        del self.voice["properties"]["VoiceRoleNames"]
        metadata = updater.update_metadata(self.snapshot, [self.voice], self.checked_at)[0]["metadata"]
        self.assertEqual(metadata["model"], "multilingual")
        self.assertEqual(metadata["age_group"], "YoungAdult")
        self.assertEqual(metadata["personalities"], ["calm", "knowledgable"])
        self.assertIsNone(metadata["roles"])

    def test_missing_duplicate_and_incompatible_records_fail_without_mutating_input(self):
        original = copy.deepcopy(self.snapshot)
        for gallery in ([], [self.voice, self.voice]):
            with self.subTest(gallery=gallery), self.assertRaises(ValueError):
                updater.update_metadata(self.snapshot, gallery, self.checked_at)
        for field, value in (("ReleaseScope", "PublicPreview"), ("FrontendVoiceType", "NeuralHD"), ("Gender", "Female"), ("AgeGroups", "GuessedAge")):
            voice = copy.deepcopy(self.voice)
            voice["properties"][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                updater.update_metadata(self.snapshot, [voice], self.checked_at)
        self.assertEqual(self.snapshot, original)
        with self.assertRaises(ValueError):
            updater.update_metadata(self.snapshot, [self.voice], "2026-10-25T13:14:11")


if __name__ == "__main__":
    unittest.main()
