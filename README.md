# Claudescript

A statically typed language and runtime for building **MCP servers** for Claude Code (and any MCP client).
Source files `.cls`, header files `.clsh`, project config `Claudescript.toml`. Implemented in Rust:
lexer → parser → type checker (also derives JSON Schemas) → bytecode compiler → stack VM → MCP server.

```
cls new demo && cd demo
cls check            # type-check
cls test             # run test "name" { ... } blocks
cls inspect          # list tools / resources / prompts   (--json for schemas)
cls call greet '{"name":"Ada"}'
cls serve            # stdio (default) — or: cls serve --http --port 3000
cls build            # dist/<name>.clsb portable bytecode bundle
cls pack             # dist/<name> standalone executable (runtime + bytecode)
```

Register with Claude Code: `claude mcp add demo -- cls serve -p /path/to/demo`
(or the packed binary: `claude mcp add demo -- /path/to/dist/demo --root /path/to/demo`).

## Example

```ts
import * as fs from "std/fs";

interface Stats { count: number; mean: number }

@tool({ description: "Statistics for numbers", readOnly: true })
function stats(values: number[]): Stats {      // input schema derived from parameter types,
  if (values.length === 0) { throw Error("empty"); }   // output schema from the return type
  return { count: values.length, mean: values.reduce((a, x) => a + x, 0) / values.length };
}

@resource("project://file/{name}", { mimeType: "text/plain" })
async function file(name: string): Promise<string> { return await fs.readFile(`data/${name}`); }

@prompt({ description: "Review code", params: { code: "Code" } })
function review(code: string): string { return `Please review:\n${code}`; }

test "stats" { assertEq(stats([1, 3]).mean, 2); }
```

## Language

- Types: `number string boolean null unknown`, literal types, unions, tuples, arrays, `Record<string,T>`, interfaces, type aliases, generics with constraints, function types, `Promise<T>`. Flow narrowing (null checks, `typeof`, `===` literals, discriminated unions, truthiness), exhaustive `switch`.
- Strict by design: only `===`/`!==`; a single `null` (no `undefined`); no implicit coercions; `throw` takes Error-shaped values.
- Functions, closures, default/rest params, destructuring, spread, optional chaining `?.`, `??`, `!`, template strings, `for/for-of/while`, `try/catch/finally`.
- `async`/`await` (cooperative: async functions return settled promises; `Promise.all/race/allSettled`).
- Modules: `import {a} from "./x"`, `import * as ns from "std/fs"`, `export`. A `.clsh` header declares a module's public interface (`declare function`, `declare const`, interfaces); the sibling `.cls` is checked against it.
- Decorators: `@tool`, `@resource(uri, opts)`, `@prompt` on top-level functions. Tool results: string → text, object → `structuredContent` (+ validated against output schema), `ToolResult` passthrough.
- Standard library: prelude (`Math JSON Object Promise console` + array/string/number methods, `assert*`), `std/fs path http process env time mcp encoding crypto regex`.

## Configuration (`Claudescript.toml`)

```toml
[project]   name, version, entry, description, instructions
[compiler]  strict_types, warnings_as_errors
[mcp]       protocol = "2025-11-25" | "2025-06-18" | "2025-03-26" | "2024-11-05"
            transport = "stdio" | "http";  host = "127.0.0.1", port = 3000, path = "/mcp", allowed_origins = []
[permissions] filesystem  = "deny" | "project-readonly" | "project" | "allow"
              network     = "deny" | "allowlisted" | "allow"   (+ network_allow = ["api.example.com", "*.x.org"])
              process     = "deny" | "allowlisted" | "allow"   (+ process_allow = ["git"]; never a shell)
              environment = "deny" | "allowlisted" | "allow"   (+ env_allow = ["HOME"])
[limits]    max_execution_seconds, max_concurrent_requests, max_input_bytes, max_output_bytes, max_call_depth
```

Permissions are enforced by the runtime on every call (symlink-safe path checks, redirect re-checks for HTTP).
Inputs are validated against the generated schema before a tool runs; outputs are validated and size-limited.

## MCP support

JSON-RPC 2.0; `initialize` with version negotiation; `tools/list|call`, `resources/list|read|templates/list`,
`prompts/list|get`, `ping`, `logging/setLevel`; progress and log notifications (`std/mcp`: `progress`, `log`);
`notifications/cancelled`. Transports: **stdio** (newline-delimited; console output goes to stderr) and
**Streamable HTTP** (`POST /mcp`, `Mcp-Session-Id`, `DELETE`, Origin validation, 127.0.0.1 by default, SSE when a call emits notifications).

## Testing

`tests/run_all.sh` runs unit tests, the language suite, header/permission/limit/cancellation tests and
the official MCP Python SDK (both 1.x and 2.x verified) against stdio and Streamable HTTP servers.
