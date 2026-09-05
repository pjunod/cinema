from __future__ import annotations

import json
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/validate-docker-startup-budget"
CHECKER = runpy.run_path(str(SCRIPT))
BudgetError = CHECKER["BudgetError"]


def compose_document(
    *,
    start_period: str = "5m0s",
    environment: dict[str, str] | None = None,
    volumes: list[dict[str, str]] | None = None,
    command: list[str] | None = None,
) -> dict[str, object]:
    return {
        "services": {
            "plurxd": {
                "environment": environment or {},
                "healthcheck": {"start_period": start_period},
                "volumes": volumes or [],
                "command": command,
            }
        }
    }


class DockerStartupBudgetTests(unittest.TestCase):
    def test_product_defaults_fit_the_compose_health_grace(self):
        budget = CHECKER["validate_document"](compose_document())

        self.assertEqual(budget.snapshot_seconds, 120)
        self.assertEqual(budget.startup_allowance_seconds, 135)
        self.assertEqual(budget.required_seconds, 255)
        self.assertEqual(int(budget.health_seconds), 300)
        self.assertEqual(budget.snapshot_source, "built-in default")

    def test_resolved_environment_timeout_fails_closed_before_deployment(self):
        document = compose_document(
            environment={
                "PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS": "600",
            }
        )

        with self.assertRaisesRegex(BudgetError, r"600s.*requires at least 735s"):
            CHECKER["validate_document"](document)

    def test_environment_timeout_parsing_matches_the_server(self):
        document = compose_document(
            environment={
                "PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS": " 120 ",
            }
        )

        with self.assertRaisesRegex(BudgetError, r"must be an integer"):
            CHECKER["validate_document"](document)

    def test_resolved_environment_overrides_a_bind_mounted_toml(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "plurx.toml"
            config.write_text(
                "[cluster]\ninstall_snapshot_timeout_secs = 600\n",
                encoding="utf-8",
            )
            document = compose_document(
                environment={
                    "PLURX_CONFIG": "/etc/plurx/plurx.toml",
                    "PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS": "120",
                },
                volumes=[
                    {
                        "type": "bind",
                        "source": str(config),
                        "target": "/etc/plurx/plurx.toml",
                    }
                ],
            )

            budget = CHECKER["validate_document"](document)

        self.assertEqual(budget.snapshot_seconds, 120)
        self.assertEqual(
            budget.snapshot_source,
            "PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS",
        )

    def test_bind_mounted_production_toml_is_included_in_the_budget(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "plurx.toml"
            config.write_text(
                "[cluster]\ninstall_snapshot_timeout_secs = 600\n",
                encoding="utf-8",
            )
            document = compose_document(
                start_period="12m15s",
                environment={"PLURX_CONFIG": "/etc/plurx/plurx.toml"},
                volumes=[
                    {
                        "type": "bind",
                        "source": directory,
                        "target": "/etc/plurx",
                    }
                ],
            )

            budget = CHECKER["validate_document"](document)

        self.assertEqual(budget.snapshot_seconds, 600)
        self.assertEqual(budget.required_seconds, 735)
        self.assertEqual(int(budget.health_seconds), 735)
        self.assertEqual(budget.snapshot_source, str(config))

    def test_cli_config_path_takes_precedence_over_config_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            env_config = Path(directory) / "env.toml"
            cli_config = Path(directory) / "cli.toml"
            env_config.write_text(
                "[cluster]\ninstall_snapshot_timeout_secs = 120\n",
                encoding="utf-8",
            )
            cli_config.write_text(
                "[cluster]\ninstall_snapshot_timeout_secs = 600\n",
                encoding="utf-8",
            )
            document = compose_document(
                start_period="12m15s",
                environment={"PLURX_CONFIG": "/etc/plurx/env.toml"},
                volumes=[
                    {
                        "type": "bind",
                        "source": directory,
                        "target": "/etc/plurx",
                    }
                ],
                command=["run", "--config", "/etc/plurx/cli.toml"],
            )

            budget = CHECKER["validate_document"](document)

        self.assertEqual(budget.snapshot_seconds, 600)
        self.assertEqual(budget.snapshot_source, str(cli_config))

    def test_opaque_config_mount_conservatively_uses_the_source_maximum(self):
        document = compose_document(
            environment={"PLURX_CONFIG": "/etc/plurx/plurx.toml"},
            volumes=[
                {
                    "type": "volume",
                    "source": "plurx-config",
                    "target": "/etc/plurx",
                }
            ],
        )

        with self.assertRaisesRegex(
            BudgetError, r"3600s.*opaque config mount.*requires at least 3735s"
        ):
            CHECKER["validate_document"](document)

    def test_more_specific_opaque_mount_masks_a_readable_parent_bind(self):
        document = compose_document(
            environment={"PLURX_CONFIG": "/etc/plurx/plurx.toml"},
            volumes=[
                {"type": "bind", "source": "/host/etc", "target": "/etc"},
                {
                    "type": "volume",
                    "source": "plurx-config",
                    "target": "/etc/plurx",
                },
            ],
        )

        with self.assertRaisesRegex(BudgetError, r"opaque config mount"):
            CHECKER["validate_document"](document)

    def test_unmapped_explicit_config_is_not_assumed_safe(self):
        document = compose_document(
            environment={"PLURX_CONFIG": "/run/config/plurx.toml"}
        )

        with self.assertRaisesRegex(BudgetError, r"is not backed.*set PLURX_CLUSTER"):
            CHECKER["validate_document"](document)

    def test_duration_parser_accepts_compose_composites_and_rejects_junk(self):
        self.assertEqual(CHECKER["parse_duration"]("1h2m15s", "health"), 3735)
        self.assertEqual(
            CHECKER["display_seconds"](
                CHECKER["parse_duration"]("5m", "health")
            ),
            "300",
        )
        with self.assertRaisesRegex(BudgetError, "not a Docker duration"):
            CHECKER["parse_duration"]("65 minutes", "health")

    def test_compose_resolution_is_read_only_and_uses_the_deploy_directory(self):
        result = subprocess.CompletedProcess(
            args=[], returncode=0, stdout=json.dumps(compose_document()), stderr=""
        )
        with mock.patch.object(subprocess, "run", return_value=result) as run:
            document = CHECKER["resolved_compose_document"]()

        self.assertEqual(document, compose_document())
        run.assert_called_once_with(
            ["docker", "compose", "config", "--format", "json"],
            cwd=ROOT / "deploy",
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def test_cli_failure_does_not_echo_resolved_environment_secrets(self):
        secret = "do-not-print-this-secret"
        document = compose_document(
            start_period="10s",
            environment={"UNRELATED_SECRET": secret},
        )
        with tempfile.TemporaryDirectory() as directory:
            resolved = Path(directory) / "compose.json"
            resolved.write_text(json.dumps(document), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--compose-json", str(resolved)],
                check=False,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )

        self.assertEqual(result.returncode, 2)
        self.assertIn("nothing was changed", result.stderr)
        self.assertNotIn(secret, result.stdout + result.stderr)

    def test_an_unset_health_grace_derives_the_period_the_deploy_needs(self):
        document = compose_document(
            environment={"PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS": "1200"}
        )

        value, explanation = CHECKER["start_period_for_deployment"](document)

        self.assertEqual(value, "1335s")
        self.assertIn("derived", explanation)
        self.assertIn("1200s", explanation)

    def test_a_deadline_only_a_production_toml_knows_still_derives_the_grace(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "plurx.toml"
            config.write_text(
                "[cluster]\ninstall_snapshot_timeout_secs = 1200\n",
                encoding="utf-8",
            )
            document = compose_document(
                environment={"PLURX_CONFIG": "/var/lib/plurx/plurx.toml"},
                volumes=[
                    {
                        "type": "bind",
                        "source": directory,
                        "target": "/var/lib/plurx",
                    }
                ],
            )
            value, explanation = CHECKER["start_period_for_deployment"](document)

        # The failure this closes: the deadline lives in a bind-mounted file
        # `.env` never mentions, so nothing paired the readiness grace to it
        # and the first report was a refused deploy on the host.
        self.assertEqual(value, "1335s")
        self.assertIn(str(config), explanation)

    def test_a_default_deployment_keeps_the_compose_default_untouched(self):
        value, _ = CHECKER["start_period_for_deployment"](compose_document())

        self.assertEqual(value, "300s")

    def test_a_grace_this_deployment_chose_is_left_as_written_and_refused(self):
        # Anything other than the tracked interpolation default was chosen by
        # somebody -- a shell variable, deploy/.env, or a literal pinned in an
        # override -- at Compose's own precedence. A deliberately short grace
        # reports a build that can never become ready, so it is refused by
        # name rather than raised past the operator who wrote it.
        document = compose_document(
            start_period="1m0s",
            environment={"PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS": "1200"},
        )

        value, explanation = CHECKER["start_period_for_deployment"](document)

        self.assertEqual(value, "60s")
        self.assertIn("chosen by this deployment", explanation)
        with self.assertRaisesRegex(BudgetError, r"60s.*requires at least 1335s"):
            CHECKER["validate_document"](document)

    def test_a_longer_grace_than_the_budget_needs_is_kept(self):
        document = compose_document(
            start_period="40m0s",
            environment={"PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS": "1200"},
        )

        value, explanation = CHECKER["start_period_for_deployment"](document)

        self.assertEqual(value, "2400s")
        self.assertIn("covers the budget", explanation)
        CHECKER["validate_document"](document)

    def test_the_tracked_default_is_read_from_compose_not_hard_coded(self):
        # Nothing here re-implements Compose's env-file or interpolation rules:
        # "did anybody choose?" is answered by comparing what Compose resolved
        # against the default this repository ships. A checker that disagreed
        # with Compose about `.env` would export a period that silently
        # outranks the operator's own file.
        self.assertEqual(
            CHECKER["tracked_default_health_seconds"](),
            CHECKER["parse_duration"]("5m", "expected tracked default"),
        )
        with tempfile.TemporaryDirectory() as directory:
            pinned = Path(directory) / "docker-compose.yml"
            pinned.write_text(
                "services:\n  plurxd:\n    healthcheck:\n"
                '      start_period: "90s"\n',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(BudgetError, "tracked default"):
                CHECKER["tracked_default_health_seconds"](pinned)

    def test_emit_start_period_prints_only_the_value_on_stdout(self):
        document = compose_document(
            environment={"PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS": "1200"}
        )
        with tempfile.TemporaryDirectory() as directory:
            resolved = Path(directory) / "compose.json"
            resolved.write_text(json.dumps(document), encoding="utf-8")
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--compose-json",
                    str(resolved),
                    "--emit-start-period",
                ],
                check=False,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )

        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "1335s")
        self.assertIn("start period", result.stderr)


if __name__ == "__main__":
    unittest.main()
