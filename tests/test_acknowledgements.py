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

    def test_asset_license_texts_and_tabler_attribution_are_included(self):
        data = json.loads((ROOT / 'assets/acknowledgements.json').read_text())
        notices = {text.strip() for text in data['texts']}
        for path in (ROOT / 'assets/themes').glob('*.LICENSE'):
            self.assertIn(path.read_text().strip(), notices, str(path))
        for path in (ROOT / 'assets/fonts').glob('*.txt'):
            self.assertIn(path.read_text().strip(), notices, str(path))
        gruvbox = next(entry for entry in data['entries'] if entry['name'] == 'Gruvbox palette')
        self.assertEqual(gruvbox['license'], 'MIT')
        self.assertIn('ef8864bb42bf244f0295d1c5a403b27e3d139695', gruvbox['source'])
        body = '\n'.join(data['texts'][index] for index in gruvbox['texts'])
        self.assertIn('Pavel Pertsev', body)
        self.assertIn('Permission is hereby granted', body)
        self.assertIn('not an upstream LICENSE file', body)
        tabler = next(entry for entry in data['entries'] if entry['name'] == 'scarlet-ui-icons-tabler')
        self.assertIn('Paweł Kuna', '\n'.join(data['texts'][index] for index in tabler['texts']))


if __name__ == '__main__':
    unittest.main()
