# Installing OCA

Step-by-step from nothing to a working `oca` on your `PATH`. OCA is a
single binary — the CLI and the TUI (`oca open`) are the same program.

> **Quick install (Linux):** from the repository root, run
> `./scripts/install.sh` — it does steps 1, 3, and the `PATH` check for
> you (installs the toolchain and ALSA packages when missing, safe to
> re-run), then pick up at step 4.

## 1. Prerequisites

**All platforms:**

- **Rust 1.89 or newer** — install with [rustup](https://rustup.rs):
  ```sh
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
  Check with `rustc --version`. If you already have rustup, run
  `rustup update stable`.

**Linux only** — the audio crate links against ALSA at build time (even on
machines with no sound device; recording is the only thing that needs an
actual device):

```sh
# Debian / Ubuntu / Mint
sudo apt install pkg-config libasound2-dev

# Fedora
sudo dnf install pkgconf alsa-lib-devel

# Arch
sudo pacman -S --needed pkgconf alsa-lib
```

**macOS / Windows:** no extra packages — CoreAudio / WASAPI are built in.

## 2. Get the source

```sh
git clone <this repository's URL> OCA
cd OCA
```

## 3. Build and install

```sh
cargo install --path crates/oca-cli
```

This compiles in release mode and puts the binary at `~/.cargo/bin/oca`
(already on your `PATH` if you installed Rust via rustup). Verify:

```sh
oca --version        # -> oca 0.1.0
which oca            # -> /home/you/.cargo/bin/oca
```

If `oca: command not found`, your `PATH` is missing `~/.cargo/bin` — add
this to your shell profile and open a new terminal:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

## 4. Create your first project

```sh
oca new "Tideways"   # creates the directory and registers it
cd Tideways
oca doctor           # health of every subsystem — warnings are fine
```

`doctor` flags nothing as broken on a machine with no microphone and no AI
models — audio capture and AI are optional subsystems, and everything else
works without them.

## 5. Use it

The TUI is the main surface — same binary, no extra install step:

```sh
oca open             # inside a project directory
oca open Tideways    # or by name, from anywhere
```

See `README.md` for the full tour: chapter writing, story queries, song
analysis, snapshots, search, and a key map for every TUI panel.

## 6. Optional: offline AI

Nothing below is required — every feature works with every model deleted.

1. **Runtime pack** (the llama.cpp / whisper runtime, side-loaded — never
   downloaded at runtime): get a signed pack for your platform and run
   ```sh
   oca runtime install --from oca-runtime-<platform>.tar.gz
   oca runtime list
   ```
2. **Model files** go under your home directory, never inside a project:
   ```
   ~/.oca/models/llm        # story/music operations
   ~/.oca/models/embeddings # hybrid (semantic) search
   ~/.oca/models/whisper    # dictation — needs no LLM at all
   ```
3. **Without any of it**, you can still try every AI surface offline by
   putting this in `~/.oca/config.toml`:
   ```toml
   [ai]
   mock = true
   ```
   (canned deterministic output, no runtime, no model files)

## 7. Updating and uninstalling

```sh
git pull && cargo install --path crates/oca-cli   # update in place
cargo uninstall oca                               # remove the binary
```

Projects are plain directories of Markdown/YAML — uninstalling the binary
never touches your work. The only machine-wide state is `~/.oca/`
(config, models, logs), which you can delete freely once the binary is
gone.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| `oca: command not found` after install | `~/.cargo/bin` is not on your `PATH` (see step 3) |
| Build error mentioning `alsa` or `pkg-config` | Install the Linux packages from step 1 |
| `error: no project here` from `oca open` | Run it inside a project directory, or pass a name/path: `oca open Tideways` |
| Recording does nothing / no input device | Only affects capture — `oca doctor` shows the audio check; every non-recording feature works regardless |
| AI panel says no runtime | Expected until step 6; the app is fully functional without it |
