# Features

Cabin features are public, additive, named-boolean capabilities the user (or a downstream consumer)
selects at build time.  Features may imply other features and may enable optional dependencies on
the same package.  They are declared in the `[features]` table of `cabin.toml`.

## Manifest syntax

```toml
[package]
name = "demo"
version = "0.1.0"

[features]
default = ["simd"]
simd = []
ssl = []
full = ["simd", "ssl"]
```

### Identifier grammar

Feature names are:

- non-empty;
- ASCII letters, digits, `_`, `-`;
- no whitespace, `/`, `.`, or `:`.

This keeps ordinary feature identifiers separate from the feature-entry syntax described below.

### Rules

- The reserved `default` key holds the list of features enabled when the user does not pass
  `--no-default-features`.  It may be omitted.
- A feature value is a list of *feature entries*, each of which is one of:
 - `"feature_name"` - enables another local feature on the same package (transitive feature
   implication).
 - `"dep:dependency_name"` - enables an optional Cabin package dependency declared by this package's
   `[dependencies]` table.  The dependency name may be scoped: `"dep:fmtlib/fmt"`.
 - `"dependency_name/feature_name"` - requests a feature on a Cabin package dependency.  If the
   dependency is optional this form also enables it.  The split is on the *last* `/` (feature names
   never contain one), so `"fmtlib/fmt/json"` requests feature `json` of the scoped dependency
   `fmtlib/fmt` and the legacy `"fmt/json"` keeps its meaning.
- The on-disk shape stays a list of strings; the typed
  [`FeatureEntry`](https://github.com/cabinpkg/cabin/blob/main/crates/cabin-core/src/config.rs) view
  is produced lazily by the feature resolver.
- Local feature references must point at another declared feature in the same package.  Unknown
  local references are rejected with a clear error.
- Cycles between local features are rejected with a clear `feature definitions contain a cycle: a ->
  b -> a` error.
- Declaring a normal feature called `default` is rejected (the key is reserved for the default
  group).
- Feature entries may only use ASCII letters, digits, `_`, `-`, `.`, plus the leading `dep:`
  prefix, the scope separator inside a scoped dependency name, and the `/` before a feature name.
  Anything else is rejected with a clear error.

## Optional dependencies and the feature resolver

Features can also turn Cabin package dependencies on and off, and can request features on dependency
packages that are present.  This layers cleanly on top of the dependency-kind model:

```toml
[dependencies]
fmt = { version = "^10", features = ["compile"], default-features = false }
openssl = { version = "^3", optional = true }

[features]
default = []
ssl = ["dep:openssl"]
full = ["ssl", "openssl/vendored"]
```

- `optional = true` declares an optional Cabin package dependency (supported in `[dependencies]`;
  rejected in `[dev-dependencies]` and ``system = true` deps`).
- `features = ["..."]` requests features on the dependency package; entries must be feature names
  declared by that dependency.
- `default-features = false` drops *this edge's* request for the dependency's `default` feature.  It
  does **not** globally disable the dependency's defaults - if any other edge requests defaults for
  the same dependency, the unified result still includes them.

The cross-package feature resolver lives in the `cabin-feature` crate.  Given a typed
`PackageGraph`, the selected root indices, and a `RootFeatureRequest` built from the CLI flags, it
computes the *additive* closure of:

- enabled features per package;
- enabled optional dependencies per package;
- per-edge feature requests applied to the depended-on package.

Resolution is deterministic (sorted iteration, fixed-point worklist) and never touches the network.
Errors are explicit and testable: unknown root features, `dep:` on non-optional dependencies, and
requests for features the depended-on package does not declare all surface with stable wording.

Effects on commands:

- **`cabin resolve` / `update` / `fetch`** apply features to local and registry packages. Active
  dependency edges combine their feature requests; defaults remain enabled whenever any active
  edge requests them. Enabled features expand local implications, `dep:`, and `dep/feature`
  entries, adding each enabled optional dependency's version requirement. Platform-inactive edges
  neither enable packages nor request features. Disabled optional packages and their dependencies
  may be absent from the index. Dev dependencies remain excluded by default; system dependencies
  remain declaration-only.
- **`cabin resolve` / `update`** write the same dependency lockfile as a build with the same feature
  selection, without downloading archives. Turning a feature off removes packages it alone enabled
  on the next resolution. `--locked` fails when newly enabled dependencies are missing from the lockfile.
- **`cabin build`** downloads, compiles, and links enabled dependencies used by the target graph for
  C and C++. A target's `deps` entry naming a disabled optional dependency fails with the dependency
  name and an enabling feature, including when that package has not been downloaded. Requested
  features missing from the selected package's manifest are diagnosed during build.
- **`cabin vendor`** includes enabled optional dependencies and excludes disabled ones.
- **`cabin package`** preserves `optional`, `features`, and `default-features` per dependency in the
  canonical metadata document.  Bare entries without overrides serialize as plain
  version-requirement strings so older readers stay happy.

## Feature-gated targets

A target may declare `required-features`: package features that must all be enabled for the target
to be built or used.  Entries name features declared in the *same* package's `[features]` table;
unknown names are rejected when the manifest loads.

```toml
[package]
name = "foo"
version = "1.0.0"
cxx-standard = "c++17"

[features]
default = []
ssl = []

[target.http]
type = "library"
sources = ["src/http.cc"]

[target.tls]
type = "library"
sources = ["src/tls.cc"]
deps = ["http"]
required-features = ["ssl"]
```

The gating rules are:

- **Default enumeration skips gated targets.**  `cabin build` (and `cabin test`'s no-`--test`
  enumeration, `cabin run`'s single-executable pick, and `cabin tidy`) simply does not select a
  target whose required features are not all enabled.  When *every* candidate is gated off, the
  command fails with an error naming the gated targets and their missing features instead of
  claiming nothing exists.
- **Explicit requests hard-error.**  A `deps` entry, a manifest-target selector, `cabin run
  --bin`, or `cabin test --test` naming a gated target fails with the missing features and the
  concrete fix (`--features <name>`, or a `features = [...]` request on the consumer's dependency
  edge).

Features stay purely additive capabilities: enabling a feature makes a gated target *available*
but never adds a `deps` entry to any consumer.  A consumer that wants an optional target does both
things explicitly - enables the feature on its dependency edge and names the target in `deps`:

```toml
[dependencies]
foo = { version = "1.0.0", features = ["ssl"] }

[target.app]
type = "executable"
sources = ["src/main.cc"]
deps = ["foo:http", "foo:tls"]
```

Dropping the `features = ["ssl"]` request fails the build with the `required-features` error
above; dropping the `"foo:tls"` entry simply never links `tls`.  The runnable
[`feature-gated-targets`](https://github.com/cabinpkg/cabin/tree/main/examples/feature-gated-targets)
example shows the full workspace shape.

`required-features` gates *whether the target builds*; to vary compile flags by feature on a
target that always builds, use `[target.'cfg(feature = "...")'.profile]` layers instead (see
[Target / platform-specific dependencies](target-dependencies.md)).

## CLI selection

`cabin build` / `cabin resolve` / `cabin update` / `cabin metadata` accept the same selection flag bundle:

```
--features <names>          # repeatable; each value may be comma-separated
--all-features              # enable every declared feature
--no-default-features       # drop the [features].default set
```

`cabin tree` and the graph-only `cabin explain` subcommands (`package`, `target`, `source`, and
`feature`) also run the feature-selection part of this bundle so unknown features and invalid `dep:`
feature entries surface consistently.

Default behavior:

- without `--no-default-features`, the names listed under `[features].default` are enabled and
  expanded transitively;
- `--all-features` overrides everything else and enables every declared feature;
- CLI selections apply to the selected root / primary packages.  Dependency feature requests
  declared on edges are then resolved additively through the graph.

Errors are validated up-front and reported with a clear message:

```
$ cabin build --features missing
unknown feature "missing" for package "demo"
```

## `cabin metadata`

`cabin metadata --format json` includes a per-package `features` block (omitted when empty so older
consumers see the same JSON shape they always have) and a resolved `configuration` block whenever
the package declared features:

```json
{
  "configuration": {
    "features": ["simd"],
    "fingerprint": "<64 hex chars>"
  }
}
```

Selection flags passed to `cabin metadata` flow into the configuration block exactly the way they do
for `cabin build`.

## Package metadata and registry preservation

`cabin package` writes feature declarations into the `<name>-<version>.json` document next to the
archive.  `cabin publish --registry-dir <path>` carries them through the registry's per-package
index file.  `cabin-index` (local file index) and `cabin-index-http` (sparse HTTP index) parse the
optional `features` field on every version entry and preserve it on `VersionMetadata`.  Older
registry entries that omit the field continue to load.

The version resolver uses index feature metadata to activate optional registry edges while solving
version requirements. If an enabled dependency cannot resolve, it tries other versions of the
package or its dependents. Feature requests from rejected registry versions do not activate packages
in the final solution. Sparse HTTP indexes fetch package metadata only when resolution needs it;
missing transitive packages permit backtracking.

A `[patch]` replacement receives the union of requests for the replaced package, including names
that only the replacement declares. Its own manifest supplies its features and defaults, and its
edges and local path dependencies forward requests normally. Enabled optional dependencies also
participate in native `links` collision checks during resolution or build.

The lockfile records resolved registry versions and dependency edges; per-`BuildConfiguration`
feature state lives in the run-time fingerprint and is not persisted to `cabin.lock`.
