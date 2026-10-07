"""Historical Python app and bridge: opt in only from tests / reference tools."""
from pathlib import Path
import provenance
import provenance.sources
import web

ROOT = Path(__file__).resolve().parents[2]
# The historical Python app keeps its import names (web.store, provenance.run, ...) but lives here (#186);
# the production packages only hold the old Python calculators, kept as test references (#9).
for package, folder in ((web, 'web'), (provenance, 'provenance'), (provenance.sources, 'provenance/sources')):
    reference = str(Path(__file__).parent / folder)
    if reference not in package.__path__:
        package.__path__.append(reference)
