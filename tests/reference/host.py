"""Explicitly built legacy transport example; never falls back to the production CLI."""
import os
from pathlib import Path
from web import terms_core

def binary():
    configured = os.environ.get('ILLUSTASH_REFERENCE_HTTP_BIN')
    path = Path(configured) if configured else terms_core.binary().parent / 'examples' / ('reference-http.exe' if os.name == 'nt' else 'reference-http')
    if not path.is_file():
        raise RuntimeError('Build test reference: cargo build --manifest-path native/terms/Cargo.toml --features test-reference-host --example reference-http')
    return path
