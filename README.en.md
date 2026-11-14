# Illustash

[简体中文](README.md) | English

A local library tool for anime-style illustrations. It organizes thousands of pictures saved from X and Pixiv by series and character, and helps trace back their artists and original posts. Originals stay where they are; recognition and indexing run locally, and your pictures never leave your computer.

Save illustrations from X and Pixiv long enough and you easily end up with tens of thousands of pictures. Kept in a system photo app, they mix with everyday photos and screenshots because general-purpose albums do not recognize anime characters; sorting into folders by hand breaks down when multiple characters share a picture or appear in different outfits, while asset managers like Eagle require tagging and maintaining everything yourself.

Once you connect your picture sources and finish local recognition, Illustash builds a ready-to-browse catalog of series and characters. You do not need to sort tens of thousands of pictures before using it, and pending-work counts are not shown on screen by default (“Counts in the sidebar” is off by default). Browse your pictures like a photo album; when you see a misidentified character, fix it with a click and the library updates subsequent assignments around your decision.

## What it does

### 1. Picture sources and character catalog

**System Photos and local folders:** Connects to macOS Photos (read-only, importing pictures saved from X and Pixiv) and local folders without moving or copying your originals. Picture sources are organized into tabs by saved origin (“Pixiv”, “X”, “Other”): macOS Photos distinguishes them automatically by the app that saved each picture, while folders are assigned an origin when added.

**Hierarchical series and character catalog:** Uses local models to identify characters in each picture and builds a “Catalog” structured as Series (with optional parent series) → Character → Outfit / Sub-tag (outfit, alternate version, another form); turning on “Online name lookups” can fill in Chinese names as well. Searching by Chinese, Japanese, or English names, aliases, or raw tags finds the matching character. Group pictures appear under every character in them, and different outfits or looks stay grouped under the same character.

**Creating new series and characters:** Series, characters, or original characters not yet in the catalog can be created directly without waiting for a character index update. Once you confirm 5 pictures for a character, Illustash uses their visual features to link other pictures of that character; for characters whose names you do not know yet, look-alike pictures can be grouped under “Unidentified” first and renamed or merged into a character later.

### 2. Manual corrections and local learning

Machine recognition results apply by default, so you do not need to check every tag before browsing. When you spot a mistake while paging through pictures, you can change, remove, or merge characters with a click, and every action can be undone at any time (⌘Z / Ctrl+Z).

**Your decisions take precedence:** Any assignment you confirm or reject always overrides machine judgment. Re-running recognition or updating the character index later will never overwrite assignments you edited or re-add a wrong character you removed.

**Identifying new looks from confirmed examples:** When a picture has a fan-made outfit, a different art style, or was missed initially, confirming 5 pictures for that character lets Illustash look for their other appearances based on existing examples: high-confidence matches are assigned automatically, while uncertain ones are marked “Might also be” for you to confirm.

**Grouping obscure and original characters:** For characters whose names you do not know yet and original characters (OCs), Illustash groups look-alike pictures by appearance and traits such as hair color, eye color, and animal ears. Once you know who they are, you can assign the entire group at once without editing picture by picture.

“Home” occasionally shows a “Who's this?” card to add confirmed examples for uncertain characters or characters with few samples. For series you follow regularly, you can also turn on “Follow new characters” in the “Catalog” to fetch reference features for newly debuted characters on demand after confirming network access.

### 3. Finding artists and original posts

For older pictures whose file names were changed and whose source posts were lost, Illustash can recover the origins of part of your illustrations in batch once you allow network access, without uploading your originals or thumbnails.

**Matching posts from multiple clues:** Combines Danbooru pixel hashes, locally computed IQDB signatures, and your Pixiv following list or an imported X data-archive following list to find original posts and restore artist names and post links.

**Finding other works by a known artist:** Once an artist is identified, you can click “Find more of their pictures” in the details panel to fetch thumbnails of their public works on Danbooru and Pixiv (matching R18 works on Pixiv requires signing in with your own account inside the desktop app). The thumbnails are compared with your library by signature on your computer only and are not kept afterward; matches show you other works by the same artist in your library.

Clicking an artist from picture details lets you filter the library by that artist and open links to their profile pages and original posts.

### 4. Excluding non-illustrations, grouping duplicates, and R18 filtering

**Excluding non-illustrations:** Automatically distinguishes anime-style illustrations from real-life photos, chat logs, UI screenshots, and text images, placing non-illustrations under “Excluded” (rules can be toggled in Settings). A macOS Photos source only reads pictures saved from X and Pixiv, and everyday photos or clutter mixed into folders or saved files are moved to “Excluded” by the same rules.

**Grouping duplicates and variants:** Re-saved originals, versions with added or removed text, crops, color-patch or mosaic censored pictures, and subtle variants that only change an expression or glasses are compared and grouped locally. Select several pictures and click “Compare” to outline differing regions or crop positions on the pictures.

Identical and re-saved pictures fold automatically by default; subtle variants, local edits, crops, and other relations can also be set to fold in Settings. Editing character assignments on copies folded into the same duplicate group syncs automatically; if copies in a group disagree on their characters, they are held for alignment first and not folded yet.

**R18 rating and filtering:** Automatically detects picture age ratings. The “R18” button in the top bar hides R18 content (shortcut H) or switches to “R18 only”.

## Privacy and boundaries

**Pictures stay local:** Picture recognition, feature extraction, similarity matching, and library indexing all run on your computer. Originals and features computed from them are never uploaded to the cloud, never sent to any LLM or multimodal API, and never used to train or improve any shared model (including the recognition models used by this app). Scoring rules learned locally from your labels (such as character prototypes and custom tag candidates) are saved only in the current library and disappear if the library is deleted.

**Originals stay untouched:** Building a library does not copy picture files, delete or modify original files, or directly modify the Apple Photos database. Illustash only builds its own index, so your existing folders or albums can be used as usual.

**Network access requires your permission:** Every network feature is off by default. Looking up Chinese character names and searching Danbooru tag data send only text such as tag names and series names (when assigning a character to a picture, Danbooru candidates fetch one small all-ages preview picture by tag); you can grant permission once for the current library and revoke it at any time.

“Find artists” and “New characters” each require separate confirmation before every run; recognition models are fetched online only when you click download (or can be imported offline from files); update checks periodically query the GitHub release list only after you actively turn them on (sending no library data), and can be run manually in “Settings › About” when off; a new version is downloaded from GitHub only when you click “Download and restart”, and is installed after its signature is verified. “Network and privacy” in Settings lists every network feature and the data it sends; saving an account or changing startup preferences does not grant network access. Microsoft's ONNX Runtime, which recognition uses, has telemetry on by default in its official releases; Illustash turns it off in every recognition process, so it does not go online. On Windows, the Microsoft DirectML component used for GPU-accelerated recognition is registered with the system's telemetry channel and may, under its license, send usage data to Microsoft according to your system's “Diagnostic data” settings; if you prefer not to use it, turn off “Use the GPU to speed up recognition” in “Settings › Data › Recognition models” (switching to CPU only without loading the component), as detailed in [Troubleshooting installation and launch](docs/INSTALL.md#windows) (in Chinese).

**Third-party site access:** External services such as Pixiv, X, Danbooru, and IQDB are all optional. When an account is needed, Illustash uses your own account with read-only, low-frequency access; if it encounters a CAPTCHA or rate limit, it stops and does not attempt to bypass it.

## Status and running

Illustash is a personal project built primarily for the author's own needs and is still in early development.

macOS (Apple silicon, macOS 14 or later) is the primary platform, and Windows x64 is also usable. Linux is frozen (decided 2026-10-06): no installers or local computation (recognition, indexing, learning, origin lookup) are provided, no checks are run on Linux, and Rust code is not guaranteed to compile on Linux; if needed later, support will be built on top of the native macOS/Windows implementation.

The interface supports Simplified Chinese and English, defaults to “System”, and can be switched in “Settings › General › Language”.

There is no official release yet, nor are notarized or code-signed installers provided; building from source is currently required. The core uses Rust stable; Python is not needed at runtime, while some build and check tools require Python 3.12 and Node 22.

Model files required for local recognition are not included in the code repository (in the full app they can be downloaded when prompted on first use, or imported offline from files; running from source requires preparing them separately). See [Installation and platforms](docs/BACKEND_TASKS.md#独立安装与可选计算包) for dependencies, build steps, and compute component setup, and [Troubleshooting installation and launch](docs/INSTALL.md) for common gatekeeping prompts, model downloads, and runtime troubleshooting.

### Running from source

```sh
# Build the core
cargo build --release --manifest-path native/terms/Cargo.toml

# Create a library for the first time: registers sources only, without scanning pictures
./native/terms/target/release/illustash-terms library-create \
    --dest ~/Pictures/Illustash/MyLibrary.pilib --folder /path/to/pictures

# Open (defaults to http://127.0.0.1:8765)
./start-web.command --library ~/Pictures/Illustash/MyLibrary.pilib
```

Creating a library for the first time only registers picture sources and does not immediately scan all pictures. For the desktop app (Tauri), installer builds, launch options, and library package layout, see [Development guide](docs/DEVELOPMENT.md).

## Feedback and contributing

Feature trade-offs prioritize the author's own usage needs.

Bug reports are welcome via Issues using the [Bug report template](.github/ISSUE_TEMPLATE/bug_report.md), along with your OS, the output of “Settings › About › Copy diagnostics”, and steps to reproduce (please do not post real pictures, picture file names, local paths, or account details); if the app fails to install or open, check [Troubleshooting installation and launch](docs/INSTALL.md) first.

Feature suggestions can be submitted with the [Feature request template](.github/ISSUE_TEMPLATE/feature_request.md), though adoption or implementation is not guaranteed.

Code contributions (Pull Requests) are not accepted at this time.

## License

Illustash's source code is released under the [GPL-3.0-or-later](LICENSE) license, with an additional permission under GPLv3 section 7 allowing this program to be distributed and run together with GPU inference runtime libraries such as Microsoft DirectML; see [Additional permission](docs/THIRD_PARTY.md#附加许可gplv3-第-7-条) for the full text.

Third-party components, models, and data follow their respective licenses; see [Third-party components and licenses](docs/THIRD_PARTY.md) for details. Recognition models, character indexes, and other data—whether bundled or prepared separately—are not uniformly covered by this project's code license. The CCIP model used for recognition (not included in the repository or release installers; downloaded from upstream on first use or prepared separately) is provided under the BigScience Open RAIL-M license, and its use and redistribution are bound by the use restrictions in paragraph 5 and Attachment A.

The Illustash name and icon are not licensed along with the code. Modified versions based on this project must use a different name and icon.

## Credits

Character and picture recognition use [SmilingWolf's WD Tagger](https://huggingface.co/SmilingWolf/wd-swinv2-tagger-v3), [DeepGHS's CCIP](https://huggingface.co/deepghs/ccip), and [Character Index](https://huggingface.co/datasets/deepghs/character_index); tag and character data come from [Danbooru](https://danbooru.donmai.us), and Chinese character names come from [Bangumi](https://bgm.tv).
