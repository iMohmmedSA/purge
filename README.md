# purge

> [!WARNING]
> **Experimental. Use at your own risk.** purge deletes build output on its
> own, in the background. It only removes what build tools can recreate and
> skips projects that are in use, but it is young software: run `purge scan`
> first to see what it would do. It comes with no warranty of any kind; see
> the licenses below.

Build tools leave a lot behind: compiled dependencies, caches, and copies
of old builds that nothing will use again. Across a folder full of projects
that adds up to many gigabytes. purge watches the folders where your
projects live and reclaims that space in the background, removing only what
the build tools can recreate, and never while you are working on a project.

It runs on Linux, macOS and Windows, and handles Rust and
JavaScript/TypeScript projects today. Each kind of project is handled by an
*ecosystem*; see [Supported ecosystems](#supported-ecosystems).

## Install

**Linux and macOS:**

```sh
curl -LsSf https://github.com/iMohmmedSA/purge/releases/latest/download/install.sh | sh
```

**Windows** (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/iMohmmedSA/purge/releases/latest/download/install.ps1 | iex"
```

The installer downloads the latest release for your system, checks its
checksum, and installs three commands: `purge`, the same tool as
`purge-rs`, and `purged`, the background daemon. They go into
`~/.local/bin` (on Windows, `%LOCALAPPDATA%\Programs\purge`, which is added
to your `PATH`).

**Other ways:**

- With [Rust](https://rustup.rs): `cargo install --locked purge`
- Debian and Ubuntu: `sudo apt install ./purge_<version>-1_amd64.deb`, and
  Fedora and RHEL: `sudo dnf install ./purge-<version>-1.x86_64.rpm`, with
  the package from the [latest release](https://github.com/iMohmmedSA/purge/releases/latest)
- Or download an archive from the
  [releases page](https://github.com/iMohmmedSA/purge/releases) and put the
  three commands on your `PATH`, side by side

## Quick start

```sh
purge config init ~/Dev            # choose the folders to watch
purge scan                         # preview: what would happen right now
purge service install              # run in the background, starting at login
```

## How it works

purge finds **projects** below the folders you choose and measures their
**build output**. The ecosystem splits that output into **groups** that can
be cleaned on their own (for example a debug and a release build), and each
group into the individual **builds** inside it.

For every project purge watches two clocks: when its **sources** last
changed (including git commits and checkouts), and when each group was last
**built**. Then it applies these rules:

| Rule | When | What |
|---|---|---|
| Dormant project | no source change or build for `dormant_after` (14d) | remove all its build output; nothing is rebuilt |
| Unused group | a group not built for `max_age` (14d) | remove that group only |
| Stale builds | inside a group in use, builds not used for `max_age`, once they add up to `min_savings` (500 MB) | remove just those builds; nothing is rebuilt |
| Bloated group | still in use, but ≥ `bloat_factor` (1.5×) its fresh size, and cleaning saves ≥ `min_savings` | clean it, then rebuild it at low priority so it is ready and smaller |

**Stale builds** pile up inside output you use every day: a new copy for
every dependency update, change of options or compiler upgrade, with the old
copy left behind for good. purge removes the builds your tools have not used
for `max_age`, plus everything made by a compiler you have not used for that
long, and leaves the builds in use alone, so your next build is as fast as
before. It tells when a build was last used from file access times, after
checking that the filesystem records them (Linux's default does; where it
does not, only the compiler rule applies). Set `trim = false` to turn this
off.

The bloat check compares a group with its **fresh size**, which purge learns
without extra work: after it cleans a group (or a whole project), it records
the size of your next build, and after one of its own rebuilds, your next
build raises that size to include what the rebuild left out (tests,
examples). If a group has not grown well past its fresh size, rebuilding
would end up at about the same size, so purge leaves it alone. Only a group
with no recorded size that is at least `learn_rebuild_above` (5 GB) gets one
rebuild just to measure it. purge's own rebuilds never count as you using a
group.

The daemon checks every `interval` (6h). A bloated group is rebuilt at most
once per `refresh_cooldown` (7d), only while the machine is idle, at most
`max_rebuilds` (2) times per run, and never for a project you edited or
built in the last `quiet_period` (1h), so it doesn't compete with you while
you work.

**Shared build output**, used by several projects at once, is measured once
and judged by every project that uses it. It is only cleaned once none of
those projects is active (for a single group, once nobody has built it), and
it is never rebuilt. purge only touches build output outside a project when
the build tool has tagged it as its own (`CACHEDIR.TAG`).

Every threshold can be set globally, per group, and per project or folder.
See the comments in `purge config show`.

### Safety

- It skips any project where the ecosystem's tools (compilers, build tools,
  language servers) are running, or where a build holds a lock.
- Trimming removes whole builds only, never one used within `max_age`, and
  reads build files without changing their access times.
- It cleans through the build tool's own clean command where there is one.
  It deletes directly only inside directories the build tool has marked as
  its own.
- `purge clean` refuses to clean shared build output unless you pass
  `--force`.
- Rebuilds run at the lowest CPU priority (on Linux, their I/O priority
  drops with it), are capped per run, and have a per-group cooldown.
- The daemon postpones runs while the machine is busy, or outside the
  configured `window`.
- Every action is logged (`purge history`), and `purge scan` / `purge run
  --dry-run` change nothing.

## Supported ecosystems

### Rust (Cargo)

- **Projects:** Cargo workspaces, as `cargo metadata` describes them; every
  member belongs to its workspace.
- **Build output:** `target/`, plus a separate build directory if
  `build.build-dir` sets one (Cargo 1.91+). Output shared through
  `CARGO_TARGET_DIR`, `build.target-dir` or `build.build-dir` is judged as
  shared.
- **Groups:** profiles (`debug`, `release`, custom ones) and
  cross-compilation targets (`target/<triple>/<profile>`). An editor's
  private build directory such as `target/rust-analyzer` is cleaned but never
  rebuilt.
- **Builds:** Cargo compiles each crate once per set of options (dependency
  version, features, compiler) and never removes the old copy. purge
  identifies each build by its fingerprint and reads which compiler made it.
  Old incremental-compilation caches count as stale builds too.
- **Cleaning and rebuilding:** `cargo clean` (per profile and target), and
  `cargo build --locked` with `build_args` (default `--workspace`). purge
  never lets rustup download a toolchain.
- **In use:** `cargo`, `rustc`, `rustdoc`, `clippy-driver` or
  `rust-analyzer` running in the project, or a build holding Cargo's locks.

### JavaScript and TypeScript (npm, pnpm, Yarn, Bun)

- **Projects:** every `package.json`; a workspace member belongs to its
  workspace root (`workspaces` in `package.json`, or `pnpm-workspace.yaml`).
- **Groups:** only output purge can identify, each needing the tool that
  makes it to be a dependency of the project:

  | Group | Folder | Needs |
  |---|---|---|
  | `node_modules` | `node_modules/` | a lockfile, so it can be installed again exactly |
  | `next` | `.next/` | `next` |
  | `nuxt` | `.nuxt/`, `.output/` | `nuxt` |
  | `sveltekit` | `.svelte-kit/` | `@sveltejs/kit` |
  | `astro` | `.astro/` | `astro` |
  | `angular` | `.angular/cache/` | `@angular/cli` |
  | `turbo` | `.turbo/` | `turbo` |
  | `nx` | `.nx/cache/` | `nx` |
  | `parcel` | `.parcel-cache/` | `parcel` |
  | `docusaurus` | `.docusaurus/` | `@docusaurus/core` |

  Nothing else is touched: not build folders such as `dist/`, not `.env`
  files or anything else a project keeps, and never a file git tracks
  (some projects commit `node_modules`).
- **Builds:** tool caches inside a group, such as `node_modules/.cache/*`,
  `node_modules/.vite` and `.next/cache/*`, are trimmed when unused.
- **Last use:** dependencies are written once at install and then only
  read, so purge counts them as used whenever Node loads one (from file
  access times, where the filesystem records them) or the sources change.
- **Shared stores:** pnpm and Bun hard-link packages from a shared store,
  so removing their `node_modules` frees next to nothing; purge leaves it
  alone. npm copies packages, so its `node_modules` is worth removing from
  a dormant project. Yarn's default Plug'n'Play keeps no `node_modules`.
- **Cleaning:** purge removes these folders directly and never installs or
  builds; after it cleaned a dormant project, your next `npm install` (or
  `pnpm install`, ...) brings the dependencies back from the lockfile.
- **In use:** Node, Bun or Deno running in the project (dev servers,
  watchers, language servers), or a package manager.

More ecosystems are planned. Each one is a separate crate; see
[Adding an ecosystem](#adding-an-ecosystem).

## Commands

| | |
|---|---|
| `purge scan [FOLDER...]` | Show projects and planned actions (dry run) |
| `purge run [--dry-run] [--no-rebuild]` | Apply the policy now |
| `purge clean [PATH] [--profile P [--rebuild]] [--force]` | Clean one project, or one group, now |
| `purge clean [PATH] --stale [--profile P]` | Remove only stale builds now, keeping what is in use |
| `purge status` | Configuration, service, last run, space freed |
| `purge stats [--top N] [--json]` | Space freed per period, month, action and project; current build output size |
| `purge history [-n N]` | Action log |
| `purge config init/show/path` | Manage the configuration |
| `purge service install/uninstall/status` | Background service (systemd user unit, launchd agent, or login entry on Windows) |
| `purge daemon` | Run the background loop in the foreground |

## Layout

```
apps/
  cli/            the `purge` package: purge, purge-rs and purged binaries
  daemon/         the background loop: schedule, idle checks, notifications
core/
  config/         TOML config, policy resolution, schedule
  engine/         decide() (pure policy) and the executor
  scan/           walks roots and asks ecosystems about each folder
  session/        config + saved state + run lock + history around the engine
  state/          baselines, history log, run lock
ecosystems/
  api/            the Ecosystem trait and shared fs/command helpers
  rust/           Rust (Cargo): layout, artifacts (groups), units (builds)
  js/             JavaScript/TypeScript: project roots, known outputs, git check
  all/            registry of supported ecosystems
platform/
  notify/         desktop notifications
  process/        low-priority spawning, process table, CPU load
  service/        systemd / launchd / Windows login registration
```

## Development

```sh
cargo fmt --all                         # format
cargo clippy --all-targets              # lint; must be warning-free
cargo test                              # unit and integration tests
cargo run -p purge --bin purge -- scan  # try the CLI without installing
```

CI (`.github/workflows/ci.yml`) runs the same checks on Linux, macOS and
Windows.

To install from a checkout, or update after pulling changes:

```sh
cargo install --locked --path apps/cli  # purge, purge-rs and purged
purge service install                   # restarts the service on the new binary
```

### Releasing

1. Bump `version` in the root `Cargo.toml` (and the internal crates'
   `version` in `[workspace.dependencies]`), and commit.
2. Tag and push: `git tag v0.1.0 && git push origin v0.1.0`. The release
   workflow builds binaries for every platform and `.deb`/`.rpm` packages,
   and attaches them to a **draft** release. It refuses a tag that does not
   match the version.
3. Review the draft on GitHub and publish it. Publishing runs the publish
   workflow, which uploads every crate to crates.io after you approve it
   (`scripts/publish-crates.sh`: one crate at a time, dependencies first,
   waiting out crates.io's limit on new crates, so the very first release
   takes over an hour).
   The token lives in the `crates-io` environment as `CARGO_REGISTRY_TOKEN`
   (a crates.io token with the publish-new and publish-update scopes,
   limited to `purge*`). Publishing cannot be undone.

CI builds the Linux packages on every push too (`scripts/package-linux.sh`),
so packaging problems show up before a release.

Configuration and state stay compatible across updates: new settings get
defaults, and if a setting is ever renamed, its old name stays accepted.

### Conventions

- Latest stable Rust (`rust-version` 1.99, edition 2024) and the latest
  release of every dependency.
- `unsafe` code is forbidden in every crate (`unsafe_code = "forbid"`); OS
  calls go through safe wrappers such as `rustix`.
- `cargo clippy --all-targets` is warning-free with `clippy::pedantic` on.

### Adding an ecosystem

1. Create `ecosystems/<name>/` and implement `purge_ecosystem::Ecosystem`:
   `detect`, `source_modified`, `inspect`, `busy`, `clean_all`,
   `clean_group`, `trim` and `rebuild_group`.
2. Add it to `ecosystems/all/src/lib.rs`.
3. Add a section under [Supported ecosystems](#supported-ecosystems).

The scanner, policy engine, config, history and daemon need no changes.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
