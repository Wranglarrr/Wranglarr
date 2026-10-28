#!/usr/bin/env python3
"""Run the black-box tests: `python tests/run.py [pytest args]`.

The same as `python -m pytest tests`, from any directory. The server under test is
$WRANGLARR_CMD / $WRANGLARR_SETUP_CMD (default: the debug build, wranglarr/target/debug/wranglarr);
see tests/README.md."""
import os
import sys

import pytest

if __name__ == "__main__":
    here = os.path.dirname(os.path.abspath(__file__))
    sys.exit(pytest.main(["-c", os.path.join(here, "pytest.ini"), "--rootdir", here, here, *sys.argv[1:]]))
