"""pytest glue for the black-box tests: on a failure, show the tail of every log of the
processes still running (the server under test and the mocks)."""
import pytest

import harness


def pytest_report_header(config):
    return [f"wranglarr under test: {' '.join(harness.WRANGLARR_CMD)}",
            f"setup wizard under test: {' '.join(harness.SETUP_CMD)}"]


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item, call):
    outcome = yield
    rep = outcome.get_result()
    if rep.failed and call.when == "call":
        for p in list(harness.LIVE):
            rep.sections.append((f"log of {p.name} ({p.log})", p.tail(60)))


def pytest_sessionfinish(session, exitstatus):
    """A fixture that failed half-way never reached its teardown: stop what it started."""
    for p in list(harness.LIVE):
        p.stop()
