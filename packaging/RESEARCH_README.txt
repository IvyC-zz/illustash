Illustash Windows x64 research test package

Extract the entire ZIP to a short local path, for example D:\Illustash.
Do not run it inside the ZIP. Use test images first; this is an unsigned test build.

First use: double-click first-start.cmd. Enter the absolute image folder and a
NEW library path, for example D:\IllustashData\test.pilib. Source images remain
in their original folder. The application indexes them; it does not copy them.
Reopen: research.cmd --library D:\IllustashData\test.pilib
Dependency diagnostic: research.cmd --check

A runtime-inclusive package needs no system Python or Cargo. Windows 10/11 x64,
Microsoft Edge WebView2 Runtime and the Microsoft Visual C++ runtime are required.
CPU inference is used; GPU acceleration has not been enabled in this package.

Learning/clustering/similar-image calculation, WD/CCIP recognition and local
provenance calculations are enabled by research.cmd. Recognition adds machine
candidates, never a human confirmation. Fresh installs contain no private
character anchors or external reference dataset. They still compute WD tags and
CCIP vectors; extra character-index references require explicit network permission.

Apple Photos is not supported by this Windows adapter.
Third-party provenance requests require separate in-app authorization. No images
are sent to recognition services or language models. No accounts/credentials are bundled.

The program is unsigned: Windows may show SmartScreen. Do not disable antivirus
or device security policies to run it. This package is for personal testing.
Python/dependency licenses are included with their distributions. Model origins:
WD embeddings: https://huggingface.co/deepghs/wd14_tagger_with_embeddings (Apache-2.0)
CCIP: https://huggingface.co/deepghs/ccip (OpenRAIL), ONNX distribution:
https://huggingface.co/deepghs/ccip_onnx
Observe upstream model use restrictions; this is not a public release approval.
