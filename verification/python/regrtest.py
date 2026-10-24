# SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
# SPDX-License-Identifier: GPL-3.0-only

"""regrtest.py: runs CPython's own tests inside Veda, as `python3 -m test`
does in the Terminal, from a program that `init` starts (`run=python3:
/system/tests/python/regrtest.py,ARG1,ARG2...`, with regrtest's arguments).

The tests are in images built with `--python-tests`. A program `init`
starts has only HOME in its environment: the tests get the Terminal's
(PATH, USER) and a scratch directory, /tmp, to work in.
"""

import os
import runpy
import sys

os.environ.setdefault("PATH", "/system/bin")
os.environ.setdefault("USER", "user")
os.chdir("/tmp")
sys.argv = ["python3 -m test", *sys.argv[1:]]
runpy.run_module("test", run_name="__main__", alter_sys=True)
