#!/bin/sh
set -eu

PYTHONDONTWRITEBYTECODE=1 python3 scripts/check-license-policy.py
