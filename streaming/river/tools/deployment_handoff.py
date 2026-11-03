"""Install deployment-agent guidance without replacing installation settings."""
from pathlib import Path
import shutil

HANDOFF_FILES = (
    "AGENTS.md", "ORGANIZATIONAL-DEPLOYMENT.txt", "organizational-test.env.example",
    "read-policy-adaptive.example.json", "compose.organizational-test.override.example.yaml",
    "compose.cpu-capacity.override.example.yaml",
    ".cursor/rules/river-deployment.mdc",
    "POD-32CORE-TESTING.md", "pod-32core.env.example",
    "read-policy-nearby.patch.json", "apply-nearby-read-policy.py",
    "apply-serial-transport.py", "SERIAL-AUTO-PROFILE.md",
    "ORGANIZATIONAL-S3-PROFILE.md", "STREAMER-COMPATIBILITY.md",
)
BANNER = ("EXISTING DEPLOYMENT: Read AGENTS.md and ORGANIZATIONAL-DEPLOYMENT.txt first. "
          "Preserve existing S3 connections, configuration, environment and config/cache volumes. "
          "Examples are fresh-install presets, not replacements for existing settings.")

def install_handoff(package: Path) -> None:
    source = Path(__file__).resolve().parents[1] / "deploy/portable/agent-handoff"
    package = package.resolve()
    for relative in HANDOFF_FILES:
        origin = source / relative
        target = package / relative
        target.resolve().relative_to(package)
        if target.exists() and target.read_bytes() != origin.read_bytes():
            raise RuntimeError(f"Refusing to overwrite different existing handoff file: {relative}")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(origin, target)
    for name in ("README.md", "DEPLOY.txt"):
        target = package / name
        if target.exists():
            text = target.read_text(encoding="utf-8")
            if not text.startswith(BANNER):
                target.write_text(BANNER + "\n\n" + text, encoding="utf-8", newline="\n")
