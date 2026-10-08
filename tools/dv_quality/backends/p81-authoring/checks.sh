#!/bin/bash
set -euo pipefail
exec > >(tee /work/checks.log) 2>&1
python3 /work/cpu_reference.py
python3 /work/check_authoring.py
python3 /work/check_injector.py
python3 /work/check_picture_order.py
python3 /work/check_rpu_association.py
python3 /work/check_cross_render.py
python3 /work/check_cross_render_tests.py
python3 /work/test_import_cross_render.py
