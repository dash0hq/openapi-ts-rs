# openapi-ts-rs

Generates TypeScript types from OpenAPI 3 specs in the shape
[openapi-typescript](https://github.com/openapi-ts/openapi-typescript) 6.7.0 emits (`paths`,
`components`, `operations`, `external`), so code written against 6.7.0's output keeps compiling.
It is a native binary with no Node or TypeScript dependency, so it is unaffected by TypeScript 7.

```sh
openapi-ts-rs spec.yml -o spec.gen.d.ts   # warns about $refs that do not resolve
openapi-ts-rs spec.yml --strict           # ...or fails on them
openapi-ts-rs spec.yml --named-exports    # also exports every schema by name
openapi-ts-rs api/spec.yml --ref-root .   # keys external files relative to . instead of api/
```

`external[...]` keys each referenced file by its path relative to the spec's directory. Moving the
spec changes those keys and breaks code that spells them out. `--ref-root <dir>` keys them relative
to `<dir>` instead, so a spec can move without renaming its external types.

`--named-exports` appends `export type Pet = components["schemas"]["Pet"];` for every schema under
`components.schemas`, in the root file and in every referenced file, so code can import `Pet`
instead of spelling out the lookup. A name that more than one referenced file defines gets no
export and is listed in a comment instead; the root file's schemas always win. Without the flag
the output is unchanged.

## Install with mise

```toml
# mise.toml
[tools]
"github:dash0hq/openapi-ts-rs" = "latest"
```

Every push to `main` publishes a release (`v0.1.<run number>`) with binaries for Linux (glibc and
musl) and macOS on x86_64 and arm64.

## Performance

openapi-typescript 6.7.0 against openapi-ts-rs on 6.7.0's own example specs. Mean ± σ of wall time,
then median peak memory:

| Spec | openapi-typescript 6.7.0 | openapi-ts-rs | Speedup |
|---|---|---|---|
| `github-api.yaml` | 1,108 ± 79 ms, 246 MiB | 321 ± 16 ms, 92 MiB | 3× |
| `stripe-api.yaml` | 663 ± 43 ms, 179 MiB | 90.2 ± 7.0 ms, 49 MiB | 7× |
| `digital-ocean-api/DigitalOcean-public.v2.yaml` | 31,193 ± 569 ms, 168 MiB | 40.1 ± 5.3 ms, 21 MiB | 778× |
| `octokit-ghes-3.6-diff-to-api.json` | 228 ± 7 ms, 91 MiB | 32.6 ± 2.8 ms, 15 MiB | 7× |

The gap grows with the number of files a spec references. DigitalOcean's spec is split over 1,659
YAML files, and 6.7.0 walks and serializes every loaded file again for each one it loads. On the
single-file specs, most of openapi-ts-rs's time goes to parsing. Measured at `f352418` on an
8-vCPU Intel Xeon @ 2.60 GHz (Linux) with Node 24.22.1 and hyperfine 2.0.0. To reproduce:

```sh
cargo build --release
cd compat && npm ci && ./bench.sh   # needs hyperfine >= 2.0; prints the table above
```

## Compatibility with 6.7.0

Output is checked by type equality, not text: `compat/compare.mjs` runs both generators and asks
TypeScript whether every schema, response, parameter, request body, header, path method and
operation is identical (and mutually assignable) between the two outputs.

```sh
cargo build --release
cd compat && npm ci && ./fetch-corpus.sh corpus
node compare.mjs --list corpus/specs.txt ../target/release/openapi-ts-rs
```

The corpus is openapi-typescript 6.7.0's examples and fixtures plus the 1,970 OpenAPI 3 specs in
APIs-guru/openapi-directory at a pinned commit (CI uses every 8th). Output is type-equal to 6.7.0
except where 6.7.0 is wrong; those specs are listed with the bug in `compat/known-differences.tsv`:

- percent-encoded `$ref` pointers are left encoded, so the lookup names a key that does not exist;
- unquoted YAML dates become `Date`s, so `enum: [2011-01-01]` becomes `"2011-01-01T00:00:00.000Z"`;
- string enum values under a non-string `type` are emitted unquoted;
- parameters referenced by array index (`…/parameters/2`) are dropped;
- backslashes in string literals are not escaped;
- non-pointer fragments (`file.yaml#Name`) are ignored.

Other 6.7.0 behavior, odd or not, is kept on purpose, for example duplicate discriminator
properties and `WithRequired` for `allOf` with `required`. Only local files are supported; remote
(`http`) refs and openapi-typescript's CLI flags are not.

## Layout

- `src/yaml.rs`: YAML to `serde_json::Value` from granit-parser events, with js-yaml 4's scalar rules.
- `src/load.rs`: reads the root spec and every file it references (in parallel) and decides how each
  external file is rendered, like 6.7.0's loader.
- `src/emit.rs`, `src/ts.rs`: the TypeScript output, one method per openapi-typescript transform.
