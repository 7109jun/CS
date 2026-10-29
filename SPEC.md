# Claudescript 1.0 언어 명세

## 1. 파일
- `*.cls` 소스, `*.clsh` 헤더(타입·인터페이스·`declare` 선언만 허용), `Claudescript.toml` 설정(`claudescript.toml`도 인식).

## 2. 예시
```ts
import { readFile } from "std/fs";

interface Todo { id: number; title: string; done?: boolean }

@tool({ description: "두 수를 더한다", params: { a: "첫째 수", b: "둘째 수" } })
export function add(a: number, b: number): number {
  return a + b;
}

@tool("파일 읽기")
async function read(path: string): Promise<string> {
  return readFile(path);
}

@resource("config://app", { mimeType: "application/json" })
function config(): string { return JSON.stringify({ ok: true }); }

@resource("notes://{name}")
function note(name: string): string { return `note ${name}`; }

@prompt("요약 프롬프트")
function summarize(topic: string, style?: string): string {
  return `Summarize ${topic} ${style ?? "briefly"}`;
}

test "add works" { assertEq(add(1, 2), 3); }
```

## 3. 문법 요약
- 선언: `let`, `const`, `function`, `async function`, `interface`, `type`, `import`, `export`, `declare`(헤더), `test "name" { }`.
- 제어: `if/else`, `while`, `do/while`, `for(;;)`, `for (x of xs)`, `switch`, `break/continue`, `return`, `throw`, `try/catch/finally`.
- 식: 산술·비교·논리·비트, `??`, `?.`, 삼항, 템플릿 문자열, 스프레드(`...`), 구조분해(객체/배열, 기본값, rest), 화살표 함수, `await`, `typeof`, `x as T`, `x!`, 복합 대입(`+= ??= ...`), `++/--`.
- 동등 비교는 `===`/`!==`만 허용(`==`, `!=`은 오류).
- 값: `number`(f64), `string`, `boolean`, `null`, 배열, 객체, 함수, `Promise`. `undefined`는 없고 `null`만 존재(누락 속성은 `null`).
- 클래스는 1.0 범위 밖: 인터페이스 + 객체 리터럴 + 클로저로 모델링한다.

## 4. 타입 시스템 (정적)
- 원시: `number string boolean null void unknown any never`, 리터럴 타입(`"a" | 1 | true`).
- 합성: `T[]`, `Array<T>`, `{ a: number; b?: string }`, `Record<string, T>`, 유니온 `A | B`, 함수 `(a: A) => R`, `Promise<T>`, 타입 별칭, 제네릭 인터페이스/별칭/함수와 `extends` 제약.
- 구조적 서브타이핑, 제네릭 추론(콜백 컨텍스트 타이핑 포함), 흐름 기반 좁히기(`=== null`, `typeof`, 참/거짓, 판별 유니온 `x.kind === "a"`, 조기 반환).
- `strict_types = true`: 암시적/명시적 `any`·미주석 매개변수 오류. `false`면 any로 완화.
- 경고(미사용 변수/임포트)는 `warnings_as_errors`로 오류화.

## 5. 모듈
- `import { a, b as c } from "./x.cls"`, `import * as ns from "./x.cls"`, `import type { T } from "./types.clsh"`, `import "..."`.
- 표준 모듈은 `std/…`(`std/fs`, `std/path`, `std/json`, `std/http`, `std/process`, `std/env`, `std/time`, `std/log`, `std/test`, `std/text`).
- 순환 임포트 금지. 모듈은 의존 순서대로 한 번만 실행. 값 임포트는 스냅샷 바인딩.

## 6. 비동기
- `async function`은 `Promise<T>`를 반환. `await`는 단일 스레드 협력형 런타임에서 즉시 정산(run-to-completion). `Promise.all/allSettled/race/resolve/reject`, `sleep(ms)` 제공.

## 7. 오류 처리
- `throw <any>`, `try/catch/finally`. `Error("msg")`는 `{ name, message }` 객체. 런타임 오류는 `TypeError`/`RangeError`/`PermissionError`. 한계 초과(시간)는 잡을 수 없는 치명 오류.

## 8. MCP 매핑
| 데코레이터 | MCP | 규칙 |
|-----------|-----|------|
| `@tool(desc \| {description,title,name,params,readOnly,destructive,idempotent,openWorld})` | tools/list, tools/call | 파라미터 타입 → `inputSchema`, 반환 타입이 객체면 `outputSchema` + `structuredContent` |
| `@resource(uri, {name,description,mimeType})` | resources/list·read, resources/templates/list | `{var}`이 있으면 템플릿, 함수 파라미터는 변수명(string) |
| `@prompt(desc \| {…})` | prompts/list·get | 파라미터는 string(옵셔널 가능), 반환 `string` 또는 `PromptMessage[]` |

- 반환 `ToolResult`면 그대로 전달(`content`, `isError`, `structuredContent`).
- 입력은 호출 전에 스키마 검증, 구조화 출력은 반환 후 검증. 실패는 `isError: true` 결과(입력 오류) 또는 내부 오류로 보고.
- 지원 프로토콜: `2025-11-25`(기본), `2025-06-18`, `2025-03-26`, `2024-11-05` 협상.
- 전송: `stdio`(줄 단위 JSON-RPC), Streamable HTTP(`POST /mcp`, JSON 응답, 세션 `Mcp-Session-Id`, `DELETE` 종료, Origin 검증, 기본 127.0.0.1 바인딩).

## 9. Claudescript.toml
```toml
[project]            # name, version, entry, description, instructions
[compiler]           # strict_types, warnings_as_errors
[mcp]                # protocol, transport = "stdio" | "http", host, port, path, allowed_origins
[permissions]        # filesystem = deny|project|project-readonly|allow
                     # network = deny|allowlisted|allow      (+ network_allow = [hosts])
                     # process = deny|allowlisted|allow      (+ process_allow = [commands])
                     # environment = deny|allowlisted|allow  (+ env_allow = [names])
[limits]             # max_execution_seconds, max_concurrent_requests, max_input_bytes, max_output_bytes, max_call_depth
```

## 10. 실행 모델
소스 → 렉서 → 파서(AST) → 타입체커(+스키마 생성) → 바이트코드 → 스택 VM(Lua식 업밸류 클로저, 핸들러 스택 예외). VM 한계: 실행 시간(데드라인), 호출 깊이, 입출력 바이트.

## 11. CLI
`cls new <dir>` · `cls check` · `cls build` · `cls run [file]` · `cls serve [--transport http --port N]` · `cls test` · `cls inspect` · `cls pack` · `cls version`.
