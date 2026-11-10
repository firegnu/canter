"""Canter's installed-agent adapter for Harbor 0.25.0, using local Docker.

References (no copied implementation): Harbor 0.25.0 BaseInstalledAgent,
Pi, DockerEnvironment and UnixOps' stdin-based tar upload.
"""

import base64
import io
import json
import os
import shlex
import tarfile
from pathlib import Path, PurePosixPath
from typing import Any

from harbor.agents.capabilities import AgentCapabilities
from harbor.agents.installed.base import BaseInstalledAgent, NonZeroAgentExitCodeError
from harbor.agents.options import InstalledAgentOptions
from harbor.constants import MAIN_SERVICE_NAME
from harbor.environments.docker.docker import DockerEnvironment
from harbor.models.agent.context import AgentContext, ModelUsage


class CanterOptions(InstalledAgentOptions):
    binary_path: str
    endpoint: str
    key_env: str
    model_settings: dict[str, Any]


class Canter(BaseInstalledAgent):
    capabilities = AgentCapabilities(reasoning_effort=True)
    options_model = CanterOptions
    options: CanterOptions

    @staticmethod
    def name() -> str:
        return "canter"

    @classmethod
    def preflight(cls, kwargs=None, env=None) -> None:
        options = cls.parse_options(kwargs, env)
        if not Path(options.binary_path).is_file():
            raise ValueError("binary_path must point to the musl canter executable")
        if not os.environ.get(options.key_env):
            raise ValueError(f"Set the host environment variable {options.key_env}")
        if options.key_env in (env or {}):
            raise ValueError("Pass only key_env, not the credential through --ae")
        if "context_window" not in options.model_settings:
            raise ValueError("model_settings must include the model's context_window")

    def get_version_command(self) -> str:
        return "/usr/local/bin/canter --version"

    async def install(self, environment) -> None:
        await self.ensure_system_dependencies(environment, ("ca_certificates",))
        await environment.upload_file(
            Path(self.options.binary_path), "/usr/local/bin/canter"
        )
        await self.exec_as_root(environment, "chmod 755 /usr/local/bin/canter")

    async def setup(self, environment) -> None:
        if not isinstance(environment, DockerEnvironment):
            raise ValueError("This adapter requires Harbor's local Docker environment")
        await super().setup(environment)
        # The released 0.25.0 has no configure hook; call ours after installation.
        await self.configure(environment)

    async def _exec_private(self, environment, command, *, stdin=None):
        # Do not use environment.exec: its streaming callback can persist output
        # before we redact it. The pinned Docker helper accepts an in-memory pipe
        # and, with check=False and no callback, never logs stdin or output.
        args = ["exec", "-T"]
        if environment.task_env_config.workdir:
            args += ["-w", environment.task_env_config.workdir]
        if environment.default_user is not None:
            args += ["-u", str(environment.default_user)]
        args += [MAIN_SERVICE_NAME, "sh", "-c", command]
        return await environment._run_docker_compose_command(
            args, check=False, stdin_data=stdin
        )

    async def configure(self, environment) -> None:
        provider, separator, model = (self.model_name or "").partition("/")
        if not separator or not provider or not model:
            raise ValueError("Use --model provider/model")
        self._key = os.environ[self.options.key_env]
        settings = dict(self.options.model_settings)
        if self.options.reasoning_effort is not None:
            settings["default_reasoning_effort"] = self.options.reasoning_effort
        config = {
            "model": self.model_name,
            "providers": {
                provider: {
                    "base_url": self.options.endpoint,
                    "api_key": self._key,
                    "models": {model: settings},
                }
            },
        }
        result = await self._exec_private(
            environment,
            'set -eu; umask 077; mkdir -p "$HOME/.canter" /tmp/canter-harbor; '
            'cat > "$HOME/.canter/config.json"; '
            'chmod 600 "$HOME/.canter/config.json"',
            stdin=json.dumps(config).encode(),
        )
        if result.return_code:
            await self._remove_config(environment)
            raise RuntimeError("Could not create the container's canter configuration")

    async def _remove_config(self, environment) -> None:
        result = await self._exec_private(
            environment, 'rm -f "$HOME/.canter/config.json"'
        )
        if result.return_code:
            raise RuntimeError("Could not remove the container's canter configuration")

    async def run(self, instruction, environment, context: AgentContext) -> None:
        try:
            result = await self._exec_private(
                environment,
                "/usr/local/bin/canter exec --yolo -- "
                + shlex.quote(instruction)
                + " </dev/null > /tmp/canter-harbor/stdout.txt 2>&1",
            )
        finally:
            try:
                await self._collect(environment)
            finally:
                await self._remove_config(environment)
        if result.return_code:
            raise NonZeroAgentExitCodeError(
                f"canter exec exited {result.return_code}; see agent/stdout.txt"
            )

    async def _collect(self, environment) -> None:
        result = await self._exec_private(
            environment,
            "/usr/local/bin/canter stats --last --json "
            "> /tmp/canter-harbor/stats.json 2> /tmp/canter-harbor/stats.stderr; "
            "status=$?; printf '%s' \"$status\" > /tmp/canter-harbor/stats.exit; "
            'if [ -d "$HOME/.canter/sessions" ]; then '
            'cp -R "$HOME/.canter/sessions" /tmp/canter-harbor/sessions; fi; '
            "tar -C /tmp/canter-harbor -cf /tmp/canter-harbor.tar . && "
            "base64 /tmp/canter-harbor.tar",
        )
        if result.return_code:
            raise RuntimeError("Could not collect canter stats and sessions")
        # Download through memory, then redact before any host write. Never
        # archive config.json, server state, or the whole ~/.canter directory.
        archive = base64.b64decode(result.stdout or "")
        redactions = 0
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            for member in tar:
                if not member.isfile():
                    continue
                name = PurePosixPath(member.name)
                if name.is_absolute() or ".." in name.parts:
                    raise RuntimeError("Unexpected path in canter log archive")
                content = tar.extractfile(member).read()
                redactions += content.count(self._key.encode())
                content = content.replace(self._key.encode(), b"[REDACTED]")
                path = self.logs_dir / str(name).replace(self._key, "[REDACTED]")
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
        (self.logs_dir / "collection.json").write_text(
            json.dumps({"credential_redactions": redactions}) + "\n"
        )
        if (self.logs_dir / "stats.exit").read_text() != "0":
            raise RuntimeError("canter stats failed; see agent/stats.stderr")

    def populate_context_post_run(self, context: AgentContext) -> None:
        report = json.loads((self.logs_dir / "stats.json").read_text())
        overview = report["overview"]
        usage = overview.get("usageWithSubAgents")
        if usage is None:
            usage = overview["usage"]["total"]
        context.n_cache_tokens = usage["cachedInputTokens"]
        context.n_input_tokens = (
            usage["cachedInputTokens"] + usage["uncachedInputTokens"]
        )
        context.n_output_tokens = usage["outputTokens"]
        cost = usage["cost"]
        context.cost_usd = None if cost["unknown"] else cost["usd"]
        context.model_usage = {
            self.model_name: ModelUsage(
                n_input_tokens=context.n_input_tokens,
                n_cache_tokens=context.n_cache_tokens,
                n_output_tokens=context.n_output_tokens,
                cost_usd=context.cost_usd,
            )
        }
        context.metadata = {
            "canter_session_id": report["sessionId"],
            "reasoning_effort": self.options.reasoning_effort,
            "credential_redactions": json.loads(
                (self.logs_dir / "collection.json").read_text()
            )["credential_redactions"],
        }
