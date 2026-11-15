#!/usr/bin/env python3
"""Import the installed scrutate and fail if that turned the GIL on in a free-threaded Python.

A module not marked gil_used = false turns the GIL back on at import, with only a warning, so run it
with `-W error` too where a warning must fail.
"""
import sys
import sysconfig

import scrutate

free = sysconfig.get_config_var("Py_GIL_DISABLED") == 1
gil = getattr(sys, "_is_gil_enabled", lambda: True)()
print(scrutate.__version__, "free-threaded:", free, "gil enabled:", gil)
if free and gil:
    sys.exit("importing scrutate turned the GIL on")
