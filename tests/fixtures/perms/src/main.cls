import * as fs from "std/fs";
import * as http from "std/http";
import * as proc from "std/process";
import * as env from "std/env";

@tool("read a file") async function rd(path: string): Promise<string> { return await fs.readFile(path); }
@tool("write a file") async function wr(path: string): Promise<string> { await fs.writeFile(path, "x"); return "ok"; }
@tool("fetch") async function get(url: string): Promise<string> { return await http.getText(url); }
@tool("exec") async function run(cmd: string): Promise<string> { const r = await proc.exec(cmd, ["hi"]); return r.stdout.trim(); }
@tool("env") function getenv(name: string): string | null { return env.get(name); }
@tool("big") function big(n: number): string { return "x".repeat(n); }
@tool("deep") function deep(n: number): number { return n === 0 ? 0 : 1 + deep(n - 1); }
