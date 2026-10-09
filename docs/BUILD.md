# Building nokk

nokk's fingerprinted transport is backed by BoringSSL (via
[`wreq`](https://crates.io/crates/wreq) / `boring-sys2`), which is compiled from source
on the first build. That step needs a C/C++ toolchain, **CMake**, and **libclang**
(bindgen uses it to parse BoringSSL's headers).

> **Windows: `git` must be on `PATH`.** The vendored `btls-sys` build script
> runs `git init`, and a default Git-for-Windows install lands outside `PATH`
> (e.g. `%LocalAppData%\Programs\Git\bin\git.exe`). Prepend that directory for
> the build shell instead of installing anything heavy:
> ```powershell
> $env:PATH = "$env:LOCALAPPDATA\Programs\Git\bin;$env:PATH"
> cargo check -p nokk-captcha   # no native deps; passes without cmake/VS
> ```
> `cargo check -p nokk` additionally needs CMake + the VS C++ workload +
> libclang (failure mode: `cmake-0.1.58: failed to execute command`). Light
> crates (`nokk-captcha`, `nokk-stealth`, `nokk-net --lib blocklist`) build and
> test without that toolchain.

## With root (recommended)

Debian / Ubuntu:

```bash
sudo apt install build-essential cmake clang libclang-dev
cargo build --release
```

Fedora:

```bash
sudo dnf install gcc gcc-c++ cmake clang clang-devel
cargo build --release
```

macOS (Homebrew):

```bash
brew install cmake llvm
cargo build --release
```

The first build compiles BoringSSL (~45s); it is cached afterward.

## Without root (user-space bootstrap)

If you can't install system packages, CMake and libclang can be provided from
user-space `pip` wheels, and clang's builtin headers borrowed from an existing GCC
install. This is exactly how the reference environment builds.

1. **CMake** — `pip install --user cmake` (lands in `~/.local/bin`; make sure it's on `PATH`).
2. **libclang** — `pip install --user libclang` (lands at
   `~/.local/lib/python3.X/site-packages/clang/native/libclang.so`).
3. **clang builtin headers** (`stddef.h`, etc.) — reuse your GCC ones, e.g.
   `/usr/lib/gcc/x86_64-linux-gnu/12/include`.

Wire it up with a repo-local `.cargo/config.toml` (copy from
[`.cargo/config.toml.example`](../.cargo/config.toml.example) and edit the paths):

```toml
[env]
LIBCLANG_PATH = "/home/you/.local/lib/python3.X/site-packages/clang/native"
BINDGEN_EXTRA_CLANG_ARGS = "-isystem /usr/lib/gcc/x86_64-linux-gnu/12/include -isystem /usr/include"
```

> **Keep these values stable between builds.** Changing `LIBCLANG_PATH` /
> `BINDGEN_EXTRA_CLANG_ARGS` forces `boring-sys2` to rebuild from scratch, which is why
> they live in `.cargo/config.toml` rather than ad-hoc shell exports. The file is
> `.gitignore`d because the paths are machine-specific.

## The V8 archive

`v8` comes from crates.io (`=152.2.0`, V8 15.2), and its build script downloads the
prebuilt V8 for the target from the matching rusty_v8 GitHub release (~185 MB
unpacked, per target). Offline, or to keep one copy across clean builds, download
the two files once and name them in `.cargo/config.toml`:

```
https://github.com/denoland/rusty_v8/releases/tag/v152.2.0
  librusty_v8_release_<target>.a.gz  → lib.a
  src_binding_release_<target>.rs    → binding.rs
```

```toml
[env]
RUSTY_V8_ARCHIVE = "/home/you/.cache/nokk/rusty_v8-152.2.0/lib.a"
RUSTY_V8_SRC_BINDING_PATH = "/home/you/.cache/nokk/rusty_v8-152.2.0/binding.rs"
```

Building V8 from source instead (`V8_FROM_SOURCE=1`) works but wants ~30 GB and
`depot_tools`; the release archive is the same binary Deno ships.

## Verifying the build

```bash
cargo test                      # 192 tests, offline (no network)
cargo run --bin nokk -- --fetch https://tls.browserleaks.com/json
```

The `--fetch` call should return a JA3/JA4 that matches current Chrome.
