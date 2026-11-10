import hashlib
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]


class AcknowledgementsTests(unittest.TestCase):
    def test_catalogue_matches_lockfile_and_contains_displayable_notices(self):
        data = json.loads((ROOT / 'assets/acknowledgements.json').read_text())
        self.assertEqual(data['cargo_lock_sha256'], hashlib.sha256((ROOT / 'Cargo.lock').read_bytes()).hexdigest(),
                         'Regenerate acknowledgements after changing Cargo.lock')
        self.assertEqual(data['generator'], 'cargo-about')
        self.assertGreater(len(data['entries']), 100)
        names = {entry['name'] for entry in data['entries']}
        self.assertTrue({'kureha-code', 'scarlet-ui', 'sgfx', 'syntect', 'Noto Sans CJK JP', 'Monokai palette'} <= names)
        for entry in data['entries']:
            self.assertTrue(entry['license'], entry['name'])
            self.assertTrue(entry['texts'], entry['name'])
            for index in entry['texts']:
                self.assertTrue(data['texts'][index].strip(), entry['name'])
        serialized = json.dumps(data)
        self.assertNotIn('/Users/', serialized)
        self.assertNotIn('/nix/store/', serialized)


if __name__ == '__main__':
    unittest.main()
