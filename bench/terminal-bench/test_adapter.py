"""Adapter regressions; synthetic usage and fake Docker, no model requests."""

import asyncio
import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

from harbor.models.agent.context import AgentContext

from canter_agent import Canter, NonZeroAgentExitCodeError


def usage(cached=2, uncached=3, output=4, usd=0.1, unknown=False):
    return dict(cachedInputTokens=cached, uncachedInputTokens=uncached,
                outputTokens=output, cost=dict(usd=usd, unknown=unknown))


class AdapterTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.agent = object.__new__(Canter)
        self.agent.logs_dir = Path(self.temp.name)
        self.agent.model_name = "test/model"
        self.agent.options = SimpleNamespace(reasoning_effort="high")
        self.agent._key = "test-only-credential"
        self.agent._errors = []
        self.agent._read_stats = AsyncMock(return_value={"subAgents": []})
        self.context = AgentContext()
        self.write("collection.json", {"credential_redactions": 0})

    def write(self, name, data):
        (self.agent.logs_dir / name).write_text(json.dumps(data))

    def populate(self, parent, children=(), aggregate=None):
        self.write("stats.json", {
            "sessionId": "fixture", "overview": {
                "usage": {"total": parent} if parent is not None else None,
                "usageWithSubAgents": aggregate,
            }, "subAgents": [{"usage": child} for child in children],
        })
        self.agent.populate_context_post_run(self.context)

    def test_null_child_does_not_drop_other_child_usage(self):
        self.populate(usage(), [usage(10, 20, 30, 0.2), None])
        self.assertEqual(self.context.n_input_tokens, 35)
        self.assertEqual(self.context.n_cache_tokens, 12)
        self.assertEqual(self.context.n_output_tokens, 34)
        self.assertAlmostEqual(self.context.cost_usd, 0.3)
        self.assertEqual(self.context.metadata["sub_agent_count"], 2)
        self.assertEqual(self.context.metadata["sub_agent_empty_usage_count"], 1)

    def test_unknown_child_cost_remains_unknown(self):
        self.populate(usage(), [usage(unknown=True), None])
        self.assertIsNone(self.context.cost_usd)
        self.assertEqual(self.context.n_input_tokens, 10)

    def test_empty_usage_is_zero_tokens_unknown_dollars(self):
        self.populate(None, [None])
        self.assertEqual(self.context.n_input_tokens, 0)
        self.assertEqual(self.context.n_output_tokens, 0)
        self.assertIsNone(self.context.cost_usd)

    def test_aggregate_not_double_counted(self):
        self.populate(usage(), [usage()], aggregate=usage(4, 6, 8, 0.2))
        self.assertEqual(self.context.n_input_tokens, 10)
        self.assertEqual(self.context.n_output_tokens, 8)

    def test_malformed_stats_does_not_abort_grading(self):
        (self.agent.logs_dir / "stats.json").write_text("not json")
        self.agent.populate_context_post_run(self.context)
        self.assertIsNone(self.context.n_input_tokens)
        self.assertIsNone(self.context.cost_usd)
        self.assertTrue(self.context.metadata["errors"])

    async def test_collection_failure_still_removes_config_and_keeps_server(self):
        order = []
        async def stop(_):
            order.append("stop")
        async def collect(_):
            order.append("collect")
            raise OSError("synthetic failure")
        async def remove(_):
            order.append("remove")
        self.agent._exec_private = AsyncMock(return_value=SimpleNamespace(return_code=0))
        self.agent._stop_processes = stop
        self.agent._collect = collect
        self.agent._remove_config = remove
        await self.agent.run("fixture", None, self.context)
        self.agent.populate_context_post_run(self.context)
        self.assertEqual(order, ["collect", "remove"])
        self.assertTrue(self.context.metadata["errors"])
        self.assertIsNone(self.context.n_input_tokens)

    async def test_cancelled_error_survives_cleanup_failure(self):
        self.agent._exec_private = AsyncMock(side_effect=asyncio.CancelledError)
        self.agent._stop_processes = AsyncMock()
        self.agent._collect = AsyncMock(side_effect=OSError("collect"))
        self.agent._remove_config = AsyncMock(side_effect=OSError("remove"))
        with self.assertRaises(asyncio.CancelledError):
            await self.agent.run("fixture", None, self.context)
        self.agent._stop_processes.assert_awaited_once_with(None, stop_server=False)

    async def test_remove_failure_still_raises_without_cancellation(self):
        self.agent._exec_private = AsyncMock(return_value=SimpleNamespace(return_code=0))
        self.agent._stop_processes = AsyncMock()
        self.agent._collect = AsyncMock()
        self.agent._remove_config = AsyncMock(side_effect=OSError("remove"))
        with self.assertRaisesRegex(OSError, "remove"):
            await self.agent.run("fixture", None, self.context)

    async def test_running_child_stops_server_then_refreshes_usage(self):
        order = []
        async def record(phase):
            order.append(phase)
        async def stats(_):
            order.append("stats")
            return {"subAgents": [{"status": "running"}]}
        self.agent._exec_private = AsyncMock(return_value=SimpleNamespace(return_code=0))
        self.agent._read_stats = stats
        self.agent._stop_processes = lambda _, *, stop_server: record(f"stop:{stop_server}")
        self.agent._collect = lambda _: record("collect")
        self.agent._remove_config = lambda _: record("remove")
        await self.agent.run("fixture", None, self.context)
        self.assertEqual(order, ["stats", "stop:True", "stats", "collect", "remove", "stop:True"])
        self.populate(usage())
        self.assertTrue(self.context.metadata["server_stopped_for_sub_agents"])
        self.assertEqual(self.context.metadata["server_stop_reason"], "为停子 agent 停了服务端")

    async def test_normal_completion_without_running_children_keeps_server(self):
        self.agent._exec_private = AsyncMock(return_value=SimpleNamespace(return_code=0))
        self.agent._read_stats = AsyncMock(return_value={"subAgents": [
            {"status": "completed"}, {"status": "interrupted"},
        ]})
        self.agent._stop_processes = AsyncMock()
        self.agent._collect = AsyncMock()
        self.agent._remove_config = AsyncMock()
        await self.agent.run("fixture", None, self.context)
        self.agent._stop_processes.assert_not_awaited()
        self.agent._read_stats.assert_awaited_once()
        self.agent._collect.assert_awaited_once()
        self.agent._remove_config.assert_awaited_once()
        self.assertFalse(self.agent._server_stopped_for_sub_agents)

    async def test_repeated_cancellation_waits_for_cleanup(self):
        started, release = asyncio.Event(), asyncio.Event()
        async def collect(_):
            started.set()
            await release.wait()
        self.agent._exec_private = AsyncMock(return_value=SimpleNamespace(return_code=0))
        self.agent._stop_processes = AsyncMock()
        self.agent._collect = collect
        self.agent._remove_config = AsyncMock()
        run = asyncio.create_task(self.agent.run("fixture", None, self.context))
        await started.wait()
        run.cancel()
        await asyncio.sleep(0)
        run.cancel()
        await asyncio.sleep(0)
        self.assertFalse(run.done())
        release.set()
        with self.assertRaises(asyncio.CancelledError):
            await run
        self.agent._remove_config.assert_awaited_once()
        self.agent._stop_processes.assert_not_awaited()

    async def test_collection_cancellation_is_preserved(self):
        self.agent._exec_private = AsyncMock(return_value=SimpleNamespace(return_code=0))
        self.agent._stop_processes = AsyncMock()
        self.agent._collect = AsyncMock(side_effect=asyncio.CancelledError)
        self.agent._remove_config = AsyncMock()
        with self.assertRaises(asyncio.CancelledError):
            await self.agent.run("fixture", None, self.context)
        self.agent._remove_config.assert_awaited_once()
        self.agent._stop_processes.assert_not_awaited()

    async def test_nonzero_exec_still_stops_and_collects(self):
        self.agent._exec_private = AsyncMock(return_value=SimpleNamespace(return_code=7))
        self.agent._stop_processes = AsyncMock()
        self.agent._collect = AsyncMock()
        self.agent._remove_config = AsyncMock()
        with self.assertRaises(NonZeroAgentExitCodeError):
            await self.agent.run("fixture", None, self.context)
        self.agent._collect.assert_awaited_once()
        self.agent._remove_config.assert_awaited_once()
        self.agent._stop_processes.assert_awaited_once_with(None, stop_server=False)

    async def test_stats_failure_does_not_stop_background_services(self):
        self.agent._exec_private = AsyncMock(return_value=SimpleNamespace(return_code=0))
        self.agent._read_stats = AsyncMock(side_effect=ValueError("bad stats"))
        self.agent._stop_processes = AsyncMock()
        self.agent._collect = AsyncMock()
        self.agent._remove_config = AsyncMock()
        await self.agent.run("fixture", None, self.context)
        self.agent.populate_context_post_run(self.context)
        self.agent._stop_processes.assert_not_awaited()
        self.agent._remove_config.assert_awaited_once()
        self.assertTrue(self.context.metadata["errors"])
        self.assertIsNone(self.context.n_input_tokens)

    async def test_read_stats_decodes_private_memory_payload(self):
        import base64
        report = {"subAgents": [{"status": "running"}, {"status": "completed"}]}
        encoded = base64.b64encode(json.dumps(report).encode()).decode()
        self.agent._exec_private = AsyncMock(return_value=SimpleNamespace(
            return_code=0, stdout="warning\nCANTER_STATS_BEGIN\n"
            + encoded + "\nCANTER_STATS_END\nwarning\n"))
        self.assertEqual(await Canter._read_stats(self.agent, None), report)
        self.assertFalse((self.agent.logs_dir / "stats.json").exists())

    async def test_archive_ignores_docker_stderr_outside_frame(self):
        import base64
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w") as tar:
            info = tarfile.TarInfo("stats.exit")
            info.size = 1
            tar.addfile(info, io.BytesIO(b"0"))
        encoded = base64.b64encode(buffer.getvalue()).decode()
        self.agent._exec_private = AsyncMock(return_value=SimpleNamespace(
            return_code=0, stdout="compose warning\nCANTER_ARCHIVE_BEGIN\n"
            + encoded + "\nCANTER_ARCHIVE_END\ncompose warning\n"))
        await self.agent._collect(None)
        self.assertEqual((self.agent.logs_dir / "stats.exit").read_text(), "0")


if __name__ == "__main__":
    unittest.main()
