#!/bin/sh
# The suite imports the Web Push stack, so it runs in a venv pinned to web/requirements.txt, rebuilt when that changes.
set -eu
cd "$(dirname "$0")/.."
venv=target/web-checks-venv
if ! cmp -s web/requirements.txt "$venv/requirements.txt"; then
  rm -rf "$venv"
  python3 -m venv --without-pip "$venv"
  curl -fsS https://bootstrap.pypa.io/get-pip.py -o "$venv/get-pip.py"
  "$venv/bin/python" "$venv/get-pip.py" -q
  "$venv/bin/pip" install -q --disable-pip-version-check -r web/requirements.txt
  cp web/requirements.txt "$venv/requirements.txt"
fi
PYTHONDONTWRITEBYTECODE=1 exec "$venv/bin/python" -m unittest discover -s web -v
