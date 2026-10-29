# Claudescript 개발 로드맵

구현: Rust (단일 바이너리 `cls`) · 실행 모델: 타입체크 → 바이트코드 컴파일 → 자체 스택 VM

| 단계 | 내용 | 산출물 |
|------|------|--------|
| M0 | 명세 확정 | `SPEC.md`, `ROADMAP.md` |
| M1 | 렉서·파서·AST (`.cls`, `.clsh`) | `lexer.rs` `parser.rs` `ast.rs` |
| M2 | 정적 타입체커, 제네릭 추론, 흐름 기반 좁히기, 모듈 해석, JSON Schema 생성 | `checker.rs` `types.rs` |
| M3 | 바이트코드 컴파일러 + VM (클로저, 예외, async, 실행 제한) | `compiler.rs` `vm.rs` `value.rs` |
| M4 | MCP 서버: tools/resources/prompts, stdio + Streamable HTTP, 입출력 검증, 권한 | `mcp.rs` `perms.rs` `schema.rs` |
| M5 | 표준 라이브러리, CLI(`new/check/build/run/serve/test/pack/inspect`), 테스트 러너, 배포 번들 | `natives.rs` `main.rs` `std/*.clsh` |
| M6 | 통합 테스트: 공식 MCP Python SDK 클라이언트(stdio·HTTP)로 호환성 검증 | `tests/` |

## 설계 결정
- 소스 `.cls`, 헤더 `.clsh`(타입·`declare` 선언 전용), 설정 `Claudescript.toml`.
- MCP 도구는 데코레이터(`@tool`, `@resource`, `@prompt`)가 붙은 **일반 함수**이며, 입력 스키마는 파라미터 타입에서, 출력 스키마는 반환 타입에서 컴파일 시 자동 생성된다.
- 권한(파일/네트워크/프로세스/환경변수)과 한계(시간·동시성·입출력 크기)는 런타임이 강제한다.
- 모든 전송 방식에서 VM은 단일 스레드(이벤트 루프형) 워커가 소유한다. HTTP는 I/O 스레드 → VM 워커 채널 구조.
- 배포: `cls build`(바이트코드 `.clsb`), `cls pack`(런타임 + 번들을 합친 단일 실행 파일).
