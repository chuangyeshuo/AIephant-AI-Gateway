import importlib.util
import sys
import unittest
from pathlib import Path


PROVIDER_MOCK_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PROVIDER_MOCK_DIR))

import manage_provider_mocks


def load_module(module_path: Path, module_name: str):
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class AnthropicPortTest(unittest.TestCase):
    def test_anthropic_mock_spec_uses_8023(self) -> None:
        spec = next(item for item in manage_provider_mocks.MOCK_SPECS if item.name == "anthropic")
        self.assertEqual(spec.port, 8023)
        self.assertEqual(spec.script_path.name, "mock_anthropic_8023.py")

    def test_anthropic_script_default_port_uses_8023(self) -> None:
        module = load_module(
            PROVIDER_MOCK_DIR / "mock_anthropic_8023.py",
            "mock_anthropic_8023",
        )
        self.assertEqual(module.CONFIG.default_port, 8023)


if __name__ == "__main__":
    unittest.main()
